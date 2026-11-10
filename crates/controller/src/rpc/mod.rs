//! The UI API: JSON-RPC text frames from one browser tab, dispatched into the controller.
//!
//! Each connection is a [`UiClient`]: text frames go in with [`UiClient::send`], and responses
//! and server-pushed notifications (`state.events`, `session.events`) come out of
//! [`UiClient::recv`] on one ordered channel, so a subscription's response always precedes its
//! first event. Dropping the client ends its subscriptions and focus.
//!
//! **Per request:** the login session and allow-list are re-checked (a revoked session or a
//! subject removed from the allow-list closes the connection); `hello` must come first and is
//! handled before the next frame is read; the [`order`] contract decides what runs
//! concurrently; at most [`crate::Tuning::max_concurrent_requests`] requests run at once, and
//! the connection isn't read further while at the cap.

mod board;
mod dispatch;
mod keys;
mod machines;
mod order;
mod passthrough;
mod people;
mod profile_paths;
mod sandbox;
mod sandbox_decisions;
mod schedules;
mod state;
mod subscriptions;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use controlroom_protocol::ids::DeviceId;
use controlroom_protocol::session::DeviceRef;
use controlroom_protocol::{ErrorData, Message, RpcError, RpcRequest, RpcResponse};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc, watch};

pub(crate) use passthrough::agentd_error;
pub(crate) use subscriptions::resubscribe;

use crate::auth::{AuthError, Principal, reauthorize};
use crate::errors::internal;
use crate::hub::{ClientId, ClientSink, CloseReason, Hub};

/// Frames buffered from the shell before it waits.
const INCOMING_QUEUE: usize = 64;

/// One UI connection, as the shell sees it.
pub struct UiClient {
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<String>,
    close: watch::Receiver<Option<CloseReason>>,
}

/// The connection is closed; see [`UiClient::close_reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the UI connection is closed")]
pub struct Closed;

impl UiClient {
    /// Hands the controller one text frame from the browser. Waits while the connection is at
    /// its concurrency cap.
    pub async fn send(&self, frame: String) -> Result<(), Closed> {
        if self.close.borrow().is_some() {
            return Err(Closed);
        }
        self.tx.send(frame).await.map_err(|_| Closed)
    }

    /// The next text frame for the browser; `None` once the controller closed the connection.
    pub async fn recv(&mut self) -> Option<String> {
        recv_frame(&mut self.rx, &mut self.close).await
    }

    /// Why the controller closed the connection, if it did.
    pub fn close_reason(&self) -> Option<CloseReason> {
        *self.close.borrow()
    }

    /// Splits the client for a reader and a writer task. Dropping both halves ends the
    /// connection.
    pub fn split(self) -> (UiSender, UiReceiver) {
        (
            UiSender {
                tx: self.tx,
                close: self.close.clone(),
            },
            UiReceiver {
                rx: self.rx,
                close: self.close,
            },
        )
    }
}

/// The browser-to-controller half of a [`UiClient`].
pub struct UiSender {
    tx: mpsc::Sender<String>,
    close: watch::Receiver<Option<CloseReason>>,
}

impl UiSender {
    /// See [`UiClient::send`].
    pub async fn send(&self, frame: String) -> Result<(), Closed> {
        if self.close.borrow().is_some() {
            return Err(Closed);
        }
        self.tx.send(frame).await.map_err(|_| Closed)
    }
}

/// The controller-to-browser half of a [`UiClient`].
pub struct UiReceiver {
    rx: mpsc::Receiver<String>,
    close: watch::Receiver<Option<CloseReason>>,
}

impl UiReceiver {
    /// See [`UiClient::recv`].
    pub async fn recv(&mut self) -> Option<String> {
        recv_frame(&mut self.rx, &mut self.close).await
    }

    /// See [`UiClient::close_reason`].
    pub fn close_reason(&self) -> Option<CloseReason> {
        *self.close.borrow()
    }
}

/// The next outbound frame, unless the connection was closed (queued frames are dropped then:
/// the client reconnects and catches up through its cursors).
async fn recv_frame(
    rx: &mut mpsc::Receiver<String>,
    close: &mut watch::Receiver<Option<CloseReason>>,
) -> Option<String> {
    if close.borrow().is_some() {
        return None;
    }
    tokio::select! {
        biased;
        _ = close.wait_for(Option::is_some) => None,
        frame = rx.recv() => frame,
    }
}

/// A connection's identity and shared state, as handlers see it.
pub(crate) struct Conn {
    pub hub: Arc<Hub>,
    pub client: ClientId,
    pub principal: Principal,
    /// The login session, re-checked per request; `None` for a trusted embedding.
    pub token_hash: Option<String>,
    pub sink: ClientSink,
    pub device: Mutex<Option<DeviceId>>,
    helloed: AtomicBool,
}

impl Conn {
    /// Re-checks the login session and the allow-list.
    fn authorize(&self) -> Result<(), AuthError> {
        reauthorize(
            &self.hub.services,
            self.token_hash.as_deref(),
            &self.principal,
        )
    }

    /// The connection's device with its current label, for attribution.
    pub(crate) fn device_ref(&self) -> Option<DeviceRef> {
        let device_id = self
            .device
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        let label = match self.hub.repo().devices().get(&device_id) {
            Ok(Some(device)) => device.label,
            _ => device_id.to_string(),
        };
        Some(DeviceRef { device_id, label })
    }

    /// Sets the connection's device.
    pub(crate) fn set_device(&self, device: DeviceId) {
        if let Some(client) = self.hub.lock().clients.get_mut(&self.client) {
            client.credentials.device = Some(device.clone());
        }
        *self.device.lock().unwrap_or_else(|p| p.into_inner()) = Some(device);
    }
}

/// An action to run once the response is queued (activating a subscription).
pub(crate) type After = Box<dyn FnOnce() + Send>;

/// A handler's result.
pub(crate) struct Reply {
    pub value: Value,
    pub after: Option<After>,
}

/// A plain result.
pub(crate) fn reply<T: Serialize>(result: T) -> Result<Reply, RpcError> {
    Ok(Reply {
        value: serde_json::to_value(result).map_err(|e| internal(format!("encoding: {e}")))?,
        after: None,
    })
}

/// Opens a connection for `principal` on `hub` and starts serving it.
pub(crate) fn open(hub: Arc<Hub>, principal: Principal, token_hash: Option<String>) -> UiClient {
    let tuning = &hub.services.tuning;
    let (out_tx, out_rx) = mpsc::channel(tuning.client_buffer.max(1));
    let (in_tx, in_rx) = mpsc::channel(INCOMING_QUEUE);
    let (close_tx, close_rx) = watch::channel(None);
    let sink = ClientSink::new(out_tx, close_tx);
    let client = hub.add_client(sink.clone(), token_hash.clone());
    let conn = Arc::new(Conn {
        hub,
        client,
        principal,
        token_hash,
        sink,
        device: Mutex::new(None),
        helloed: AtomicBool::new(false),
    });
    tokio::spawn(serve(conn, in_rx, close_rx.clone()));
    UiClient {
        tx: in_tx,
        rx: out_rx,
        close: close_rx,
    }
}

/// Reads frames until the shell drops the client or the controller closes it.
async fn serve(
    conn: Arc<Conn>,
    mut incoming: mpsc::Receiver<String>,
    mut close: watch::Receiver<Option<CloseReason>>,
) {
    let permits = Arc::new(Semaphore::new(
        conn.hub.services.tuning.max_concurrent_requests.max(1),
    ));
    let sequencer = order::Sequencer::default();
    loop {
        let frame = tokio::select! {
            frame = incoming.recv() => frame,
            _ = close.wait_for(Option::is_some) => None,
        };
        let Some(frame) = frame else { break };
        let request = match parse_request(&frame) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                respond(&conn, RpcResponse::error(None, error)).await;
                continue;
            }
        };
        if let Err(e) = conn.authorize() {
            let error = RpcError::new(ErrorData::Unauthenticated, e.to_string());
            respond(&conn, RpcResponse::error(Some(request.id), error)).await;
            conn.sink.close(CloseReason::Unauthenticated);
            break;
        }
        let method = request.method.clone();
        if method == "hello" {
            // The handshake completes before the next frame is read.
            run(&conn, request).await;
            continue;
        }
        if method != "ping" && !conn.helloed.load(Ordering::SeqCst) {
            let error = RpcError::new(ErrorData::HelloRequired, "call hello first");
            respond(&conn, RpcResponse::error(Some(request.id), error)).await;
            continue;
        }
        let permit = tokio::select! {
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
            _ = close.wait_for(Option::is_some) => break,
        };
        let mut turn = sequencer.enter(order::order_key(&method, &request.params));
        let conn = conn.clone();
        tokio::spawn(async move {
            turn.ready().await;
            run(&conn, request).await;
            drop(turn);
            drop(permit);
        });
    }
    conn.hub.remove_client(conn.client);
}

/// Handles one request and queues its response.
async fn run(conn: &Arc<Conn>, request: RpcRequest<Value>) {
    let id = request.id.clone();
    let (response, after) = match dispatch::dispatch(conn, request).await {
        Ok(Reply { value, after }) => (RpcResponse::success(id, value), after),
        Err(error) => (RpcResponse::error(Some(id), error), None),
    };
    respond(conn, response).await;
    if let Some(after) = after {
        after();
    }
}

async fn respond(conn: &Conn, response: RpcResponse<Value>) {
    match serde_json::to_string(&response) {
        Ok(text) => conn.sink.send(text).await,
        Err(e) => tracing::error!(error = %e, "encoding a response"),
    }
}

/// Parses one frame. `Ok(None)` for messages that get no reply.
fn parse_request(text: &str) -> Result<Option<RpcRequest<Value>>, RpcError> {
    if text.contains('\n') {
        return Err(RpcError::new(
            ErrorData::ParseError,
            "a frame must be one message without newlines",
        ));
    }
    match Message::parse(text) {
        Ok(Message::Request(request)) => Ok(Some(request)),
        // The UI API defines no client-to-server notifications; responses aren't expected.
        Ok(Message::Notification(_) | Message::Response(_)) => Ok(None),
        Err(e) => match e.id {
            Some(id) => Err(RpcError {
                message: format!("request {id}: {}", e.error.message),
                ..*e.error
            }),
            None => Err(*e.error),
        },
    }
}

/// Marks the connection handshaken (by `hello`).
pub(crate) fn mark_helloed(conn: &Conn) {
    conn.helloed.store(true, Ordering::SeqCst);
}
