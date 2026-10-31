//! The daemon API client: line-delimited JSON-RPC 2.0 over any byte stream,
//! typically an SSH exec channel running `controlroomd connect`.
//!
//! - Calls are multiplexed: [`DaemonClient::call_raw`] may run concurrently
//!   from many tasks.
//! - Every call has a timeout ([`ClientOptions::call_timeout`] unless
//!   [`CallOptions::timeout`] overrides it), and dropping a call's future
//!   cancels it, so post-connect steps can be abandoned at any point.
//! - A response whose body doesn't parse fails only its own call
//!   ([`CallError::Malformed`]); the connection carries on.
//! - Notifications go to one handler, called on the reader task in wire
//!   order. A call can carry a response hook ([`CallOptions::on_response`])
//!   that runs on the reader task before the next message is read: this is
//!   how a subscriber records a new subscription before any notification
//!   for it is routed.
//!
//! The core is untyped (`serde_json::Value`); [`DaemonClient::call`] adds
//! types through [`RpcMethod`].

mod line;
mod message;
mod method;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

use self::line::{Line, read_line};
use self::message::{Incoming, parse_incoming};
pub use self::message::{Notification, RpcError};
pub use self::method::RpcMethod;

/// Receives every notification, in wire order, on the reader task. It must not
/// block: hand slow work to another task.
pub type NotificationHandler = Box<dyn Fn(Notification) + Send + Sync>;

/// Runs on the reader task with a call's outcome, before the next message is
/// read. It doesn't run if the call timed out, was cancelled, or the
/// connection closed first.
pub type ResponseHook = Box<dyn FnOnce(&Result<Value, CallError>) + Send>;

/// Why a call failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CallError {
    /// The daemon answered with an error.
    #[error("{0}")]
    Rpc(RpcError),
    /// No answer arrived in time. The call is abandoned; a late answer is dropped.
    #[error("no answer from the daemon within {0:?}")]
    Timeout(Duration),
    /// The connection closed before the answer arrived.
    #[error("daemon connection closed: {0}")]
    Closed(String),
    /// The answer arrived but its body is malformed (or doesn't match the
    /// method's result type).
    #[error("malformed response: {0}")]
    Malformed(String),
    /// The params could not be encoded.
    #[error("cannot encode params: {0}")]
    Encode(String),
}

/// Connection-wide settings.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// The timeout for calls that don't set their own.
    pub call_timeout: Duration,
    /// Longest accepted message line; longer ones are discarded and logged.
    pub max_line_bytes: usize,
    /// Requests buffered for the writer before callers wait.
    pub outgoing_queue: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(30),
            max_line_bytes: 32 << 20,
            outgoing_queue: 64,
        }
    }
}

/// Per-call settings.
#[derive(Default)]
pub struct CallOptions {
    /// Overrides [`ClientOptions::call_timeout`].
    pub timeout: Option<Duration>,
    /// See [`ResponseHook`].
    pub on_response: Option<ResponseHook>,
}

struct PendingCall {
    reply: oneshot::Sender<Result<Value, CallError>>,
    hook: Option<ResponseHook>,
}

struct Inner {
    out: mpsc::Sender<String>,
    pending: Mutex<HashMap<i64, PendingCall>>,
    next_id: AtomicI64,
    /// `Some(reason)` once the connection is gone.
    closed: watch::Sender<Option<String>>,
    options: ClientOptions,
}

impl Inner {
    fn pending(&self) -> MutexGuard<'_, HashMap<i64, PendingCall>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn close(&self, reason: String) {
        self.closed.send_if_modified(|c| {
            if c.is_none() {
                *c = Some(reason);
                true
            } else {
                false
            }
        });
        // Dropping the reply senders fails the waiting calls with `Closed`.
        let pending: Vec<_> = self.pending().drain().collect();
        drop(pending);
    }

    fn closed_error(&self) -> CallError {
        CallError::Closed(
            self.closed
                .borrow()
                .clone()
                .unwrap_or_else(|| "connection closed".into()),
        )
    }
}

/// Removes a call's pending entry when its future ends early (timeout or cancellation).
struct PendingGuard<'a> {
    inner: &'a Inner,
    id: i64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.inner.pending().remove(&self.id);
    }
}

/// A connection to one daemon. Cheap to clone; the daemon sees EOF when every
/// clone is dropped (or on [`Self::close`]).
#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<Inner>,
    _writer_guard: Arc<WriterGuard>,
}

/// Ends the writer task (EOF to the daemon) when the last client clone is dropped.
struct WriterGuard(Option<oneshot::Sender<()>>);

impl Drop for WriterGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("closed", &*self.inner.closed.borrow())
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Starts the reader and writer tasks on `reader` / `writer`.
    pub fn start<R, W>(
        reader: R,
        writer: W,
        options: ClientOptions,
        on_notification: NotificationHandler,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel(options.outgoing_queue.max(1));
        let (closed, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            out: out_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            closed,
            options,
        });
        let (stop_tx, stop_rx) = oneshot::channel();
        tokio::spawn(write_loop(inner.clone(), writer, out_rx, stop_rx));
        tokio::spawn(read_loop(inner.clone(), reader, on_notification));
        Self {
            inner,
            _writer_guard: Arc::new(WriterGuard(Some(stop_tx))),
        }
    }

    /// Calls `method` with untyped params and the default options.
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, CallError> {
        self.call_raw_with(method, params, CallOptions::default())
            .await
    }

    /// Calls `method` with untyped params. `Value::Null` params are omitted.
    pub async fn call_raw_with(
        &self,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<Value, CallError> {
        let inner = &*self.inner;
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, rx) = oneshot::channel();
        inner.pending().insert(
            id,
            PendingCall {
                reply,
                hook: options.on_response,
            },
        );
        let _guard = PendingGuard { inner, id };
        // `close` marks the connection closed before draining `pending`, so
        // either this check sees it or the drain fails our reply.
        if inner.closed.borrow().is_some() {
            return Err(inner.closed_error());
        }
        let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if !params.is_null() {
            request["params"] = params;
        }
        let line = format!("{request}\n");
        let timeout = options.timeout.unwrap_or(inner.options.call_timeout);
        let exchange = async {
            inner
                .out
                .send(line)
                .await
                .map_err(|_| inner.closed_error())?;
            rx.await.map_err(|_| inner.closed_error())?
        };
        tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or(Err(CallError::Timeout(timeout)))
    }

    /// Calls method `M` with the default options.
    pub async fn call<M: RpcMethod>(&self, params: &M::Params) -> Result<M::Result, CallError> {
        self.call_with::<M>(params, CallOptions::default()).await
    }

    /// Calls method `M`. A result that doesn't decode as `M::Result` fails
    /// this call only, with [`CallError::Malformed`].
    pub async fn call_with<M: RpcMethod>(
        &self,
        params: &M::Params,
        options: CallOptions,
    ) -> Result<M::Result, CallError> {
        let params = serde_json::to_value(params).map_err(|e| CallError::Encode(e.to_string()))?;
        let value = self.call_raw_with(M::NAME, params, options).await?;
        serde_json::from_value(value)
            .map_err(|e| CallError::Malformed(format!("{} result: {e}", M::NAME)))
    }

    /// Whether the connection is gone.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.borrow().is_some()
    }

    /// Resolves once the connection is gone, with the reason.
    pub async fn closed(&self) -> String {
        let mut rx = self.inner.closed.subscribe();
        match rx.wait_for(Option::is_some).await {
            Ok(reason) => reason.clone().unwrap_or_default(),
            Err(_) => "connection closed".into(),
        }
    }

    /// Closes the connection: pending calls fail and the daemon sees EOF.
    pub fn close(&self) {
        self.inner.close("closed by client".into());
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    inner: Arc<Inner>,
    mut writer: W,
    mut out: mpsc::Receiver<String>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut closed = inner.closed.subscribe();
    loop {
        tokio::select! {
            line = out.recv() => {
                let Some(line) = line else { break };
                let written = writer.write_all(line.as_bytes()).await.is_ok()
                    && writer.flush().await.is_ok();
                if !written {
                    inner.close("write to daemon failed".into());
                    break;
                }
            }
            _ = &mut stop => {
                inner.close("closed by client".into());
                break;
            }
            _ = async { closed.wait_for(Option::is_some).await.map(|_| ()) } => break,
        }
    }
    let _ = writer.shutdown().await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    inner: Arc<Inner>,
    reader: R,
    on_notification: NotificationHandler,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let max = inner.options.max_line_bytes;
    let reason = loop {
        match read_line(&mut reader, &mut buf, max).await {
            Ok(Line::Complete) => {}
            Ok(Line::TooLong) => {
                tracing::warn!("discarding a daemon message longer than {max} bytes");
                continue;
            }
            Ok(Line::Eof) => break "daemon closed the connection".to_owned(),
            Err(e) => break format!("reading from daemon: {e}"),
        }
        if buf.trim_ascii().is_empty() {
            continue;
        }
        match parse_incoming(&buf) {
            Ok(Incoming::Response { id, outcome }) => {
                let call = id.as_i64().and_then(|id| inner.pending().remove(&id));
                let Some(call) = call else {
                    tracing::debug!(%id, "dropping a response to no pending call");
                    continue;
                };
                if let Some(hook) = call.hook {
                    hook(&outcome);
                }
                let _ = call.reply.send(outcome);
            }
            Ok(Incoming::Notification(notification)) => on_notification(notification),
            Ok(Incoming::Request { id, method }) => {
                tracing::warn!(%method, "refusing a request from the daemon");
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "the controller serves no methods"},
                });
                let _ = inner.out.send(format!("{reply}\n")).await;
            }
            Err(e) => tracing::warn!("unparseable daemon message: {e}"),
        }
    };
    inner.close(reason);
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn cancelled_and_timed_out_calls_leave_nothing_pending() {
        let (client_side, server_side) = tokio::io::duplex(1 << 16);
        let (cr, cw) = tokio::io::split(client_side);
        let client = DaemonClient::start(cr, cw, ClientOptions::default(), Box::new(|_| {}));
        let mut requests = BufReader::new(server_side).lines();

        let mut call = Box::pin(client.call_raw("slow", Value::Null));
        // Poll until the request is on the wire, then drop the call.
        tokio::select! {
            _ = &mut call => panic!("no answer was sent"),
            line = requests.next_line() => assert!(line.unwrap().unwrap().contains("slow")),
        }
        assert_eq!(client.inner.pending().len(), 1);
        drop(call);
        assert_eq!(client.inner.pending().len(), 0);

        let options = CallOptions {
            timeout: Some(Duration::from_millis(1)),
            ..Default::default()
        };
        let err = client
            .call_raw_with("slow", Value::Null, options)
            .await
            .unwrap_err();
        assert!(matches!(err, CallError::Timeout(_)), "{err}");
        assert_eq!(client.inner.pending().len(), 0);
    }
}
