//! Terminal streams (spec §22): a browser attached to one of a session's shells.
//!
//! Each [`TerminalStream`] runs `controlroomd pty attach <shell id> --framed` on its own exec
//! channel of the controller's existing SSH connection to the shell's machine
//! ([`controlroom_protocol::host::terminal`]). Several streams may attach to one shell, and
//! each of them types into it.
//!
//! - **Replay and resume.** The stream starts with a replay of recent output. When the link
//!   drops, it tells the browser (`detached`), waits for the machine to reconnect and resumes
//!   at the next transcript position of the same shell instance, so nothing is shown twice or
//!   lost (a reopened shell, or a gap too large, replays afresh with `reset`).
//! - **Flow control.** russh grants the remote side window as data arrives, not as it is
//!   consumed, so a reader that stops reading stalls the whole SSH connection. The stream
//!   therefore never waits on the browser while it holds a channel: when the browser falls
//!   [`OUTPUT_QUEUE`] frames behind, it closes the channel, waits until half the queue has
//!   drained, and resumes from the transcript (the host keeps the output).
//! - **Auth.** Revocation (logout, device removal, owner revoke) and shutdown close it like a
//!   UI connection ([`CloseReason`]); the login session and allow-list are re-checked before
//!   every attach and every [`AUTH_RECHECK`].
//! - **End.** When the shell's process ends, or the shell, its session or its machine is
//!   removed, the stream sends `ended` and closes.

mod channel;
mod stream;
mod target;

use std::sync::Arc;
use std::time::Duration;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::terminal::TerminalEvent;
use tokio::sync::{mpsc, watch};

use crate::auth::Principal;
use crate::hub::{CloseReason, Credentials, Hub, TerminalEntry};

/// Output frames buffered for the browser before the stream detaches to catch up.
const OUTPUT_QUEUE: usize = 256;
/// Input frames buffered from the browser before it waits.
const INPUT_QUEUE: usize = 64;
/// How often an open stream re-checks its login session.
const AUTH_RECHECK: Duration = Duration::from_secs(60);

/// A browser's terminal on a session shell. [`Self::split`] gives its halves: feed the
/// browser's input to the [`TerminalInput`] and write the [`TerminalOutput`] to it.
pub struct TerminalStream {
    input: TerminalInput,
    output: TerminalOutput,
}

/// The browser's side of a [`TerminalStream`]: keystrokes and resizes.
pub struct TerminalInput {
    tx: mpsc::Sender<Input>,
    close: watch::Receiver<Option<CloseReason>>,
}

/// What the browser receives from a [`TerminalStream`].
pub struct TerminalOutput {
    rx: mpsc::Receiver<TerminalFrame>,
    close: watch::Receiver<Option<CloseReason>>,
}

/// One frame for the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalFrame {
    /// Terminal output (a binary WebSocket frame).
    Output(Vec<u8>),
    /// An event (a text WebSocket frame).
    Event(TerminalEvent),
}

/// From the browser.
#[derive(Debug)]
enum Input {
    Bytes(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// The stream is closed; see [`TerminalOutput::close_reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the terminal stream is closed")]
pub struct Closed;

impl TerminalStream {
    /// Splits into the input and output halves.
    pub fn split(self) -> (TerminalInput, TerminalOutput) {
        (self.input, self.output)
    }
}

impl TerminalInput {
    /// Types `bytes` into the terminal (dropped while the machine is unreachable). Waits while
    /// earlier input is still being sent.
    pub async fn send(&self, bytes: Vec<u8>) -> Result<(), Closed> {
        self.push(Input::Bytes(bytes)).await
    }

    /// Resizes the terminal; the last size from any attached browser wins.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), Closed> {
        self.push(Input::Resize { cols, rows }).await
    }

    async fn push(&self, input: Input) -> Result<(), Closed> {
        if self.close.borrow().is_some() {
            return Err(Closed);
        }
        self.tx.send(input).await.map_err(|_| Closed)
    }
}

impl TerminalOutput {
    /// The next frame; `None` once the stream ended (after `ended`) or was closed (see
    /// [`Self::close_reason`]; queued frames are dropped then).
    pub async fn recv(&mut self) -> Option<TerminalFrame> {
        if self.close.borrow().is_some() {
            return None;
        }
        tokio::select! {
            biased;
            () = closed(&mut self.close) => None,
            frame = self.rx.recv() => frame,
        }
    }

    /// Why the controller closed the stream; `None` when it ended normally.
    pub fn close_reason(&self) -> Option<CloseReason> {
        *self.close.borrow()
    }
}

/// Who opened a stream, for re-checks and revocation.
#[derive(Clone)]
pub(crate) struct Opener {
    pub principal: Principal,
    /// The login session; `None` for a trusted embedding.
    pub token_hash: Option<String>,
    pub device: Option<controlroom_protocol::ids::DeviceId>,
}

/// Opens a stream to `shell` of `session`: `unknown_session` or `shell_gone` when there is
/// no such shell. An unreachable machine is no error: the stream waits for it.
pub(crate) fn open(
    hub: Arc<Hub>,
    opener: Opener,
    session: SessionId,
    shell: &str,
) -> Result<TerminalStream, RpcError> {
    let target = target::Target::resolve(&hub, session, shell)?;
    let (in_tx, in_rx) = mpsc::channel(INPUT_QUEUE);
    let (out_tx, out_rx) = mpsc::channel(OUTPUT_QUEUE);
    let (close_tx, close_rx) = watch::channel(None);
    let id = hub.add_terminal(TerminalEntry {
        close: close_tx.clone(),
        credentials: Credentials {
            device: opener.device.clone(),
            auth_token_hash: opener.token_hash.clone(),
        },
    });
    tokio::spawn(stream::run(stream::Stream {
        hub,
        id,
        target,
        opener,
        input: in_rx,
        output: out_tx,
        close: close_tx,
    }));
    Ok(TerminalStream {
        input: TerminalInput {
            tx: in_tx,
            close: close_rx.clone(),
        },
        output: TerminalOutput {
            rx: out_rx,
            close: close_rx,
        },
    })
}

/// Waits until the stream is closed with a reason. A stream that ended normally drops its
/// senders without one: that is no close (the queued `ended` must still be read).
async fn closed(close: &mut watch::Receiver<Option<CloseReason>>) {
    if close.wait_for(Option::is_some).await.is_err() {
        std::future::pending::<()>().await;
    }
}
