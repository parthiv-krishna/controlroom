//! `GET /api/stream/terminal/{session}/{shell}`: a terminal stream (spec §22,
//! [`controlroom_protocol::terminal`]), bridged into a controller [`TerminalStream`].
//!
//! - The upgrade follows `/api/ws`'s rules ([`ws::admit`]): an allowed `Origin`, then a live
//!   login session. An unknown session or shell is a 404 before the upgrade.
//! - Binary frames from the browser are typed into the terminal; text frames are
//!   [`TerminalCommand`]s. Output goes out as binary frames, events as text frames.
//! - Messages are capped at [`MAX_INPUT_BYTES`] (a large paste).
//! - Close codes: 1000 after `ended`, [`ws::CLOSE_UNAUTHENTICATED`] when the login is gone,
//!   1001 at shutdown, 1008 for an invalid frame.

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use controlroom_controller::{TerminalFrame, TerminalOutput, TerminalStream};
use controlroom_protocol::ErrorData;
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::terminal::TerminalCommand;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use tokio::sync::oneshot;

use crate::server::AppState;
use crate::ws::{self, WRITER_GRACE, close_frame, controller_close};

/// The largest accepted message from the browser.
pub const MAX_INPUT_BYTES: usize = 1 << 20;

/// The upgrade.
pub(crate) async fn upgrade(
    State(state): State<Arc<AppState>>,
    Path((session, shell)): Path<(String, String)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let auth = match ws::admit(&state, &headers) {
        Ok(auth) => auth,
        Err(refused) => return refused.into_response(),
    };
    let no_shell = || (StatusCode::NOT_FOUND, "no such shell\n").into_response();
    let Ok(session) = session.parse::<SessionId>() else {
        return no_shell();
    };
    let stream = match state.controller.open_terminal(&auth, session, &shell) {
        Ok(stream) => stream,
        Err(e)
            if matches!(
                e.data,
                Some(ErrorData::UnknownSession { .. } | ErrorData::ShellGone { .. })
            ) =>
        {
            return no_shell();
        }
        Err(e) => {
            tracing::error!(error = %e.message, "opening a terminal stream");
            return (StatusCode::SERVICE_UNAVAILABLE, "try again later\n").into_response();
        }
    };
    upgrade
        .max_message_size(MAX_INPUT_BYTES)
        .max_frame_size(MAX_INPUT_BYTES)
        .on_upgrade(move |socket| bridge(socket, stream))
}

/// Shuttles frames between the socket and the stream until either side closes.
async fn bridge(socket: WebSocket, stream: TerminalStream) {
    let (input, output) = stream.split();
    let (sink, mut source) = socket.split();
    let (close_tx, close_rx) = oneshot::channel();
    let mut writer = tokio::spawn(write(sink, output, close_rx));

    let close = loop {
        let message = tokio::select! {
            message = source.next() => message,
            _ = &mut writer => return,
        };
        let sent = match message {
            Some(Ok(Message::Binary(bytes))) => input.send(bytes.to_vec()).await,
            Some(Ok(Message::Text(text))) => match serde_json::from_str(text.as_str()) {
                Ok(TerminalCommand::Resize { cols, rows }) => input.resize(cols, rows).await,
                Ok(TerminalCommand::Unknown) => Ok(()),
                Err(_) => break Some(close_frame(close_code::POLICY, "invalid command")),
            },
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => Ok(()),
            Some(Ok(Message::Close(_))) | None => break Some(close_frame(close_code::NORMAL, "")),
            Some(Err(e)) => {
                tracing::debug!(error = %e, "terminal WebSocket read failed");
                break Some(close_frame(close_code::POLICY, "invalid frame"));
            }
        };
        if sent.is_err() {
            // The stream ended or was closed; the writer sends the close frame.
            break None;
        }
    };
    match close {
        Some(frame) => {
            let _ = close_tx.send(frame);
        }
        None => drop(close_tx),
    }
    drop(input);
    if tokio::time::timeout(WRITER_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

/// Writes the stream's frames, then a close frame: the controller's reason (normal after
/// `ended`), or the reader's when it stopped first.
async fn write(
    mut sink: SplitSink<WebSocket, Message>,
    mut output: TerminalOutput,
    mut close_rx: oneshot::Receiver<CloseFrame>,
) {
    let frame = loop {
        tokio::select! {
            biased;
            frame = &mut close_rx => match frame {
                Ok(frame) => break frame,
                Err(_) => break controller_close(output.close_reason()),
            },
            frame = output.recv() => {
                let message = match frame {
                    Some(TerminalFrame::Output(bytes)) => Message::Binary(bytes.into()),
                    Some(TerminalFrame::Event(event)) => match serde_json::to_string(&event) {
                        Ok(text) => Message::Text(text.into()),
                        Err(e) => {
                            tracing::error!(error = %e, "encoding a terminal event");
                            continue;
                        }
                    },
                    None => break controller_close(output.close_reason()),
                };
                if sink.send(message).await.is_err() {
                    return;
                }
            }
        }
    };
    drop(output);
    let _ = sink.send(Message::Close(Some(frame))).await;
}
