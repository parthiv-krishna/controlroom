//! Connected UI clients as the hub sees them: their outbound queue, state subscriptions, focus
//! and identity, and how the hub closes them.

use std::collections::HashMap;

use controlroom_protocol::NotificationMethod;
use controlroom_protocol::RpcNotification;
use controlroom_protocol::ids::{DeviceId, SessionId, SubscriptionId};
use controlroom_protocol::ui::StateChange;
use tokio::sync::{mpsc, watch};

/// Identifies one UI connection within a hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ClientId(pub u64);

/// Why the controller closed a UI connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The login session or device was revoked, the allow-list no longer admits the user, or
    /// the user logged out: sign in again.
    Unauthenticated,
    /// The client fell too far behind on server-pushed messages; reconnect and catch up.
    Overflow,
    /// The controller is shutting down.
    Shutdown,
}

/// A UI client's outbound queue.
#[derive(Clone)]
pub(crate) struct ClientSink {
    tx: mpsc::Sender<String>,
    close: watch::Sender<Option<CloseReason>>,
}

impl ClientSink {
    pub(crate) fn new(tx: mpsc::Sender<String>, close: watch::Sender<Option<CloseReason>>) -> Self {
        Self { tx, close }
    }

    /// Queues a server-pushed message without waiting; a client that fell this far behind is
    /// closed (it reconnects and catches up through its cursors).
    pub(crate) fn push(&self, message: String) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(message) {
            tracing::warn!("UI client fell behind; closing it");
            self.close(CloseReason::Overflow);
        }
    }

    /// Queues a notification.
    pub(crate) fn notify<N: NotificationMethod>(&self, params: N::Params) {
        match serde_json::to_string(&RpcNotification::new::<N>(params)) {
            Ok(text) => self.push(text),
            Err(e) => tracing::error!(error = %e, "encoding a notification"),
        }
    }

    /// Queues a response, waiting for room.
    pub(crate) async fn send(&self, message: String) {
        let _ = self.tx.send(message).await;
    }

    /// Closes the connection (the first reason wins).
    pub(crate) fn close(&self, reason: CloseReason) {
        self.close.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(reason);
                true
            } else {
                false
            }
        });
    }
}

/// What a client shows, for push deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Focus {
    /// Its device.
    pub device_id: DeviceId,
    /// The open session.
    pub session_id: Option<SessionId>,
    /// Whether the page is visible and focused.
    pub visible: bool,
}

/// A connected UI client.
pub(crate) struct ClientEntry {
    pub sink: ClientSink,
    /// State subscriptions, with the changes buffered until the subscribe response was sent.
    pub state_subs: HashMap<SubscriptionId, Option<Vec<StateChange>>>,
    /// What it shows, for push deduplication.
    pub focus: Option<Focus>,
    /// Its device, from `hello` or `devices.register`.
    pub device: Option<DeviceId>,
    /// The login session it authenticated with, if any.
    pub auth_token_hash: Option<String>,
}
