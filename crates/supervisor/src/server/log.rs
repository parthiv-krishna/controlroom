//! The supervisor's transcript handle, shared by pumps, the stdin logger and the
//! socket server.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use controlroom_transcript::{Body, Line, Record, Syncer, Writer};
use tokio::sync::watch;

/// Why an append was refused.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AppendError {
    /// The exit record is written; the transcript is closed for this run.
    #[error("transcript closed")]
    Closed,
    /// The child was reaped; no more `in` records.
    #[error("child has exited")]
    ChildExited,
    #[error(transparent)]
    Io(#[from] io::Error),
}

struct State {
    writer: Writer,
    child_reaped: bool,
    closed: bool,
}

/// The transcript writer plus the published `last_seq` and `durable_seq`.
///
/// Appends are short page-cache writes under a mutex and happen on plain threads
/// (pumps, the stdin logger) or briefly on the async runtime. Fsyncs go through a
/// separate [`Syncer`] without the mutex, so appends continue during an fsync, and
/// always run on a blocking thread.
pub(crate) struct Log {
    state: Mutex<State>,
    syncer: Syncer,
    last_seq: watch::Sender<u64>,
    durable_seq: watch::Sender<u64>,
}

impl Log {
    pub(crate) fn new(writer: Writer) -> io::Result<Self> {
        let last = writer.last_seq();
        Ok(Self {
            syncer: writer.syncer()?,
            state: Mutex::new(State {
                writer,
                child_reaped: false,
                closed: false,
            }),
            last_seq: watch::Sender::new(last),
            durable_seq: watch::Sender::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn append_locked(&self, state: &mut State, body: Body) -> Result<Record, AppendError> {
        if state.closed {
            return Err(AppendError::Closed);
        }
        let record = state.writer.append(body)?;
        self.last_seq.send_replace(record.seq);
        Ok(record)
    }

    /// Appends a record.
    pub(crate) fn append(&self, body: Body) -> Result<Record, AppendError> {
        self.append_locked(&mut self.lock(), body)
    }

    /// Appends an `in` record, unless the child has been reaped.
    pub(crate) fn append_in(&self, line: String) -> Result<Record, AppendError> {
        let mut state = self.lock();
        if state.child_reaped {
            return Err(AppendError::ChildExited);
        }
        self.append_locked(&mut state, Body::In(Line::new(line)))
    }

    /// Appends the final record of this run and closes the log.
    pub(crate) fn append_final(&self, body: Body) -> Result<Record, AppendError> {
        let mut state = self.lock();
        let record = self.append_locked(&mut state, body)?;
        state.closed = true;
        Ok(record)
    }

    /// Marks the child as reaped. Taking the lock orders this against
    /// [`append_in`](Self::append_in): no `in` record follows it.
    pub(crate) fn mark_child_reaped(&self) {
        self.lock().child_reaped = true;
    }

    pub(crate) fn child_reaped(&self) -> bool {
        self.lock().child_reaped
    }

    /// Fsyncs everything appended so far and returns the new durable seq.
    /// Blocking: call from a plain thread or `spawn_blocking`.
    pub(crate) fn sync(&self) -> io::Result<u64> {
        let seq = *self.last_seq.borrow();
        if seq <= *self.durable_seq.borrow() {
            return Ok(*self.durable_seq.borrow());
        }
        self.syncer.sync()?;
        self.durable_seq.send_if_modified(|durable| {
            let advanced = seq > *durable;
            *durable = (*durable).max(seq);
            advanced
        });
        Ok(*self.durable_seq.borrow())
    }

    pub(crate) fn last_seq(&self) -> u64 {
        *self.last_seq.borrow()
    }

    pub(crate) fn durable_seq(&self) -> u64 {
        *self.durable_seq.borrow()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.last_seq.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_rules() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = Writer::open(&dir.path().join("t.jsonl")).unwrap();
        let log = Log::new(writer).unwrap();
        assert_eq!(log.append_in("a".into()).unwrap().seq, 1);
        assert_eq!((log.last_seq(), log.durable_seq()), (1, 0));
        assert_eq!(log.sync().unwrap(), 1);
        assert_eq!(log.durable_seq(), 1);

        log.mark_child_reaped();
        assert!(matches!(
            log.append_in("b".into()),
            Err(AppendError::ChildExited)
        ));
        log.append(Body::Out(Line::new("late output"))).unwrap();
        log.append_final(Body::Out(Line::new("last"))).unwrap();
        assert!(matches!(
            log.append(Body::Out(Line::new("x"))),
            Err(AppendError::Closed)
        ));
        assert_eq!(log.sync().unwrap(), 3);
    }
}
