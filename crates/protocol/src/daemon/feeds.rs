//! The daemon's subscription feeds: `machine.subscribe` (every session's summary, by revision)
//! and `session.subscribe` (one session's timeline, by transcript seq).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::DaemonSettings;
use crate::ids::{Rev, SessionId, SubscriptionId};
use crate::session::SessionSummary;

/// Params of `machine.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MachineSubscribeParams {
    /// The last `rev` the client applied, or `null` when it has no cached state.
    pub since_rev: Option<Rev>,
}

/// Everything the machine feed describes, as of one revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineSnapshot {
    /// Every session on the machine, closed ones included.
    pub sessions: Vec<SessionSummary>,
    /// The daemon settings.
    pub settings: DaemonSettings,
}

/// Result of `machine.subscribe`. With a `snapshot` (when `since_rev` was `null`, is older than
/// the daemon's persisted rev log, or is ahead of the daemon's current rev), the snapshot
/// replaces the client's state and `machine.events` follow from `rev`. Without one,
/// `machine.events` follow with every change after `since_rev`. The result is sent before any
/// notification of the subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineSubscribeResult {
    /// Identifies this subscription in its notifications and in `unsubscribe`.
    pub subscription_id: SubscriptionId,
    /// The daemon's current revision.
    pub rev: Rev,
    /// The full state at `rev`, when the client can't catch up incrementally.
    pub snapshot: Option<MachineSnapshot>,
}

/// One change in the machine feed, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineEvent {
    /// A session was created, or anything in its summary changed.
    SessionUpserted {
        /// The full summary.
        session: Box<SessionSummary>,
    },
    /// A session was deleted (by `session.delete` or retention).
    SessionRemoved {
        /// The deleted session.
        id: SessionId,
    },
    /// The daemon settings changed.
    SettingsChanged {
        /// The new settings.
        settings: DaemonSettings,
    },
    /// An event this build doesn't know; ignore it (the rev still counts).
    #[serde(other)]
    Unknown,
}

/// A [`MachineEvent`] with its revision; on the wire the event's fields sit next to `rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineChange {
    /// The revision of this change; strictly increasing, persisted across restarts.
    pub rev: Rev,
    /// What changed.
    #[serde(flatten)]
    pub event: MachineEvent,
}

/// Params of the `machine.events` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineEventsParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// Changes in `rev` order.
    pub changes: Vec<MachineChange>,
}
