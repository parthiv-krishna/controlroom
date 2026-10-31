//! The write path: log, fsync, then write to the child's stdin.
//!
//! Two threads, so that a child that stops reading its stdin never delays logging,
//! fsyncing or barriers:
//!
//! - the **logger** takes [`LogOp`]s in arrival order, appends an `in` record per
//!   write, fsyncs once per batch, replies, and only then hands the lines on;
//! - the **stdin writer** owns the child's stdin (keeping it open while no client is
//!   attached) and writes the lines to it.
//!
//! A barrier queued behind writes is answered after those writes are logged and
//! fsynced, which is what a daemon in doubt (just after a restart) waits for.

use crate::diag::diag;
use std::io::Write;
use std::process::ChildStdin;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender};

use tokio::sync::{mpsc, oneshot};

use super::log::{AppendError, Log};
use crate::proto::{ErrorBody, ErrorCode};

/// Most operations handled per fsync.
const BATCH: usize = 64;

/// Replies with the seq of the `in` record (write) or the durable seq (barrier).
pub(crate) type Reply = oneshot::Sender<Result<u64, ErrorBody>>;

/// An operation for the logger thread.
pub(crate) enum LogOp {
    /// Log, fsync, then write `line` to the child's stdin.
    Write { line: String, reply: Reply },
    /// Reply once every earlier write is logged and fsynced.
    Barrier { reply: Reply },
}

/// The logger thread; ends when every [`LogOp`] sender is gone.
pub(crate) fn logger(
    log: Arc<Log>,
    mut ops: mpsc::Receiver<LogOp>,
    stdin: Option<SyncSender<Vec<u8>>>,
) {
    let mut batch = Vec::with_capacity(BATCH);
    while let Some(first) = ops.blocking_recv() {
        batch.push(first);
        while batch.len() < BATCH {
            match ops.try_recv() {
                Ok(op) => batch.push(op),
                Err(_) => break,
            }
        }
        handle_batch(&log, batch.drain(..), stdin.as_ref());
    }
}

fn handle_batch(
    log: &Log,
    batch: impl Iterator<Item = LogOp>,
    stdin: Option<&SyncSender<Vec<u8>>>,
) {
    let mut writes = Vec::new();
    let mut barriers = Vec::new();
    for op in batch {
        match op {
            LogOp::Write { line, reply } => match log.append_in(line.clone()) {
                Ok(record) => writes.push((record.seq, line, reply)),
                Err(e) => {
                    let _ = reply.send(Err(append_error(e)));
                }
            },
            LogOp::Barrier { reply } => barriers.push(reply),
        }
    }
    match log.sync() {
        Ok(durable) => {
            for (seq, line, reply) in writes {
                let mut bytes = line.into_bytes();
                bytes.push(b'\n');
                // A closed channel means the child's stdin is gone; the line is
                // logged but undeliverable, and the exit record follows.
                if let Some(stdin) = stdin {
                    let _ = stdin.send(bytes);
                }
                let _ = reply.send(Ok(seq));
            }
            for reply in barriers {
                let _ = reply.send(Ok(durable));
            }
        }
        Err(e) => {
            // Logged but not durable: don't let the child see it.
            let error = ErrorBody::new(ErrorCode::Io, format!("fsync: {e}"));
            for reply in writes
                .into_iter()
                .map(|(_, _, reply)| reply)
                .chain(barriers)
            {
                let _ = reply.send(Err(error.clone()));
            }
        }
    }
}

fn append_error(e: AppendError) -> ErrorBody {
    match e {
        AppendError::Closed | AppendError::ChildExited => {
            ErrorBody::new(ErrorCode::ChildExited, "child has exited")
        }
        AppendError::Io(e) => ErrorBody::new(ErrorCode::Io, format!("transcript: {e}")),
    }
}

/// The stdin writer thread; ends when the logger is gone.
pub(crate) fn stdin_writer(stdin: ChildStdin, lines: Receiver<Vec<u8>>) {
    let mut stdin = Some(stdin);
    for line in lines {
        let Some(pipe) = stdin.as_mut() else {
            continue;
        };
        if let Err(e) = pipe.write_all(&line).and_then(|()| pipe.flush()) {
            diag!("writing child stdin: {e}");
            stdin = None;
        }
    }
}
