//! `session.subscribe` (one session's timeline and detail, by transcript seq) and
//! `unsubscribe`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{Seq, SessionId, SubscriptionId};
use crate::session::SessionDetail;
use crate::timeline::{RawFrame, TimelineOp};

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
    /// Send timeline ops (default `true`). With `false` the subscription carries the detail
    /// only: no backlog and no live ops (`since_seq` is ignored), e.g. for a queue panel of a
    /// session whose timeline isn't shown. Raw frames still follow `raw_since`.
    #[serde(default = "yes")]
    #[ts(as = "Option<bool>", optional)]
    pub timeline: bool,
}

fn yes() -> bool {
    true
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
    pub detail: Option<Box<SessionDetail>>,
}

/// Params of `unsubscribe`, which ends any subscription on the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UnsubscribeParams {
    /// The subscription to end.
    pub subscription_id: SubscriptionId,
}
