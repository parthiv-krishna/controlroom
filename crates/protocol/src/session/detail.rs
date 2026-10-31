//! The session detail: what a client watching one session needs beyond the summary, delivered
//! by `session.subscribe` and replaced whole whenever it changes.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{QueueId, UnixMillis};
use crate::session::{AutoContinueInfo, ContentPart, Job, PromptOrigin, Todo};

/// The driver that folds an agent's raw stdio into the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    /// pi's native RPC mode.
    Pi,
    /// The deterministic in-process test driver; never seen in production.
    Fake,
    /// A driver this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// How an agent session was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StartedBy {
    /// With the controlroom session.
    New,
    /// Reattached to an earlier agent session.
    Resume,
    /// "Start fresh": a new agent session in the same controlroom session.
    Fresh,
    /// A value this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One agent-native session within a controlroom session (one per start, resume or start
/// fresh).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AgentSession {
    /// The driver.
    pub driver: DriverKind,
    /// The agent's own session id (opaque).
    pub native_id: String,
    /// When it started.
    pub started_at: UnixMillis,
    /// When it ended, if it did.
    pub ended_at: Option<UnixMillis>,
    /// How it started.
    pub started_by: StartedBy,
}

/// One queued prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QueueItem {
    /// The item.
    pub id: QueueId,
    /// The prompt.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    pub origin: PromptOrigin,
    /// Being handed to the agent right now. The item stays listed (and the session `working`)
    /// until the agent echoes it, so there is no idle flicker.
    pub sending: bool,
    /// When it was queued.
    pub queued_at: UnixMillis,
}

/// The per-session detail behind the todo panel, queue, jobs strip and history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionDetail {
    /// Agent sessions, oldest first.
    pub agent_sessions: Vec<AgentSession>,
    /// The queue, in send order.
    pub queue: Vec<QueueItem>,
    /// The todo list, in display order.
    pub todos: Vec<Todo>,
    /// Auto-continue details.
    pub auto_continue: AutoContinueInfo,
    /// Background jobs, newest first: running ones and recently finished ones.
    pub jobs: Vec<Job>,
}
