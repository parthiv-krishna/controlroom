//! The session summary: what every client knows about every session, fed by the agent
//! daemon's `sessions.subscribe`. Attention and roll-ups are computed by the agent daemon;
//! clients derive nothing.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{Seq, SessionId, UnixMillis};
use crate::session::{AutoContinueStatus, Request, SandboxInfo, Shell, Workspace};
use crate::timeline::TokenUsage;

/// A session's state. `working` means a run is active, whoever started it (user, queue, steer,
/// job completion, message, auto-continue).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The agent is being launched or resumed.
    Starting,
    /// The agent is up and no run is active.
    Idle,
    /// A run is active.
    Working,
    /// A blocking approval is pending.
    AwaitingPermission,
    /// A blocking dialog is waiting for input.
    AwaitingInput,
    /// The agent failed to start or crashed.
    Failed,
    /// No agent process; the session resumes lazily on the next prompt.
    Dormant,
    /// The agent died mid-run (e.g. a reboot); resume to continue.
    Interrupted,
    /// Closed ("archived"); reopen to use again.
    Closed,
    /// A state this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// How a run ended. Neutral across agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// The agent finished its turn.
    Completed,
    /// Cancelled by the user.
    Cancelled,
    /// Failed (provider error, retries exhausted, agent crash).
    Failed,
    /// Stopped at the output token limit.
    MaxTokens,
    /// The model refused.
    Refusal,
    /// Ended for a reason the driver couldn't classify (also the fallback for unknown values).
    #[serde(other)]
    Unknown,
}

/// What a session needs from the user, most urgent first. Computed by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// A blocking approval or dialog is pending (push).
    Approval,
    /// A non-blocking question is open (the lighter tier; push only if opted in).
    Question,
    /// The last run failed or was interrupted, or auto-continue paused with attention.
    Problem,
    /// A run finished and its output is unread.
    Unread,
    /// An attention kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Attention counts over a session's descendants (child sessions on the same machine), so a
/// collapsed parent can show what its subtree needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AttentionRollup {
    /// Descendants needing an approval.
    pub approvals: u32,
    /// Descendants with an open question.
    pub questions: u32,
    /// Descendants with a problem.
    pub problems: u32,
    /// Descendants with unread output.
    pub unread: u32,
}

/// The todo roll-up shown in lists and the session header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TodoRollup {
    /// Entries done.
    pub done: u32,
    /// All entries except skipped ones.
    pub total: u32,
    /// Auto-continue status.
    pub auto_continue: AutoContinueStatus,
}

/// Work that continues without the agent running, and that will wake it later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LiveWork {
    /// Running background jobs.
    pub jobs: u32,
    /// Running child sessions.
    pub children: u32,
    /// Outstanding mailbox asks awaiting a reply.
    pub asks: u32,
    /// Tool calls waiting for an unreachable host, or past the grace period with their late
    /// result still to come.
    #[serde(default)]
    pub host_calls: u32,
}

/// Context window usage, from the agent's usage reports.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
pub struct ContextUsage {
    /// Tokens in the context now.
    pub used: u64,
    /// The context window size, when known.
    pub size: Option<u64>,
    /// The session's cost so far in USD, when the provider reports it.
    pub cost: Option<f64>,
}

/// A session's cumulative token usage and cost: every model call its agents reported (replies,
/// tools' own model calls, compactions and other summaries), across agent processes and native
/// sessions (a `start_fresh` keeps counting).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct SessionUsage {
    /// Token totals.
    pub tokens: TokenUsage,
    /// Total cost in USD; 0 while no provider reported one.
    pub cost: f64,
    /// The latest agent reply's cache hit rate, from 0 to 1: its cache reads over its prompt
    /// tokens (input, cache reads and cache writes). `null` before the first reply and when the
    /// latest reply reported no prompt tokens.
    pub latest_cache_hit: Option<f64>,
}

/// The queue's state in the summary; the items themselves are in the session detail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QueueStatus {
    /// Queued items, including one being handed over (`sending`).
    pub len: u32,
    /// The queue stopped advancing (e.g. after a cancel); `queue.resume` restarts it.
    pub paused: bool,
    /// Whether the queue advances automatically when a run ends.
    pub auto_send: bool,
    /// A send-now is waiting for the run to stop at a safe point.
    pub send_now_pending: bool,
}

/// One choice of a select option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OptionChoice {
    /// The value sent with `session.set_option`.
    pub value: String,
    /// The display label.
    pub label: String,
}

/// The control and current value of a session option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OptionControl {
    /// Pick one of `choices`.
    Select {
        /// The current choice's value.
        value: String,
        /// The available choices.
        choices: Vec<OptionChoice>,
    },
    /// On or off.
    Bool {
        /// The current value.
        value: bool,
    },
    /// A control this build doesn't know; show it read-only.
    #[serde(other)]
    Unknown,
}

/// An agent setting the user can change, such as the model or thinking level. Neutral: the
/// driver maps it to the agent's own setters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SessionOption {
    /// Sent with `session.set_option`, e.g. `"model"`.
    pub id: String,
    /// Display label, e.g. `"Model"`.
    pub label: String,
    /// Its control and current value.
    #[serde(flatten)]
    pub control: OptionControl,
}

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
    /// Cumulative token usage and cost (zero until the first report).
    #[serde(default)]
    pub usage: SessionUsage,
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
