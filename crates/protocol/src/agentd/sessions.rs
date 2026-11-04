//! The agent daemon's sessions feed (`sessions.subscribe`, `sessions.events`) of global
//! sessions ([`SessionSummary`]).
//!
//! Sessions belong to the server, not to a machine: a session's files are in its workspaces,
//! which may sit on several machines. A session never goes stale with a machine; only its
//! workspaces become unavailable.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::DaemonSettings;
use crate::ids::{Rev, SessionId, SubscriptionId};
use crate::session::SessionSummary;

/// Params of `sessions.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionsSubscribeParams {
    /// The last `rev` the client applied, or `null` when it has no cached state.
    pub since_rev: Option<Rev>,
}

/// Everything the sessions feed describes, as of one revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionsSnapshot {
    /// Every session, closed ones included.
    pub sessions: Vec<SessionSummary>,
    /// The agent daemon's settings.
    pub settings: DaemonSettings,
}

/// Result of `sessions.subscribe`. With a `snapshot` (when `since_rev` was `null`, is older than
/// the persisted rev log, or is ahead of the current rev), the snapshot replaces the client's
/// state and `sessions.events` follow from `rev`. Without one, `sessions.events` follow with
/// every change after `since_rev`. The result is sent before any notification of the
/// subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionsSubscribeResult {
    /// Identifies this subscription in its notifications and in `unsubscribe`.
    pub subscription_id: SubscriptionId,
    /// The current revision.
    pub rev: Rev,
    /// The full state at `rev`, when the client can't catch up incrementally.
    pub snapshot: Option<SessionsSnapshot>,
}

/// One change in the sessions feed, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionsEvent {
    /// A session was created, or anything in its summary changed (including a workspace's
    /// availability or a shell).
    SessionUpserted {
        /// The full summary.
        session: Box<SessionSummary>,
    },
    /// A session was deleted (by `session.delete` or retention).
    SessionRemoved {
        /// The deleted session.
        id: SessionId,
    },
    /// The settings changed.
    SettingsChanged {
        /// The new settings.
        settings: DaemonSettings,
    },
    /// An event this build doesn't know; ignore it (the rev still counts).
    #[serde(other)]
    Unknown,
}

/// A [`SessionsEvent`] with its revision; on the wire the event's fields sit next to `rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionsChange {
    /// The revision of this change; strictly increasing, persisted across restarts.
    pub rev: Rev,
    /// What changed.
    #[serde(flatten)]
    pub event: SessionsEvent,
}

/// Params of the `sessions.events` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionsEventsParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// Changes in `rev` order.
    pub changes: Vec<SessionsChange>,
}
