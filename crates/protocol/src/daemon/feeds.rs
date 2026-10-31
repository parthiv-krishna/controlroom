//! The daemon's subscription feeds: `machine.subscribe` (every session's summary, by revision)
//! and `session.subscribe` (one session's timeline, by transcript seq).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::DaemonSettings;
use crate::ids::{Rev, Seq, SessionId, SubscriptionId};
use crate::session::{SessionDetail, SessionSummary};
use crate::timeline::{RawFrame, TimelineOp};

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

/// Params of `session.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionSubscribeParams {
    /// The session.
    pub id: SessionId,
    /// Send every timeline item that changed after this seq (`0` for the whole timeline), then
    /// live ops. Resume with the last `head_seq` received.
    pub since_seq: Seq,
    /// Also stream raw transcript frames with a seq after this one. Only for the raw view or a
    /// seq link; omit otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_since: Option<Seq>,
}

/// Result of `session.subscribe`. It is sent before any `session.events` of the subscription;
/// the first events carry the backlog (an `upsert` of every item changed after `since_seq`,
/// served from the materialized timeline), then live changes follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionSubscribeResult {
    /// Identifies this subscription in its notifications and in `unsubscribe`.
    pub subscription_id: SubscriptionId,
    /// The transcript head when the subscription started.
    pub head_seq: Seq,
    /// The session detail at that point.
    pub detail: SessionDetail,
}

/// Params of the `session.events` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionEventsParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// The session.
    pub id: SessionId,
    /// Everything up to this seq is reflected; resume from here.
    pub head_seq: Seq,
    /// Timeline ops, in order.
    pub ops: Vec<TimelineOp>,
    /// Raw frames in seq order (only with `raw_since`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<RawFrame>,
    /// The new session detail, when it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<SessionDetail>,
}

/// Params of `unsubscribe`, which ends any subscription on the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UnsubscribeParams {
    /// The subscription to end.
    pub subscription_id: SubscriptionId,
}
