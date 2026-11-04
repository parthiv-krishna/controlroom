//! Global sessions: the summary every client sees, and the agent daemon's sessions feed
//! (`sessions.subscribe`, `sessions.events`).
//!
//! Sessions belong to the server, not to a machine: a session's files are in its workspaces,
//! which may sit on several machines. A session never goes stale with a machine; only its
//! workspaces become unavailable.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::DaemonSettings;
use crate::ids::{Rev, Seq, SessionId, SubscriptionId, UnixMillis};
use crate::session::{
    Attention, AttentionRollup, ContextUsage, LiveWork, QueueStatus, Request, RunOutcome,
    SandboxInfo, SessionOption, SessionState, Shell, TodoRollup, Workspace,
};

/// What every client knows about a session. Sent whole on every change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionSummary {
    /// The session.
    pub id: SessionId,
    /// The user-set name, if any.
    pub name: Option<String>,
    /// The agent-reported title, if any. Clients show `name`, else `title`, else the primary
    /// workspace.
    pub title: Option<String>,
    /// Its workspaces, the primary one first.
    pub workspaces: Vec<Workspace>,
    /// Its named shells, in creation order.
    pub shells: Vec<Shell>,
    /// When it was created.
    pub created_at: UnixMillis,
    /// When anything in the summary last changed.
    pub updated_at: UnixMillis,
    /// When it was closed, while closed.
    pub closed_at: Option<UnixMillis>,
    /// The parent session, for subagents.
    pub parent_id: Option<SessionId>,
    /// The state.
    pub state: SessionState,
    /// How the last run ended.
    pub last_outcome: Option<RunOutcome>,
    /// The last run's error, when it failed.
    pub last_error: Option<String>,
    /// The shared read cursor: everything up to here has been seen.
    pub read_seq: Seq,
    /// Where the last run ended (any ending: completed, failed, interrupted, reboot).
    pub last_turn_end_seq: Seq,
    /// Whether output after `read_seq` is unread.
    pub unread: bool,
    /// One line of at most 200 characters, plain text: the pending request, else the error,
    /// else the running tool, else the last agent text. Updated at most once a second.
    pub preview: String,
    /// What the session needs from the user, if anything.
    pub attention: Option<Attention>,
    /// What its descendants need.
    pub rollup: AttentionRollup,
    /// Context usage, once reported.
    pub context: Option<ContextUsage>,
    /// Queue state.
    pub queue: QueueStatus,
    /// Pending requests, including descendants' (with `origin_child`).
    pub requests: Vec<Request>,
    /// The todo roll-up; `null` while the list is empty.
    pub todo: Option<TodoRollup>,
    /// Background work that will wake the agent (jobs, children, asks, waiting host calls).
    pub live_work: LiveWork,
    /// Agent options (model, thinking level, ...).
    pub options: Vec<SessionOption>,
    /// A live, ready agent process is attached: options can be changed and the agent's own
    /// requests answered. Clients disable option pickers while it is `false`.
    pub agent_live: bool,
    /// The sandbox (one level for the agent and every workspace).
    pub sandbox: SandboxInfo,
    /// Whether the agent session can be reattached after it stops.
    pub resumable: bool,
}

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
