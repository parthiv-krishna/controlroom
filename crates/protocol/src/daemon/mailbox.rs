//! The mailbox channel between a daemon and its connected controller, which brokers messages
//! between machines. Only the controller (not debug clients) may use it. The daemon never calls
//! the controller:
//!
//! 1. The daemon appends what its agents send (messages, cross-machine spawns, answers to
//!    controller-raised requests) to a durable outbox. The controller reads it with
//!    `outbox.subscribe`.
//! 2. The controller applies the contact policy, delivers, and reports each item's outcome with
//!    `outbox.ack`. A `queued` ack is interim; any other outcome is final and removes the item.
//! 3. Everything addressed to a session comes back as `inbox.deliver` (with an `op_id`, so a
//!    retried delivery is applied once): inbound messages and replies, requests the controller
//!    needs the user to answer (contact, spawn), the session's peer directory, and updates from
//!    remote children.
//!
//! Unacknowledged items survive disconnects: after a reconnect the controller resubscribes with
//! the last rev it saw.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{
    MessageId, OpId, OutboxId, RemotePath, RequestId, Rev, SessionId, SubscriptionId, UnixMillis,
};
use crate::session::{ContentPart, Peer, PeerRef, Request, Response, RunOutcome, SessionState};

/// What an outbox item asks the controller to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxBody {
    /// Send a message (or an ask, or a reply) to another session.
    Message {
        /// The message.
        message_id: MessageId,
        /// The recipient.
        to: PeerRef,
        /// The text.
        text: String,
        /// The message this replies to.
        reply_to: Option<MessageId>,
        /// Whether the sender awaits a reply.
        expects_reply: bool,
        /// Hold it while the recipient is unreachable, until `expires_at`, instead of failing
        /// with `connection_lost`.
        queue_if_offline: bool,
    },
    /// Spawn a child session on another machine (always needs the user's approval).
    Spawn {
        /// The target machine's display name, as the agent named it.
        machine: String,
        /// The agent key.
        agent: String,
        /// The child's working directory.
        cwd: RemotePath,
        /// The child's title.
        title: String,
        /// The child's first prompt.
        prompt: Vec<ContentPart>,
    },
    /// The user's answer to a request the controller raised with `inbox.deliver`.
    Resolved {
        /// The request.
        request_id: RequestId,
        /// The answer.
        response: Response,
    },
    /// An item kind this build doesn't know; ack it `denied`.
    #[serde(other)]
    Unknown,
}

/// One outbox item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutboxItem {
    /// The item.
    pub id: OutboxId,
    /// Its position in the outbox feed.
    pub rev: Rev,
    /// The sending session.
    pub session_id: SessionId,
    /// When it was written.
    pub created_at: UnixMillis,
    /// When it expires undelivered.
    pub expires_at: Option<UnixMillis>,
    /// What to do.
    pub body: OutboxBody,
}

/// Params of `outbox.subscribe`. Only the controller may subscribe (`unsupported` for debug
/// clients).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutboxSubscribeParams {
    /// Skip items up to this rev (already seen); `null` for every unacknowledged item.
    pub since: Option<Rev>,
}

/// Result of `outbox.subscribe`. `outbox.items` notifications follow with newer items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutboxSubscribeResult {
    /// Identifies this subscription.
    pub subscription_id: SubscriptionId,
    /// Unacknowledged items after `since`, in rev order.
    pub items: Vec<OutboxItem>,
}

/// Params of the `outbox.items` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutboxItemsParams {
    /// The subscription.
    pub subscription_id: SubscriptionId,
    /// New items, in rev order.
    pub items: Vec<OutboxItem>,
}

/// Which link was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LinkScope {
    /// The daemon can't reach the controller.
    Controller,
    /// The controller can't reach the peer's machine.
    Peer,
    /// A scope this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The outcome of an outbox item, reported to the sending agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OutboxOutcome {
    /// Delivered (a `resolved` item: applied).
    Delivered,
    /// The child session was created (for `spawn`).
    Spawned {
        /// The new child.
        child: PeerRef,
    },
    /// Interim: held until the recipient is reachable or the item expires.
    Queued {
        /// When it expires.
        expires_at: UnixMillis,
    },
    /// Refused by the policy or the user.
    Denied {
        /// Why, if known.
        reason: Option<String>,
    },
    /// Not delivered because a link is down.
    ConnectionLost {
        /// Which link.
        scope: LinkScope,
        /// Since when it has been down.
        since: UnixMillis,
    },
    /// Expired undelivered.
    Expired,
    /// An outcome this build doesn't know; the item stays unacknowledged.
    #[serde(other)]
    Unknown,
}

/// Params of `outbox.ack`. Idempotent per item and outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OutboxAckParams {
    /// The item.
    pub item: OutboxId,
    /// What happened.
    pub outcome: OutboxOutcome,
}

/// What the controller delivers to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxDelivery {
    /// A message, ask or reply from another session; injected into the agent's next turn.
    Message {
        /// The message.
        message_id: MessageId,
        /// The sender.
        from: PeerRef,
        /// The text.
        text: String,
        /// The message this replies to.
        reply_to: Option<MessageId>,
        /// Whether the sender awaits a reply.
        expects_reply: bool,
        /// A reply that arrives after its ask resolved (e.g. with `connection_lost`).
        late: bool,
    },
    /// A request the user must answer on this session (`contact` or `spawn`). The answer goes
    /// back as an outbox `resolved` item.
    Request {
        /// The request.
        request: Box<Request>,
    },
    /// The session's contact directory, replacing the previous one.
    Peers {
        /// Sessions it may contact.
        peers: Vec<Peer>,
    },
    /// The state of a child session on another machine, for the parent's subagent item and
    /// `await`.
    ChildUpdate {
        /// The child.
        child: PeerRef,
        /// Its state.
        state: SessionState,
        /// How its last run ended.
        outcome: Option<RunOutcome>,
        /// Its result text, once done.
        result: Option<String>,
    },
    /// A delivery kind this build doesn't know; rejected with `unsupported`.
    #[serde(other)]
    Unknown,
}

/// Params of `inbox.deliver`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InboxDeliverParams {
    /// Idempotency key: a retried delivery is applied once.
    pub op_id: OpId,
    /// The receiving session.
    pub id: SessionId,
    /// What to deliver.
    pub delivery: InboxDelivery,
}
