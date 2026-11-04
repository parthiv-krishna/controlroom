//! The daemon API client: line-delimited JSON-RPC 2.0 over any byte stream,
//! typically an SSH exec channel running `controlroomd connect`.
//!
//! - Calls are multiplexed: [`DaemonClient::call_raw`] may run concurrently
//!   from many tasks.
//! - Every call has a timeout ([`ClientOptions::call_timeout`] unless
//!   [`CallOptions::timeout`] overrides it; only calls that last as long as
//!   the work they wait for, such as a host's `tool.run`, opt out with
//!   [`CallTimeout::Never`]), and dropping a call's future
//!   cancels it, so post-connect steps can be abandoned at any point.
//! - Lines are classified with the protocol's tolerant [`Message::parse`]. A
//!   response whose body doesn't parse fails only its own call (as an
//!   `invalid_response` error); the connection carries on. Typed calls decode
//!   their result with [`RpcResponse::into_typed`].
//! - Notifications go to one handler, called on the reader task in wire
//!   order. A call can carry a response hook ([`CallOptions::on_response`])
//!   that runs on the reader task before the next message is read: this is
//!   how a subscriber records a new subscription before any notification
//!   for it is routed.
//! - [`hello`] performs the handshake as the controller and maps the typed
//!   refusals (incompatible protocol, another controller connected).

mod handshake;
mod line;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use controlroom_protocol::jsonrpc::encode_line;
use controlroom_protocol::{
    ErrorData, Message, Method, RpcError, RpcId, RpcNotification, RpcRequest, RpcResponse,
};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

pub use self::handshake::{
    HelloError, agentd_hello, check_version, hello, hello_value, host_hello,
};
use self::line::{Line, read_line};

/// Receives every notification, in wire order, on the reader task. It must not
/// block: hand slow work to another task.
pub type NotificationHandler = Box<dyn Fn(RpcNotification<Value>) + Send + Sync>;

/// Runs on the reader task with a call's response, before the next message is
/// read. It doesn't run if the call timed out, was cancelled, or the
/// connection closed first.
pub type ResponseHook = Box<dyn FnOnce(&RpcResponse<Value>) + Send>;

/// Why a call failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CallError {
    /// The daemon answered with an error, or its answer was malformed
    /// (`invalid_response`, also for a result that doesn't match the method).
    #[error("{0}")]
    Rpc(RpcError),
    /// No answer arrived in time. The call is abandoned; a late answer is dropped.
    #[error("no answer from the daemon within {0:?}")]
    Timeout(Duration),
    /// The connection closed before the answer arrived.
    #[error("daemon connection closed: {0}")]
    Closed(String),
    /// The params could not be encoded.
    #[error("cannot encode params: {0}")]
    Encode(String),
}

impl CallError {
    /// The typed protocol error, when the daemon answered (or answered malformed).
    pub fn rpc(&self) -> Option<&RpcError> {
        match self {
            CallError::Rpc(e) => Some(e),
            _ => None,
        }
    }
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

/// How long a call may wait for its answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CallTimeout {
    /// [`ClientOptions::call_timeout`].
    #[default]
    Default,
    /// This long.
    After(Duration),
    /// As long as it takes (the call still fails when the connection closes).
    Never,
}

/// Per-call settings.
#[derive(Default)]
pub struct CallOptions {
    /// Overrides [`ClientOptions::call_timeout`].
    pub timeout: CallTimeout,
    /// See [`ResponseHook`].
    pub on_response: Option<ResponseHook>,
}

struct PendingCall {
    reply: oneshot::Sender<RpcResponse<Value>>,
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

    /// Resolves call `id` with `response`, running its hook first.
    fn resolve(&self, id: i64, response: RpcResponse<Value>) {
        let Some(call) = self.pending().remove(&id) else {
            tracing::debug!(id, "dropping a response to no pending call");
            return;
        };
        if let Some(hook) = call.hook {
            hook(&response);
        }
        let _ = call.reply.send(response);
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

    /// Calls `method` with untyped params. `Value::Null` params are sent as `{}`.
    pub async fn call_raw_with(
        &self,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<Value, CallError> {
        self.exchange(method, params, options)
            .await?
            .into_result()
            .map_err(CallError::Rpc)
    }

    /// Calls method `M` with the default options.
    pub async fn call<M: Method>(&self, params: &M::Params) -> Result<M::Result, CallError> {
        self.call_with::<M>(params, CallOptions::default()).await
    }

    /// Calls method `M`. A result that doesn't decode as `M::Result` fails
    /// this call only, with an `invalid_response` error.
    pub async fn call_with<M: Method>(
        &self,
        params: &M::Params,
        options: CallOptions,
    ) -> Result<M::Result, CallError> {
        let params = serde_json::to_value(params).map_err(|e| CallError::Encode(e.to_string()))?;
        self.exchange(M::NAME, params, options)
            .await?
            .into_typed::<M>()
            .map_err(CallError::Rpc)
    }

    /// Sends one request and waits for its response.
    async fn exchange(
        &self,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<RpcResponse<Value>, CallError> {
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
        let params = match params {
            Value::Null => Value::Object(Default::default()),
            other => other,
        };
        let request = RpcRequest {
            jsonrpc: Default::default(),
            id: RpcId::Number(id),
            method: method.to_owned(),
            params,
        };
        let line = encode_line(&request).map_err(|e| CallError::Encode(e.to_string()))?;
        let exchange = async {
            inner
                .out
                .send(line)
                .await
                .map_err(|_| inner.closed_error())?;
            rx.await.map_err(|_| inner.closed_error())
        };
        let timeout = match options.timeout {
            CallTimeout::Default => inner.options.call_timeout,
            CallTimeout::After(timeout) => timeout,
            CallTimeout::Never => return exchange.await,
        };
        tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or(Err(CallError::Timeout(timeout)))
    }

    /// Whether `other` is a clone of this client (the same connection).
    pub fn same_connection(&self, other: &DaemonClient) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
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
        let line = String::from_utf8_lossy(&buf);
        match Message::parse(&line) {
            Ok(Message::Response(response)) => match response.id.clone() {
                Some(RpcId::Number(id)) => inner.resolve(id, response),
                other => tracing::debug!(?other, "dropping a response with a foreign id"),
            },
            Ok(Message::Notification(notification)) => on_notification(notification),
            Ok(Message::Request(request)) => {
                tracing::warn!(method = %request.method, "refusing a request from the daemon");
                let reply = RpcResponse::<Value>::error(
                    Some(request.id),
                    RpcError::new(
                        ErrorData::MethodNotFound,
                        "the controller serves no methods",
                    ),
                );
                if let Ok(line) = encode_line(&reply) {
                    let _ = inner.out.send(line).await;
                }
            }
            Err(e) => match e.id {
                // A response we can attribute fails only its own call.
                Some(RpcId::Number(id)) => inner.resolve(
                    id,
                    RpcResponse::error(
                        Some(RpcId::Number(id)),
                        RpcError::new(
                            ErrorData::InvalidResponse,
                            format!("malformed response: {}", e.error.message),
                        ),
                    ),
                ),
                _ => tracing::warn!("unparseable daemon message: {e}"),
            },
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
            timeout: CallTimeout::After(Duration::from_millis(1)),
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
