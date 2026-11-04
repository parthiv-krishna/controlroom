//! Session lifecycle, session state and queue commands. Every mutating command carries an
//! `op_id`; repeating one returns the original result.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{OpId, QueueId, RemotePath, Seq, SessionId};
use crate::session::{ContentPart, PromptOrigin, SandboxSpec};

/// Params of commands that only name a session: `session.cancel`, `session.close`,
/// `session.reopen`, `session.delete`, `session.mark_unread`, `queue.resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionOpParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
}

/// The first prompt of `session.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct InitialPrompt {
    /// The prompt.
    pub content: Vec<ContentPart>,
    /// Where it comes from; default: the user.
    #[serde(default)]
    pub origin: PromptOrigin,
}

/// What `session.prompt` does when a run is active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum IfBusy {
    /// Append to the queue (the default).
    #[default]
    Queue,
    /// Drop the prompt (schedules with `if_busy: skip`).
    Skip,
    /// A value this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Params of `session.prompt`: send now when idle, otherwise queue it. A `dormant` session is
/// resumed first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionPromptParams {
    /// Idempotency key; also recorded with the queue item.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The prompt.
    pub content: Vec<ContentPart>,
    /// Where it comes from; default: the user.
    #[serde(default)]
    pub origin: PromptOrigin,
    /// What to do when a run is active; default `queue`.
    #[serde(default)]
    pub if_busy: IfBusy,
}

/// Result of `session.prompt` (and of the `then` prompt of `request.respond`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PromptResult {
    /// Sent to the agent, starting a run.
    Sent,
    /// Delivered into the active run as a steer.
    Steered,
    /// Appended to the queue.
    Queued {
        /// The new queue item.
        queue_id: QueueId,
    },
    /// Dropped because a run was active (`if_busy: skip`).
    Skipped,
    /// An outcome this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Params of `session.resume`: reattach a `dormant` or `interrupted` session's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionResumeParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// Refreshed sandbox defaults to restart with; absent keeps the current ones. An explicit
    /// level stays in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
}

/// Params of `session.start_fresh`: a new agent session in the same controlroom session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionStartFreshParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// Export the previous conversation to a file and point the next prompt at it.
    pub include_history: bool,
    /// Refreshed sandbox defaults to start with; absent keeps the current ones. An explicit
    /// level stays in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxSpec>,
}

/// Result of `session.start_fresh`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StartFreshResult {
    /// Where the previous conversation was exported, with `include_history`.
    pub history_path: Option<RemotePath>,
}

/// Params of `session.update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionUpdateParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The user-set name; `""` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether the queue advances automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_send: Option<bool>,
    /// Turn auto-continue on or off. Turning it on also resumes a paused auto-continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_continue: Option<bool>,
    /// The auto-continue run budget; raising it resumes a `budget`-paused auto-continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_continue_budget: Option<u32>,
}

/// Params of `session.mark_read`. The cursor only moves forward, so a stale client can't
/// un-read anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionMarkReadParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// Everything up to and including this seq has been read.
    pub seq: Seq,
}

/// Result of `session.mark_read` and `session.mark_unread`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ReadCursor {
    /// The read cursor after the command.
    pub read_seq: Seq,
    /// The unread flag after the command.
    pub unread: bool,
}

/// A new value for a session option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum OptionValue {
    /// For a `select` option: one choice's value.
    Choice(String),
    /// For a `bool` option.
    Bool(bool),
}

/// Params of `session.set_option`: change an agent option such as the model. Works in any live
/// state, including during a run. Errors: `not_found` (`option`) for an unknown option,
/// `invalid_params` for a value the option doesn't allow, `invalid_state` without a live agent
/// (see [`crate::session::SessionSummary::agent_live`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionSetOptionParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The [`crate::session::SessionOption::id`].
    pub option_id: String,
    /// The new value.
    pub value: OptionValue,
}

/// Params of `queue.send_now`: deliver a queued item ahead of everything else. While a run is
/// active the item is steered in when the agent can take it; otherwise the run is cancelled at
/// a safe point (no tool running) and the item goes next. Calling it again for the same item
/// with `interrupt` escalates a send-now that is waiting for a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QueueSendNowParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The item.
    pub queue_id: QueueId,
    /// "Interrupt now": cancel the active run at once, even while a tool is running, and send
    /// the item as the next prompt (never as a steer). Default `false`.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub interrupt: bool,
}

/// Result of `queue.send_now`. How it is delivered is the driver's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SendNowResult {
    /// The agent was idle: sent, starting a run.
    Sent,
    /// Delivered into the active run as a steer.
    Steered,
    /// The run is being cancelled (at a safe point, or at once with `interrupt`); the item
    /// goes next (`queue.send_now_pending` in the summary while it waits for the safe point).
    Interrupting,
    /// The agent isn't ready (starting, reconnecting, or stopped and now starting): the item
    /// is first in the queue and goes as soon as the agent is ready.
    Queued,
    /// An outcome this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Params of `queue.edit`: replace a queued item's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QueueEditParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The item.
    pub queue_id: QueueId,
    /// The new content.
    pub content: Vec<ContentPart>,
}

/// Params of `queue.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QueueRemoveParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The item.
    pub queue_id: QueueId,
}

/// Params of `queue.reorder`: move one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QueueReorderParams {
    /// Idempotency key.
    pub op_id: OpId,
    /// The session.
    pub id: SessionId,
    /// The item to move.
    pub queue_id: QueueId,
    /// Its new 0-based index, clamped to the queue length.
    pub to_index: u32,
}
