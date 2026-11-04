//! One client connection: newline-delimited JSON-RPC (`docs/protocol.md` §5).
//!
//! - Every line is parsed on its own ([`Message::parse`]); a bad line fails only its own id.
//! - `hello` must come first (`hello_required` otherwise): a major version mismatch gets
//!   `incompatible_version`; a controller is registered under the single-controller rule
//!   ([`super::controller`]); debug clients may connect alongside, but only the controller
//!   may carry host requests (`hostops.*`).
//! - After `hello`, each request runs on its own task, at most [`MAX_IN_FLIGHT`] at a time (the
//!   read loop waits for a slot), so slow work never blocks reading.
//! - Everything outgoing goes through one bounded queue drained by a writer task; a slow
//!   client backpressures its own feeds, which then resubscribe when they lag.
//! - When the connection ends, its subscriptions end. Nothing else happens: requests already
//!   running finish, and their answers are dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use controlroom_protocol::agentd::{ClientRole, HelloParams, HelloResult, methods};
use controlroom_protocol::ids::SubscriptionId;
use controlroom_protocol::jsonrpc::encode_line;
use controlroom_protocol::{
    AGENTD_PROTOCOL_VERSION, ErrorData, Message, Method, RpcError, RpcId, RpcRequest, RpcResponse,
};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::AbortHandle;

use super::Shared;
use super::dispatch;
use crate::version::VERSION;

/// Requests of one connection running at once.
pub const MAX_IN_FLIGHT: usize = 32;

/// Outgoing lines buffered per connection.
const OUT_QUEUE: usize = 1024;

/// Longest request line.
const MAX_LINE_BYTES: u64 = 16 << 20;

/// What a request handler needs of its connection.
pub struct Conn {
    /// Daemon-wide state.
    pub shared: Arc<Shared>,
    id: u64,
    out: mpsc::Sender<String>,
    subscriptions: Mutex<HashMap<SubscriptionId, AbortHandle>>,
    close: watch::Sender<bool>,
    /// The client said hello as the controller.
    controller: AtomicBool,
}

impl Conn {
    /// Whether this is the controller's connection.
    pub fn is_controller(&self) -> bool {
        self.controller.load(Ordering::Relaxed)
    }

    /// Queues a line; a closed connection drops it.
    pub async fn send_line(&self, line: String) {
        let _ = self.out.send(line).await;
    }

    /// Queues any JSON-RPC message.
    pub async fn send<T: Serialize>(&self, message: &T) {
        match encode_line(message) {
            Ok(line) => self.send_line(line).await,
            Err(e) => tracing::error!("encoding a message: {e}"),
        }
    }

    /// Answers request `id`.
    pub async fn respond(&self, id: RpcId, result: Result<Value, RpcError>) {
        let response = match result {
            Ok(value) => RpcResponse::success(id, value),
            Err(error) => RpcResponse::error(Some(id), error),
        };
        self.send(&response).await;
    }

    /// Registers a running feed.
    pub fn add_subscription(&self, id: SubscriptionId, task: AbortHandle) {
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, task);
    }

    /// Ends a feed; `false` if there is no such subscription.
    pub fn remove_subscription(&self, id: &SubscriptionId) -> bool {
        let task = self
            .subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        task.map(|t| t.abort()).is_some()
    }

    /// Closes the connection (a feed that can't catch up, a replaced controller).
    pub fn close(&self) {
        self.close.send_replace(true);
    }
}

/// Serves `stream` until the client goes away or the connection is closed.
pub async fn serve(shared: Arc<Shared>, id: u64, stream: UnixStream) {
    let (read, write) = stream.into_split();
    let (out, out_rx) = mpsc::channel(OUT_QUEUE);
    let writer = tokio::spawn(write_lines(write, out_rx));
    let (close, mut closed) = watch::channel(false);
    let conn = Arc::new(Conn {
        shared,
        id,
        out,
        subscriptions: Mutex::new(HashMap::new()),
        close,
        controller: AtomicBool::new(false),
    });
    let mut reader = BufReader::new(read);
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut hello_done = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut limited = (&mut reader).take(MAX_LINE_BYTES + 1);
        let read = tokio::select! {
            read = limited.read_until(b'\n', &mut line) => read,
            _ = closed.wait_for(|c| *c) => break,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() as u64 > MAX_LINE_BYTES => {
                tracing::warn!("connection {id}: request line too long; closing");
                break;
            }
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        if text.trim().is_empty() {
            continue;
        }
        let request = match Message::parse(&text) {
            Ok(Message::Request(request)) => request,
            Ok(Message::Notification(_) | Message::Response(_)) => continue,
            Err(e) => {
                conn.send(&RpcResponse::<Value>::error(e.id, *e.error))
                    .await;
                continue;
            }
        };
        if !hello_done {
            let request_id = request.id.clone();
            let result = hello(&conn, request);
            hello_done = result.is_ok();
            conn.respond(request_id, result).await;
            continue;
        }
        let Ok(slot) = slots.clone().acquire_owned().await else {
            break;
        };
        let conn = conn.clone();
        tokio::spawn(async move {
            dispatch::handle(&conn, request).await;
            drop(slot);
        });
    }
    conn.shared.controllers.release(conn.id);
    let subscriptions: Vec<AbortHandle> = conn
        .subscriptions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain()
        .map(|(_, task)| task)
        .collect();
    for task in subscriptions {
        task.abort();
    }
    drop(conn);
    // The writer ends once every running request has dropped its handle on the queue.
    let _ = writer.await;
}

/// The first request: `hello`, or `hello_required`.
fn hello(conn: &Conn, request: RpcRequest<Value>) -> Result<Value, RpcError> {
    if request.method != methods::Hello::NAME {
        return Err(RpcError::new(
            ErrorData::HelloRequired,
            format!("call hello before {}", request.method),
        ));
    }
    let params: HelloParams =
        controlroom_protocol::jsonrpc::parse_params(methods::Hello::NAME, request.params)?;
    if !params
        .protocol_version
        .is_compatible_with(AGENTD_PROTOCOL_VERSION)
    {
        return Err(RpcError::new(
            ErrorData::IncompatibleVersion {
                protocol_version: AGENTD_PROTOCOL_VERSION,
                version: VERSION.to_owned(),
            },
            format!(
                "protocol {} is incompatible with this daemon's {AGENTD_PROTOCOL_VERSION}",
                params.protocol_version
            ),
        ));
    }
    if let ClientRole::Controller { controller_id } = params.role {
        conn.shared
            .controllers
            .claim(controller_id, conn.id, conn.close.clone())?;
        conn.controller.store(true, Ordering::Relaxed);
    }
    tracing::info!(
        "connection {}: hello from {:?} ({:?})",
        conn.id,
        params.client_name,
        params.role
    );
    let result = HelloResult {
        daemon_version: VERSION.to_owned(),
        protocol_version: AGENTD_PROTOCOL_VERSION,
        daemon_id: conn.shared.runtime.daemon_id(),
        capabilities: conn.shared.capabilities.clone(),
    };
    serde_json::to_value(result).map_err(|e| RpcError::new(ErrorData::InternalError, e.to_string()))
}

/// Writes queued lines until the queue closes or the client goes away.
async fn write_lines(mut write: OwnedWriteHalf, mut lines: mpsc::Receiver<String>) {
    while let Some(line) = lines.recv().await {
        if write.write_all(line.as_bytes()).await.is_err() {
            return;
        }
    }
    let _ = write.shutdown().await;
}
