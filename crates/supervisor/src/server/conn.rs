//! Socket connections: requests, responses and record streams.

use crate::diag::diag;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use controlroom_transcript::{ReadError, Reader, Record, StdinMode, StopCause};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::stdin::LogOp;
use super::{DEFAULT_GRACE, READER_DRAIN_TIMEOUT, Shared, until_shutdown};
use crate::proto::{
    Done, DurableResult, ErrorBody, ErrorCode, Hello, HelloParams, Message, PROTOCOL_VERSION,
    ReadParams, ReadResult, Request, ResizeParams, Status, TerminateParams, TerminateResult,
    VERSION, WriteBytesParams, WriteParams, WriteResult,
};

/// Longest request line accepted; a `write` line is further capped by the spec.
const MAX_REQUEST_BYTES: u64 = controlroom_transcript::MAX_RECORD_BYTES as u64;
/// Outgoing messages buffered per connection.
const OUT_QUEUE: usize = 256;
/// Records read from the transcript per blocking-pool hop.
const READ_BATCH: usize = 256;

pub(super) async fn accept_loop(
    shared: Arc<Shared>,
    listener: UnixListener,
    done_tx: mpsc::Sender<()>,
) {
    let mut shutdown = shared.shutdown.subscribe();
    let mut next_conn_id = 0;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    next_conn_id += 1;
                    let conn = Conn::new(shared.clone(), next_conn_id);
                    tokio::spawn(conn.serve(stream, done_tx.clone()));
                }
                Err(e) => {
                    diag!("accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            () = until_shutdown(&mut shutdown) => return,
        }
    }
}

/// Per-connection state.
struct Conn {
    id: u64,
    shared: Arc<Shared>,
    out: mpsc::Sender<String>,
    out_rx: Option<mpsc::Receiver<String>>,
    hello: bool,
    is_writer: bool,
    stream: Option<JoinHandle<()>>,
}

/// A request line that couldn't be read.
enum LineError {
    TooLong,
    Closed,
}

impl Conn {
    fn new(shared: Arc<Shared>, id: u64) -> Self {
        let (out, out_rx) = mpsc::channel(OUT_QUEUE);
        Self {
            id,
            shared,
            out,
            out_rx: Some(out_rx),
            hello: false,
            is_writer: false,
            stream: None,
        }
    }

    /// Serves requests until the client closes the connection or shutdown. The
    /// writer role is released only after every request read so far was handled,
    /// so a new writer's barrier covers all of this connection's writes.
    async fn serve(mut self, stream: UnixStream, _done: mpsc::Sender<()>) {
        let (rd, wr) = stream.into_split();
        let Some(mut out_rx) = self.out_rx.take() else {
            return;
        };
        let sender = tokio::spawn(async move {
            let mut wr = BufWriter::new(wr);
            while let Some(line) = out_rx.recv().await {
                if wr.write_all(line.as_bytes()).await.is_err()
                    || wr.write_all(b"\n").await.is_err()
                {
                    return;
                }
                if out_rx.is_empty() && wr.flush().await.is_err() {
                    return;
                }
            }
            let _ = wr.flush().await;
            let _ = wr.shutdown().await;
        });

        let mut rd = BufReader::new(rd);
        let mut buf = Vec::new();
        let mut shutdown = self.shared.shutdown.subscribe();
        let client_closed = loop {
            tokio::select! {
                line = read_line(&mut rd, &mut buf) => match line {
                    Ok(()) => self.handle(&buf).await,
                    Err(LineError::TooLong) => {
                        let error = ErrorBody::new(ErrorCode::InvalidParams, "request line too long");
                        self.send(message(&Value::Null, Err(error))).await;
                        break true;
                    }
                    Err(LineError::Closed) => break true,
                },
                () = until_shutdown(&mut shutdown) => break false,
            }
        };

        if let Some(stream) = self.stream.take() {
            if client_closed {
                stream.abort();
            } else {
                // Shutting down: the stream ends after the exit record.
                let _ = tokio::time::timeout(READER_DRAIN_TIMEOUT, stream).await;
            }
        }
        if self.is_writer {
            *self
                .shared
                .writer_conn
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
        drop(self);
        let _ = tokio::time::timeout(READER_DRAIN_TIMEOUT, sender).await;
    }

    async fn send(&self, line: String) {
        let _ = self.out.send(line).await;
    }

    async fn handle(&mut self, line: &[u8]) {
        let request = match serde_json::from_slice::<Request>(line) {
            Ok(request) => request,
            Err(e) => {
                let error = ErrorBody::new(ErrorCode::ParseError, e.to_string());
                self.send(message(&Value::Null, Err(error))).await;
                return;
            }
        };
        let id = request.id.clone();
        match self.dispatch(request).await {
            Ok(Some(result)) => self.send(message(&id, Ok(result))).await,
            Ok(None) => {}
            Err(error) => self.send(message(&id, Err(error))).await,
        }
    }

    /// Returns the result to send now, or `None` if a task answers later.
    async fn dispatch(&mut self, request: Request) -> Result<Option<Value>, ErrorBody> {
        if request.method != "hello" && !self.hello {
            return Err(ErrorBody::new(ErrorCode::HelloRequired, "send hello first"));
        }
        match request.method.as_str() {
            "hello" => self.hello(params(request.params)?).map(Some),
            "status" => Ok(Some(to_value(self.status()))),
            "read" => {
                self.read(request.id, params(request.params)?).await?;
                Ok(None)
            }
            "write" => {
                self.require_writer()?;
                self.write(request.id, params(request.params)?).await?;
                Ok(None)
            }
            "write_bytes" => {
                self.write_bytes(request.id, params(request.params)?)
                    .await?;
                Ok(None)
            }
            "resize" => {
                let params: ResizeParams = params(request.params)?;
                let pty = self.pty()?;
                super::pty::resize(&pty.master, params.cols, params.rows)
                    .map_err(|e| ErrorBody::new(ErrorCode::Io, format!("resizing: {e}")))?;
                Ok(Some(to_value(Done {})))
            }
            "barrier" => {
                let (reply, rx) = oneshot::channel();
                self.enqueue(LogOp::Barrier { reply }).await?;
                self.reply_later(request.id, rx, |durable_seq| DurableResult { durable_seq });
                Ok(None)
            }
            "sync" => {
                let (reply, rx) = oneshot::channel();
                let log = self.shared.log.clone();
                tokio::task::spawn_blocking(move || {
                    let result = log
                        .sync()
                        .map_err(|e| ErrorBody::new(ErrorCode::Io, format!("fsync: {e}")));
                    let _ = reply.send(result);
                });
                self.reply_later(request.id, rx, |durable_seq| DurableResult { durable_seq });
                Ok(None)
            }
            "terminate" => {
                self.require_writer()?;
                let params: TerminateParams = params(request.params)?;
                let already_exited = self.shared.log.child_reaped();
                if !already_exited {
                    let grace = params.grace_ms.map_or(DEFAULT_GRACE, Duration::from_millis);
                    self.shared.terminate(grace, StopCause::Terminate);
                }
                Ok(Some(to_value(TerminateResult { already_exited })))
            }
            other => Err(ErrorBody::new(
                ErrorCode::UnknownMethod,
                format!("unknown method {other:?}"),
            )),
        }
    }

    fn hello(&mut self, params: HelloParams) -> Result<Value, ErrorBody> {
        if params.protocol_version != PROTOCOL_VERSION {
            // The client adapts (it speaks N and N-1); just note it.
            diag!(
                "client speaks protocol {}, answering with {PROTOCOL_VERSION}",
                params.protocol_version
            );
        }
        if params.writer && !self.is_writer {
            let mut holder = self
                .shared
                .writer_conn
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if holder.is_some() {
                return Err(ErrorBody::new(
                    ErrorCode::WriterBusy,
                    "another connection is the writer",
                ));
            }
            *holder = Some(self.id);
            self.is_writer = true;
        }
        self.hello = true;
        let info = &self.shared.info;
        Ok(to_value(Hello {
            protocol_version: PROTOCOL_VERSION,
            version: VERSION.to_owned(),
            id: info.id.clone(),
            kind: info.kind,
            pid: info.pid,
            child_pid: info.child_pid,
            transcript: info.transcript.clone(),
            writer: self.is_writer,
        }))
    }

    fn status(&self) -> Status {
        let shared = &self.shared;
        let info = &shared.info;
        Status {
            id: info.id.clone(),
            kind: info.kind,
            pid: info.pid,
            child_pid: info.child_pid,
            alive: !shared.log.child_reaped(),
            exit: shared.exited(),
            last_seq: shared.log.last_seq(),
            durable_seq: shared.log.durable_seq(),
            writer_attached: shared
                .writer_conn
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some(),
            protocol_version: PROTOCOL_VERSION,
            version: VERSION.to_owned(),
        }
    }

    /// Answers `{last_seq}`, then streams records from `from_seq` (replacing any
    /// earlier stream on this connection).
    async fn read(&mut self, id: Value, params: ReadParams) -> Result<(), ErrorBody> {
        if let Some(old) = self.stream.take() {
            old.abort();
        }
        // Subscribe before opening, so no append is missed in between.
        let seq_rx = self.shared.log.subscribe();
        let path = self.shared.info.transcript.clone();
        let reader = tokio::task::spawn_blocking(move || Reader::open(&path, params.from_seq))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Io, e.to_string()))?
            .map_err(|e| ErrorBody::new(ErrorCode::Io, e.to_string()))?;
        let last_seq = *seq_rx.borrow();
        self.send(message(&id, Ok(to_value(ReadResult { last_seq }))))
            .await;
        self.stream = Some(tokio::spawn(stream_records(
            self.shared.clone(),
            reader,
            seq_rx,
            id,
            self.out.clone(),
        )));
        Ok(())
    }

    async fn write(&mut self, id: Value, params: WriteParams) -> Result<(), ErrorBody> {
        if self.shared.pty.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::NoStdin,
                "a terminal takes write_bytes",
            ));
        }
        if self.shared.stdin != StdinMode::Pipe {
            return Err(ErrorBody::new(
                ErrorCode::NoStdin,
                "the child's stdin is not a pipe",
            ));
        }
        if params.line.contains('\n') {
            return Err(ErrorBody::new(
                ErrorCode::InvalidParams,
                "line must not contain a newline",
            ));
        }
        if params.line.len() as u64 > self.shared.max_line_bytes {
            return Err(ErrorBody::new(
                ErrorCode::LineTooLong,
                format!(
                    "line of {} bytes exceeds max_line_bytes {}",
                    params.line.len(),
                    self.shared.max_line_bytes
                ),
            ));
        }
        if self.shared.log.child_reaped() {
            return Err(ErrorBody::new(ErrorCode::ChildExited, "child has exited"));
        }
        let (reply, rx) = oneshot::channel();
        self.enqueue(LogOp::Write {
            line: params.line,
            reply,
        })
        .await?;
        self.reply_later(id, rx, |seq| WriteResult { seq });
        Ok(())
    }

    /// Queues raw input for a terminal (any connection may type); answers once it is written.
    /// Not logged: what a user types may be a password.
    async fn write_bytes(&self, id: Value, params: WriteBytesParams) -> Result<(), ErrorBody> {
        let bytes = STANDARD
            .decode(params.data)
            .map_err(|e| ErrorBody::new(ErrorCode::InvalidParams, format!("data: {e}")))?;
        let input = self.pty()?.input.clone();
        if self.shared.log.child_reaped() {
            return Err(ErrorBody::new(ErrorCode::ChildExited, "child has exited"));
        }
        let (reply, rx) = oneshot::channel();
        let out = self.out.clone();
        // The writer thread may block on a full terminal; never on this connection's loop.
        tokio::task::spawn_blocking(move || {
            if let Err(std::sync::mpsc::SendError((_, reply))) = input.send((bytes, reply)) {
                let _ = reply.send(Err(ErrorBody::new(
                    ErrorCode::ChildExited,
                    "the terminal is closed",
                )));
            }
        });
        tokio::spawn(async move {
            let result = match rx.await {
                Ok(Ok(())) => Ok(to_value(Done {})),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(ErrorBody::new(
                    ErrorCode::ChildExited,
                    "the terminal is closed",
                )),
            };
            let _ = out.send(message(&id, result)).await;
        });
        Ok(())
    }

    /// The terminal, or `invalid_params` for a child on pipes.
    fn pty(&self) -> Result<&super::PtyHandle, ErrorBody> {
        self.shared
            .pty
            .as_ref()
            .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidParams, "the child has no terminal"))
    }

    /// Queues an operation for the logger, in request order.
    async fn enqueue(&self, op: LogOp) -> Result<(), ErrorBody> {
        self.shared
            .ops
            .send(op)
            .await
            .map_err(|_| ErrorBody::new(ErrorCode::ChildExited, "the write path is closed"))
    }

    /// Sends the answer once `rx` resolves, without blocking this connection.
    fn reply_later<T: Serialize>(
        &self,
        id: Value,
        rx: oneshot::Receiver<Result<u64, ErrorBody>>,
        wrap: impl FnOnce(u64) -> T + Send + 'static,
    ) {
        let out = self.out.clone();
        tokio::spawn(async move {
            let result = match rx.await {
                Ok(Ok(n)) => Ok(to_value(wrap(n))),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(ErrorBody::new(
                    ErrorCode::ChildExited,
                    "the write path is closed",
                )),
            };
            let _ = out.send(message(&id, result)).await;
        });
    }

    fn require_writer(&self) -> Result<(), ErrorBody> {
        if self.is_writer {
            Ok(())
        } else {
            Err(ErrorBody::new(
                ErrorCode::NotWriter,
                "this connection is not the writer (hello with writer: true)",
            ))
        }
    }
}

/// Reads one `\n`-terminated line into `buf` (without the `\n`).
async fn read_line(rd: &mut BufReader<OwnedReadHalf>, buf: &mut Vec<u8>) -> Result<(), LineError> {
    buf.clear();
    let mut limited = rd.take(MAX_REQUEST_BYTES + 1);
    match limited.read_until(b'\n', buf).await {
        Ok(0) | Err(_) => Err(LineError::Closed),
        Ok(_) if buf.last() == Some(&b'\n') => {
            buf.pop();
            Ok(())
        }
        Ok(_) if buf.len() as u64 > MAX_REQUEST_BYTES => Err(LineError::TooLong),
        // EOF in the middle of a line.
        Ok(_) => Err(LineError::Closed),
    }
}

/// Decodes params; missing params count as `{}`.
fn params<T: DeserializeOwned>(params: Value) -> Result<T, ErrorBody> {
    let params = if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    };
    serde_json::from_value(params)
        .map_err(|e| ErrorBody::new(ErrorCode::InvalidParams, e.to_string()))
}

fn to_value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn message(id: &Value, result: Result<Value, ErrorBody>) -> String {
    let (result, error) = match result {
        Ok(result) => (Some(result), None),
        Err(error) => (None, Some(error)),
    };
    let msg = Message {
        id: id.clone(),
        result,
        error,
        record: None,
    };
    serde_json::to_string(&msg).unwrap_or_default()
}

fn record_message(id: &Value, record: Record) -> String {
    let msg = Message {
        id: id.clone(),
        result: None,
        error: None,
        record: Some(record),
    };
    serde_json::to_string(&msg).unwrap_or_default()
}

/// Streams records from the transcript, then follows appends until this run's exit
/// record has been sent. Reads happen on the blocking pool; a slow client only
/// falls behind (the bounded out queue backpressures), it never loses records.
async fn stream_records(
    shared: Arc<Shared>,
    mut reader: Reader,
    mut seq_rx: watch::Receiver<u64>,
    id: Value,
    out: mpsc::Sender<String>,
) {
    loop {
        seq_rx.borrow_and_update();
        // Load before reading: if set, the exit record is already in the file.
        let final_seq = shared.final_seq.load(Ordering::SeqCst);
        loop {
            let read = tokio::task::spawn_blocking(move || {
                let batch = read_batch(&mut reader);
                (reader, batch)
            })
            .await;
            let Ok((returned, batch)) = read else {
                return;
            };
            reader = returned;
            let Ok(records) = batch else {
                return;
            };
            let at_end = records.len() < READ_BATCH;
            for record in records {
                if out.send(record_message(&id, record)).await.is_err() {
                    return;
                }
            }
            if at_end {
                break;
            }
        }
        if final_seq != 0 || seq_rx.changed().await.is_err() {
            return;
        }
    }
}

/// Up to [`READ_BATCH`] records; corrupt lines are logged and skipped.
fn read_batch(reader: &mut Reader) -> Result<Vec<Record>, ()> {
    let mut records = Vec::new();
    while records.len() < READ_BATCH {
        match reader.next_record() {
            Ok(Some(record)) => records.push(record),
            Ok(None) => break,
            Err(ReadError::Io(e)) => {
                diag!("reading transcript: {e}");
                return Err(());
            }
            Err(e) => diag!("skipping transcript line: {e}"),
        }
    }
    Ok(records)
}
