//! One host API connection: newline-delimited JSON-RPC (`docs/protocol.md` §6).
//!
//! - Every line is parsed on its own; a bad line fails only its own id.
//! - `hello` must come first (`hello_required` otherwise); a major version mismatch is
//!   `incompatible_version`; a controller is registered under the single-controller rule.
//! - After `hello`, each request runs on its own task, at most [`MAX_IN_FLIGHT`] at a time.
//!   `tool.run` waits as long as its call runs, so it takes no slot from short requests: it
//!   has its own, larger budget.
//! - Everything outgoing goes through one bounded queue drained by a writer task.
//! - When the connection ends, its subscriptions end. Nothing else happens: calls it started
//!   keep running (a resend on the next connection attaches to them).

use std::sync::Arc;

use controlroom_protocol::daemon::{ClientRole, HelloParams};
use controlroom_protocol::host::{HostHelloResult, methods};
use controlroom_protocol::jsonrpc::{Method, encode_line, parse_params};
use controlroom_protocol::{
    Capability, ErrorData, HOST_PROTOCOL_VERSION, Message, RpcError, RpcId, RpcRequest, RpcResponse,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Semaphore, mpsc, watch};

use super::Host;
use super::conns::ConnHandle;
use super::dispatch;
use crate::version::VERSION;

/// Short requests of one connection running at once.
pub const MAX_IN_FLIGHT: usize = 32;
/// `tool.run`/`tool.attach` calls of one connection waiting at once.
pub const MAX_CALLS: usize = 256;
/// Outgoing lines buffered per connection.
const OUT_QUEUE: usize = 1024;
/// Longest request line (a `write` carries a whole file).
const MAX_LINE_BYTES: u64 = 64 << 20;

/// What a request handler knows of its connection.
pub struct Conn {
    /// The host.
    pub host: Arc<Host>,
    /// The connection id.
    pub id: u64,
    out: mpsc::Sender<String>,
}

impl Conn {
    /// Answers request `id`.
    pub async fn respond(&self, id: RpcId, result: Result<Value, RpcError>) {
        let response = match result {
            Ok(value) => RpcResponse::success(id, value),
            Err(error) => RpcResponse::error(Some(id), error),
        };
        match encode_line(&response) {
            Ok(line) => {
                let _ = self.out.send(line).await;
            }
            Err(e) => tracing::error!("encoding a response: {e}"),
        }
    }
}

/// The capabilities this host serves.
pub fn capabilities(host: &Host) -> Vec<Capability> {
    let mut caps = vec![Capability::SHELLS, Capability::PTY, Capability::JOBS];
    if host.approvals.port().is_some() {
        caps.push(Capability::SANDBOX_APPROVALS);
    }
    caps
}

/// Serves `stream` until the client goes away or the connection is closed.
pub async fn serve(host: Arc<Host>, id: u64, stream: UnixStream) {
    let (read, write) = stream.into_split();
    let (out, out_rx) = mpsc::channel(OUT_QUEUE);
    let writer = tokio::spawn(write_lines(write, out_rx));
    let (close, mut closed) = watch::channel(false);
    let handle = ConnHandle {
        out: out.clone(),
        close,
    };
    let conn = Arc::new(Conn {
        host: host.clone(),
        id,
        out,
    });
    let mut reader = BufReader::new(read);
    let slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let call_slots = Arc::new(Semaphore::new(MAX_CALLS));
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
                if let Ok(line) = encode_line(&RpcResponse::<Value>::error(e.id, *e.error)) {
                    let _ = conn.out.send(line).await;
                }
                continue;
            }
        };
        if !hello_done {
            let request_id = request.id.clone();
            let result = hello(&host, id, &handle, request);
            hello_done = result.is_ok();
            conn.respond(request_id, result).await;
            continue;
        }
        let long = matches!(
            request.method.as_str(),
            methods::ToolRun::NAME | methods::ToolAttach::NAME
        );
        let pool = if long { &call_slots } else { &slots };
        let Ok(slot) = pool.clone().acquire_owned().await else {
            break;
        };
        let conn = conn.clone();
        tokio::spawn(async move {
            dispatch::handle(&conn, request).await;
            drop(slot);
        });
    }
    host.conns.remove(id);
    drop(conn);
    drop(handle);
    // The writer ends once every running request has dropped its handle on the queue.
    let _ = writer.await;
}

/// The first request: `hello`, or `hello_required`.
fn hello(
    host: &Host,
    id: u64,
    handle: &ConnHandle,
    request: RpcRequest<Value>,
) -> Result<Value, RpcError> {
    if request.method != methods::Hello::NAME {
        return Err(RpcError::new(
            ErrorData::HelloRequired,
            format!("call hello before {}", request.method),
        ));
    }
    let params: HelloParams = parse_params(methods::Hello::NAME, request.params)?;
    if !params
        .protocol_version
        .is_compatible_with(HOST_PROTOCOL_VERSION)
    {
        return Err(RpcError::new(
            ErrorData::IncompatibleVersion {
                protocol_version: HOST_PROTOCOL_VERSION,
                version: VERSION.to_owned(),
            },
            format!(
                "protocol {} is incompatible with this host's {HOST_PROTOCOL_VERSION}",
                params.protocol_version
            ),
        ));
    }
    host.conns.add(id, handle.clone());
    if let ClientRole::Controller { controller_id } = params.role
        && let Err(e) = host.conns.claim_controller(controller_id, id)
    {
        host.conns.remove(id);
        return Err(e);
    }
    tracing::info!(
        "connection {id}: hello from {:?} ({:?})",
        params.client_name,
        params.role
    );
    let result = HostHelloResult {
        host_version: VERSION.to_owned(),
        protocol_version: HOST_PROTOCOL_VERSION,
        daemon_id: host.daemon_id,
        epoch: host.epoch,
        capabilities: capabilities(host),
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
