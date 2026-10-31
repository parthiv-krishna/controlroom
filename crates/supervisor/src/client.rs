//! The daemon's side of the supervisor socket.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use controlroom_transcript::Record;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};

use crate::proto::{
    DurableResult, ErrorBody, Hello, HelloParams, MIN_PROTOCOL_VERSION, Message, PROTOCOL_VERSION,
    ReadParams, ReadResult, Request, Status, TerminateParams, TerminateResult, WriteParams,
    WriteResult,
};

/// Default per-call timeout.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Records buffered per stream before the connection is backpressured.
const RECORD_QUEUE: usize = 1024;
/// Requests buffered before callers wait.
const REQUEST_QUEUE: usize = 256;

/// Errors from a supervisor connection.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Connecting or writing failed.
    #[error("supervisor socket: {0}")]
    Io(#[from] std::io::Error),
    /// No response within the call timeout.
    #[error("supervisor call timed out")]
    Timeout,
    /// The connection closed before the response arrived.
    #[error("supervisor connection closed")]
    Disconnected,
    /// The supervisor speaks a protocol version this client doesn't.
    #[error(
        "supervisor speaks protocol {0}; this client supports \
         {MIN_PROTOCOL_VERSION}..={PROTOCOL_VERSION}"
    )]
    Incompatible(u32),
    /// The supervisor answered with an error.
    #[error("supervisor error {0}")]
    Remote(#[from] ErrorBody),
    /// The supervisor sent something unexpected.
    #[error("invalid supervisor response: {0}")]
    Protocol(String),
}

type Reply = oneshot::Sender<Result<Value, ClientError>>;

/// Callers waiting for responses, by request id; `None` once the connection closed,
/// so later calls fail at once instead of timing out.
type Pending = Arc<Mutex<Option<HashMap<u64, Reply>>>>;

/// The current record stream: the id of its `read` request and where records go.
type StreamSlot = Arc<Mutex<Option<(u64, mpsc::Sender<Record>)>>>;

/// A connection to one supervisor.
///
/// Calls may be issued concurrently (`&self`); they are sent in call order, and the
/// supervisor applies writes in the order it receives them. Each call has a timeout
/// (default [`DEFAULT_CALL_TIMEOUT`]). Records of a [`read`](Client::read) stream
/// share the connection with responses, so a stream must be drained, or use a
/// separate connection for reading.
pub struct Client {
    requests: mpsc::Sender<String>,
    pending: Pending,
    stream: StreamSlot,
    next_id: AtomicU64,
    hello: Hello,
    timeout: Duration,
}

/// Records from a [`Client::read`] stream.
pub struct RecordStream {
    records: mpsc::Receiver<Record>,
    last_seq: u64,
}

impl RecordStream {
    /// The next record; `None` once the stream ended (after the exit record, when
    /// the connection closes, or when another `read` replaced it).
    pub async fn next(&mut self) -> Option<Record> {
        self.records.recv().await
    }

    /// The transcript's last seq when the stream started.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }
}

impl Client {
    /// Connects to `socket` and says hello, claiming the writer role if `writer`.
    ///
    /// Fails with [`ClientError::Incompatible`] if the supervisor's protocol version
    /// is outside N-1..=N, and with a [`ErrorCode::WriterBusy`] remote error if
    /// another connection is the writer.
    ///
    /// [`ErrorCode::WriterBusy`]: crate::ErrorCode::WriterBusy
    pub async fn connect(socket: &Path, writer: bool) -> Result<Self, ClientError> {
        let stream = tokio::time::timeout(DEFAULT_CALL_TIMEOUT, UnixStream::connect(socket))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let (rd, wr) = stream.into_split();
        let (requests, requests_rx) = mpsc::channel(REQUEST_QUEUE);
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let stream = StreamSlot::default();
        tokio::spawn(send_requests(wr, requests_rx));
        tokio::spawn(receive(rd, pending.clone(), stream.clone()));
        let mut client = Self {
            requests,
            pending,
            stream,
            next_id: AtomicU64::new(1),
            hello: Hello::default(),
            timeout: DEFAULT_CALL_TIMEOUT,
        };
        let hello: Hello = client
            .call(
                "hello",
                HelloParams {
                    protocol_version: PROTOCOL_VERSION,
                    writer,
                },
            )
            .await?;
        if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&hello.protocol_version) {
            return Err(ClientError::Incompatible(hello.protocol_version));
        }
        client.hello = hello;
        Ok(client)
    }

    /// The `hello` result: ids, pids, versions and whether this is the writer.
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// Sets the per-call timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Streams every record with `seq >= from_seq`, then live records, until the
    /// exit record. Replaces an earlier stream on this connection.
    pub async fn read(&self, from_seq: u64) -> Result<RecordStream, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, records) = mpsc::channel(RECORD_QUEUE);
        // Register before sending: records follow the response immediately.
        *lock(&self.stream) = Some((id, tx));
        let result: ReadResult = self
            .call_with_id(id, "read", ReadParams { from_seq })
            .await?;
        Ok(RecordStream {
            records,
            last_seq: result.last_seq,
        })
    }

    /// Logs `line` as an `in` record, fsyncs, and queues it for the child's stdin.
    /// Returns its seq once it is durable. Writer only.
    pub async fn write(&self, line: &str) -> Result<u64, ClientError> {
        let params = WriteParams {
            line: line.to_owned(),
        };
        Ok(self.call::<WriteResult>("write", params).await?.seq)
    }

    /// Waits until every write the supervisor accepted before this call (from any
    /// connection) is logged and fsynced; returns the durable seq. After taking
    /// over the writer role, this tells a daemon in doubt that the transcript holds
    /// everything the previous writer sent.
    pub async fn barrier(&self) -> Result<u64, ClientError> {
        Ok(self
            .call::<DurableResult>("barrier", Value::Null)
            .await?
            .durable_seq)
    }

    /// Fsyncs the transcript; returns the durable seq.
    pub async fn sync(&self) -> Result<u64, ClientError> {
        Ok(self
            .call::<DurableResult>("sync", Value::Null)
            .await?
            .durable_seq)
    }

    /// The supervisor's status.
    pub async fn status(&self) -> Result<Status, ClientError> {
        self.call("status", Value::Null).await
    }

    /// SIGTERM to the child's process group, SIGKILL after `grace` (default 5 s).
    /// Returns whether the child had already exited. Writer only.
    pub async fn terminate(&self, grace: Option<Duration>) -> Result<bool, ClientError> {
        let params = TerminateParams {
            grace_ms: grace.map(|g| u64::try_from(g.as_millis()).unwrap_or(u64::MAX)),
        };
        Ok(self
            .call::<TerminateResult>("terminate", params)
            .await?
            .already_exited)
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<T, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.call_with_id(id, method, params).await
    }

    async fn call_with_id<T: DeserializeOwned>(
        &self,
        id: u64,
        method: &str,
        params: impl Serialize,
    ) -> Result<T, ClientError> {
        let params =
            serde_json::to_value(params).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let request = Request {
            id: id.into(),
            method: method.to_owned(),
            params,
        };
        let line =
            serde_json::to_string(&request).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        match lock(&self.pending).as_mut() {
            Some(pending) => pending.insert(id, tx),
            None => return Err(ClientError::Disconnected),
        };
        let result = tokio::time::timeout(self.timeout, async {
            self.requests
                .send(line)
                .await
                .map_err(|_| ClientError::Disconnected)?;
            rx.await.map_err(|_| ClientError::Disconnected)?
        })
        .await;
        if let Some(pending) = lock(&self.pending).as_mut() {
            pending.remove(&id);
        }
        let value = result.map_err(|_| ClientError::Timeout)??;
        serde_json::from_value(value).map_err(|e| ClientError::Protocol(e.to_string()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes request lines; closing the write half when the client is dropped.
async fn send_requests(mut wr: OwnedWriteHalf, mut requests: mpsc::Receiver<String>) {
    while let Some(line) = requests.recv().await {
        if wr.write_all(line.as_bytes()).await.is_err() || wr.write_all(b"\n").await.is_err() {
            return;
        }
    }
    let _ = wr.shutdown().await;
}

/// Routes responses to their callers and records to the current stream.
async fn receive(rd: OwnedReadHalf, pending: Pending, stream: StreamSlot) {
    let mut lines = BufReader::new(rd).lines();
    let mut protocol_error = None;
    while let Ok(Some(line)) = lines.next_line().await {
        let msg: Message = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                protocol_error = Some(e.to_string());
                break;
            }
        };
        let Some(id) = msg.id.as_u64() else {
            // An error for an unparseable request; nobody to tell.
            continue;
        };
        if let Some(record) = msg.record {
            let tx = lock(&stream)
                .as_ref()
                .filter(|(stream_id, _)| *stream_id == id)
                .map(|(_, tx)| tx.clone());
            if let Some(tx) = tx {
                // Backpressure: wait for the consumer. A dropped stream is fine.
                let _ = tx.send(record).await;
            }
            continue;
        }
        let Some(reply) = lock(&pending).as_mut().and_then(|p| p.remove(&id)) else {
            continue;
        };
        let result = match (msg.result, msg.error) {
            (_, Some(error)) => Err(ClientError::Remote(error)),
            (Some(result), None) => Ok(result),
            (None, None) => Err(ClientError::Protocol("response without result".into())),
        };
        let _ = reply.send(result);
    }
    let waiting = lock(&pending).take().unwrap_or_default();
    for (_, reply) in waiting {
        let error = match &protocol_error {
            Some(e) => ClientError::Protocol(e.clone()),
            None => ClientError::Disconnected,
        };
        let _ = reply.send(Err(error));
    }
    lock(&stream).take();
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;

    use super::*;

    /// A fake supervisor that answers `hello` with `version`, then echoes ids.
    async fn fake_supervisor(listener: UnixListener, version: u32) {
        let (stream, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut lines = BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str() {
                Some("hello") => serde_json::json!({
                    "protocol_version": version, "version": "9.9.9", "id": "s1",
                    "kind": "agent", "pid": 1, "child_pid": 2,
                    "transcript": "/t", "writer": true, "future_field": 1
                }),
                Some("sync") => serde_json::json!({"durable_seq": 7}),
                _ => {
                    let error = serde_json::json!({"id": request["id"],
                        "error": {"code": "something_new", "message": "no"}});
                    wr.write_all(format!("{error}\n").as_bytes()).await.unwrap();
                    continue;
                }
            };
            let response = serde_json::json!({"id": request["id"], "result": result});
            wr.write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    }

    async fn connect_to_fake(version: u32) -> Result<Client, ClientError> {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(fake_supervisor(listener, version));
        Client::connect(&socket, true).await
    }

    #[tokio::test]
    async fn accepts_current_version_and_tolerates_unknowns() {
        let client = connect_to_fake(PROTOCOL_VERSION).await.unwrap();
        assert_eq!(client.hello().id, "s1");
        assert_eq!(client.sync().await.unwrap(), 7);
        match client.status().await {
            Err(ClientError::Remote(body)) => assert_eq!(body.code, crate::ErrorCode::Unknown),
            other => panic!("unexpected {:?}", other.map(|_| ())),
        }
    }

    #[tokio::test]
    async fn rejects_versions_outside_the_window() {
        for version in [0, PROTOCOL_VERSION + 1] {
            match connect_to_fake(version).await {
                Err(ClientError::Incompatible(v)) => assert_eq!(v, version),
                other => panic!("version {version}: {:?}", other.map(|_| ())),
            }
        }
    }

    #[tokio::test]
    async fn calls_time_out_and_disconnects_fail_pending_calls() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        // Answers hello, then never answers again, then closes on demand.
        let (close_tx, close_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            let hello: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let response = serde_json::json!({"id": hello["id"], "result": {
                "protocol_version": PROTOCOL_VERSION, "version": "x", "id": "s1",
                "kind": "job", "pid": 1, "child_pid": 2, "transcript": "/t", "writer": false}});
            wr.write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
            let _ = close_rx.await;
        });
        let mut client = Client::connect(&socket, false).await.unwrap();
        client.set_timeout(Duration::from_millis(50));
        assert!(matches!(client.status().await, Err(ClientError::Timeout)));
        client.set_timeout(DEFAULT_CALL_TIMEOUT);
        let call = client.status();
        let close = async {
            let _ = close_tx.send(());
        };
        let (result, ()) = tokio::join!(call, close);
        assert!(matches!(result, Err(ClientError::Disconnected)));
        // Later calls fail at once rather than waiting for the timeout.
        assert!(matches!(
            client.status().await,
            Err(ClientError::Disconnected)
        ));
    }
}
