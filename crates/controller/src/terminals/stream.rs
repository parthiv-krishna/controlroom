//! The task behind one [`super::TerminalStream`]: attach, and attach again after a link drop or
//! when the browser has caught up, until the stream ends or is closed.

use std::sync::Arc;
use std::time::Duration;

use controlroom_client_core::ssh::SshConnection;
use controlroom_protocol::terminal::TerminalEvent;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, watch};

use super::channel::{self, Io, Outcome, Position};
use super::target::Target;
use super::{AUTH_RECHECK, Input, OUTPUT_QUEUE, Opener, TerminalFrame, closed};
use crate::auth::reauthorize;
use crate::hub::{BusEvent, ClientId, CloseReason, Hub, close_once};

/// The first and the longest wait between attach attempts while nothing else wakes the
/// stream (a broken channel on a connection that still looks fine).
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// One stream's state.
pub(super) struct Stream {
    pub hub: Arc<Hub>,
    pub id: ClientId,
    pub target: Target,
    pub opener: Opener,
    pub input: mpsc::Receiver<Input>,
    pub output: mpsc::Sender<TerminalFrame>,
    pub close: watch::Sender<Option<CloseReason>>,
}

/// What woke a waiting stream.
enum Wake {
    /// Something about the machine or the session changed.
    Event,
    /// The retry delay passed.
    Timer,
    /// The browser left, or the stream was closed.
    Closed,
}

/// Runs the stream, then forgets it.
pub(super) async fn run(mut stream: Stream) {
    let (hub, opener, close) = (
        stream.hub.clone(),
        stream.opener.clone(),
        stream.close.clone(),
    );
    tokio::select! {
        () = stream.serve() => {}
        () = watchdog(&hub, &opener, &close) => {}
    }
    stream.hub.remove_terminal(stream.id);
}

/// Re-checks the login every [`AUTH_RECHECK`]; closes the stream once it fails.
async fn watchdog(hub: &Hub, opener: &Opener, close: &watch::Sender<Option<CloseReason>>) {
    loop {
        tokio::time::sleep(AUTH_RECHECK).await;
        if !authorized(hub, opener) {
            close_once(close, CloseReason::Unauthenticated);
            break;
        }
    }
    std::future::pending().await
}

fn authorized(hub: &Hub, opener: &Opener) -> bool {
    reauthorize(
        &hub.services,
        opener.token_hash.as_deref(),
        &opener.principal,
    )
    .is_ok()
}

impl Stream {
    async fn serve(&mut self) {
        let mut bus = self.hub.subscribe_bus();
        let mut close = self.close.subscribe();
        let mut pos = Position::default();
        let mut retry = RETRY_FIRST;
        // The connection the last attach broke on: skipped until it is replaced or the retry
        // delay passed.
        let mut broken: Option<Arc<SshConnection>> = None;
        let mut detached = false;
        loop {
            if let Some(why) = self.target.gone(&self.hub) {
                return self.end(why, &mut close).await;
            }
            if !authorized(&self.hub, &self.opener) {
                close_once(&self.close, CloseReason::Unauthenticated);
                return;
            }
            let ssh = self.hub.host_ssh(self.target.machine).filter(|ssh| {
                !ssh.is_closed() && !broken.as_ref().is_some_and(|b| Arc::ptr_eq(b, ssh))
            });
            let Some(ssh) = ssh else {
                if !detached {
                    let why = format!("{} is unreachable", self.target.machine_name(&self.hub));
                    detached = self.detach(why, &mut close).await;
                }
                match self.wait(&mut bus, &mut close, &mut pos, retry).await {
                    Wake::Closed => return,
                    Wake::Timer => {
                        broken = None;
                        retry = (retry * 2).min(RETRY_MAX);
                    }
                    Wake::Event => {}
                }
                continue;
            };

            let attaches = pos.attaches;
            let io = Io {
                input: &mut self.input,
                output: &self.output,
                close: &mut close,
            };
            let outcome = channel::attach(&ssh, &self.target.shell_id, &mut pos, io).await;
            if pos.attaches > attaches {
                detached = false;
                retry = RETRY_FIRST;
            }
            match outcome {
                Outcome::Ended(why) => return self.end(why, &mut close).await,
                Outcome::Closed => return,
                Outcome::Behind => {
                    let room = tokio::select! {
                        () = closed(&mut close) => None,
                        permits = self.output.reserve_many(OUTPUT_QUEUE / 2) => permits.ok(),
                    };
                    if room.is_none() {
                        return;
                    }
                }
                Outcome::Lost(why) => {
                    tracing::debug!(shell = %self.target.shell_id, %why, "terminal channel lost");
                    broken = Some(ssh);
                    if !detached {
                        let why = format!("{}: {why}", self.target.machine_name(&self.hub));
                        detached = self.detach(why, &mut close).await;
                    }
                }
            }
        }
    }

    /// Waits for a change that may let the stream attach (or end it), keeping the browser's
    /// latest size; input typed meanwhile is dropped.
    async fn wait(
        &mut self,
        bus: &mut broadcast::Receiver<BusEvent>,
        close: &mut watch::Receiver<Option<CloseReason>>,
        pos: &mut Position,
        delay: Duration,
    ) -> Wake {
        let timer = tokio::time::sleep(delay);
        tokio::pin!(timer);
        loop {
            tokio::select! {
                biased;
                () = closed(close) => return Wake::Closed,
                () = &mut timer => return Wake::Timer,
                event = bus.recv() => match event {
                    Ok(event) if self.target.concerns(&event) => return Wake::Event,
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => return Wake::Event,
                    Err(RecvError::Closed) => return Wake::Closed,
                },
                input = self.input.recv() => match input {
                    None => return Wake::Closed,
                    Some(Input::Resize { cols, rows }) => pos.size = Some((cols, rows)),
                    Some(Input::Bytes(_)) => {}
                },
            }
        }
    }

    /// Tells the browser the link is down; returns whether it was told.
    async fn detach(
        &self,
        reason: String,
        close: &mut watch::Receiver<Option<CloseReason>>,
    ) -> bool {
        self.event(TerminalEvent::Detached { reason }, close).await
    }

    /// Tells the browser the stream ended; dropping the queue then ends the browser's side.
    async fn end(&self, reason: String, close: &mut watch::Receiver<Option<CloseReason>>) {
        self.event(TerminalEvent::Ended { reason }, close).await;
    }

    /// Queues an event, waiting for room (unless the stream is closed meanwhile).
    async fn event(
        &self,
        event: TerminalEvent,
        close: &mut watch::Receiver<Option<CloseReason>>,
    ) -> bool {
        tokio::select! {
            () = closed(close) => false,
            sent = self.output.send(TerminalFrame::Event(event)) => sent.is_ok(),
        }
    }
}
