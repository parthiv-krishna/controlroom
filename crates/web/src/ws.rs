//! `GET /api/ws`: the UI API transport. One WebSocket per tab, one controller
//! [`UiClient`] per socket, one JSON-RPC message per text frame.
//!
//! - The upgrade needs an allowed `Origin` (cross-site WebSocket hijacking, DNS rebinding) and
//!   a live login session.
//! - Browser frames go to [`controlroom_controller::UiSender::send`], which waits while the
//!   connection is at its concurrency cap; the socket isn't read meanwhile, so backpressure
//!   reaches the browser through TCP. Outbound frames are written by a separate task.
//! - Binary frames are refused (reserved for terminal streams); messages are capped at
//!   [`MAX_MESSAGE_BYTES`].
//! - Close codes: [`CLOSE_UNAUTHENTICATED`] (logout, device or login revoked, allow-list
//!   change: check `auth/session`), 1013 when the tab fell behind (reconnect and catch up),
//!   1001 at shutdown, 1003 for binary frames, 1008 for invalid frames.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use controlroom_controller::{CloseReason, UiClient, UiReceiver};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use tokio::sync::oneshot;

use crate::auth::authenticate;
use crate::server::AppState;

/// The largest accepted message: the agent's per-record limit, so a big prompt (with images)
/// still fits.
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;
/// The close code when the login is gone; the UI checks `auth/session` and signs in again.
pub const CLOSE_UNAUTHENTICATED: u16 = 4401;
/// How long the writer may take to flush its close frame after the reader stopped.
const WRITER_GRACE: Duration = Duration::from_secs(5);

/// The upgrade.
pub(crate) async fn upgrade(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Browsers always send Origin on WebSocket upgrades; anything else is refused.
    let origin_ok = headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .is_some_and(|o| state.origin.origin_allowed(o));
    if !origin_ok {
        return (StatusCode::FORBIDDEN, "WebSocket Origin not allowed\n").into_response();
    }
    let auth = match authenticate(&state, &headers) {
        Ok(auth) => auth,
        Err(rejection) => return rejection.into_response(),
    };
    let client = match state.controller.open_client(&auth) {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(error = %e.message, "opening a UI connection");
            return (StatusCode::SERVICE_UNAVAILABLE, "try again later\n").into_response();
        }
    };
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| bridge(socket, client))
}

/// Shuttles frames between the socket and the controller until either side closes.
async fn bridge(socket: WebSocket, client: UiClient) {
    let (to_controller, from_controller) = client.split();
    let (sink, mut stream) = socket.split();
    let (close_tx, close_rx) = oneshot::channel();
    let mut writer = tokio::spawn(write(sink, from_controller, close_rx));

    let close = loop {
        let message = tokio::select! {
            message = stream.next() => message,
            _ = &mut writer => return,
        };
        match message {
            Some(Ok(Message::Text(text))) => {
                if to_controller.send(text.to_string()).await.is_err() {
                    // The controller closed it; the writer sends the close frame.
                    break None;
                }
            }
            Some(Ok(Message::Binary(_))) => {
                break Some(close_frame(
                    close_code::UNSUPPORTED,
                    "binary frames are reserved",
                ));
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => break Some(close_frame(close_code::NORMAL, "")),
            Some(Err(e)) => {
                tracing::debug!(error = %e, "UI WebSocket read failed");
                break Some(close_frame(close_code::POLICY, "invalid frame"));
            }
        }
    };
    // The reader's close frame first: dropping the sender ends the controller connection, and
    // the writer must not report that as the reason.
    match close {
        Some(frame) => {
            let _ = close_tx.send(frame);
        }
        None => drop(close_tx),
    }
    drop(to_controller);
    if tokio::time::timeout(WRITER_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

/// Writes the controller's frames, then a close frame: the controller's reason, or the
/// reader's when it stopped first.
async fn write(
    mut sink: SplitSink<WebSocket, Message>,
    mut from_controller: UiReceiver,
    mut close_rx: oneshot::Receiver<CloseFrame>,
) {
    let frame = loop {
        tokio::select! {
            biased;
            frame = &mut close_rx => match frame {
                Ok(frame) => break frame,
                // The reader stopped because the controller closed; recv() says why.
                Err(_) => break controller_close(from_controller.close_reason()),
            },
            frame = from_controller.recv() => match frame {
                Some(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                None => break controller_close(from_controller.close_reason()),
            },
        }
    };
    // Dropping the receiver ends the controller connection before the close handshake.
    drop(from_controller);
    let _ = sink.send(Message::Close(Some(frame))).await;
}

/// The close frame for a connection the controller closed.
fn controller_close(reason: Option<CloseReason>) -> CloseFrame {
    match reason {
        Some(CloseReason::Unauthenticated) => close_frame(CLOSE_UNAUTHENTICATED, "signed out"),
        Some(CloseReason::Overflow) => close_frame(close_code::AGAIN, "fell behind; reconnect"),
        Some(CloseReason::Shutdown) => close_frame(close_code::AWAY, "server shutting down"),
        None => close_frame(close_code::NORMAL, ""),
    }
}

fn close_frame(code: u16, reason: &'static str) -> CloseFrame {
    CloseFrame {
        code,
        reason: reason.into(),
    }
}
