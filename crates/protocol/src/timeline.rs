//! The timeline: the agent-neutral, materialized view of a session's transcript, streamed by
//! `session.subscribe`.
//!
//! Items have stable ids and are ordered by `seq_from`. Three operations change them:
//!
//! - [`TimelineOp::Upsert`] inserts an item or replaces it whole;
//! - [`TimelineOp::AppendText`] appends streamed text: to a message's last text part (adding one
//!   if needed), to a thinking item's text, or to a tool call's text output;
//! - [`TimelineOp::End`] marks a streaming item finished.
//!
//! Every item and op carries the transcript range `seq_from..=seq_to` it was folded from, so the
//! raw view can link back. Raw transcript frames ([`RawFrame`]) are only sent to subscribers that
//! ask for them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;

use crate::ids::{ItemId, MessageId, ScheduleId, Seq, SessionId, UnixMillis};
use crate::session::{
    ContentPart, ContextUsage, DeviceRef, Job, PeerRef, Request, RunOutcome, SessionState, Todo,
};

/// Who an item belongs to: the session itself or one of its child sessions (rolled up into the
/// parent's timeline). On the wire `"root"` or the child's session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Actor {
    /// The session's own agent.
    Root,
    /// A child session on the same machine.
    Child(SessionId),
}

impl Serialize for Actor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Actor::Root => serializer.serialize_str("root"),
            Actor::Child(id) => id.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == "root" {
            return Ok(Actor::Root);
        }
        raw.parse()
            .map(Actor::Child)
            .map_err(|_| serde::de::Error::custom(format!("invalid actor {raw:?}")))
    }
}

/// Where an injected item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum InjectOrigin {
    /// A user edit delivered into the next turn.
    User,
    /// A job's exit or follow wake.
    Job,
    /// A child session's result.
    Child,
    /// A mailbox message.
    Message,
    /// The answer to a non-blocking question.
    Answer,
    /// A todo change made by the user.
    Todo,
    /// A continuation.
    Continue,
    /// The late result of a tool call that failed with `connection_lost` (its host came back).
    ToolLate,
    /// A notice about a workspace: its host unreachable past the grace period, a workspace
    /// added or removed, a shell reset.
    Workspace,
    /// An origin this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What started a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunTrigger {
    /// A prompt sent directly by the user.
    User {
        /// The sending device.
        device: Option<DeviceRef>,
    },
    /// The next queued prompt.
    Queue {
        /// The device that queued it.
        device: Option<DeviceRef>,
    },
    /// A send-now delivered as a steer.
    Steer {
        /// The sending device.
        device: Option<DeviceRef>,
    },
    /// An injected item (job wake, child result, message, answer, ...).
    Inject {
        /// Where it came from.
        origin: InjectOrigin,
        /// A short label, e.g. `"job: tests (exit 0)"`.
        label: String,
    },
    /// Auto-continue, because open todo entries remain.
    AutoContinue,
    /// The agent's automatic retry.
    Retry,
    /// Started by an agent extension.
    Extension {
        /// The extension's name.
        name: String,
    },
    /// A controller schedule.
    Schedule {
        /// The schedule.
        schedule_id: ScheduleId,
        /// Its name, shown as `schedule: <name>`.
        name: String,
    },
    /// A trigger this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Whether a run is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Active.
    Running,
    /// Ended; see the outcome.
    Settled,
    /// A state this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Who a message is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// The user's prompt (including injected items).
    User,
    /// The agent.
    Agent,
    /// A notice addressed to the user (e.g. a startup banner).
    Notice,
    /// A role this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A neutral classification of a tool, for its icon and rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reads files.
    Read,
    /// Edits or writes files.
    Edit,
    /// Runs commands.
    Execute,
    /// Searches files or code.
    Search,
    /// Fetches from the network.
    Fetch,
    /// Anything else.
    Other,
    /// A kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A tool call's progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Announced but not started (e.g. waiting for approval).
    Pending,
    /// Running.
    Running,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled or denied.
    Cancelled,
    /// A status this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A tool call's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolOutput {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// A file change.
    Diff {
        /// The file.
        path: String,
        /// The old content.
        old: String,
        /// The new content.
        new: String,
    },
    /// Command output that may contain ANSI escapes; render in a terminal style.
    Terminal {
        /// The output.
        text: String,
    },
    /// An output kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A mailbox message's direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    /// Sent by this session.
    Sent,
    /// Received from the peer.
    Received,
    /// A direction this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What happened to a mailbox operation. Every operation reports one to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MessageOutcome {
    /// Delivered to the peer.
    Delivered,
    /// Held for delivery until its expiry (only with `queue_if_offline`).
    Queued,
    /// The controller or the peer's machine is unreachable; nothing was delivered.
    ConnectionLost,
    /// Refused by the contact policy or the user.
    Denied,
    /// A queued message expired undelivered.
    Expired,
    /// An outcome this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What a usage report covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope {
    /// One model turn.
    Turn,
    /// The whole session so far.
    Session,
    /// A scope this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Token counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TokenUsage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the provider's cache.
    pub cache_read: u64,
    /// Input tokens written to the provider's cache.
    pub cache_write: u64,
}

/// Why context compaction ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// The context crossed the agent's threshold.
    Threshold,
    /// Requested explicitly.
    Manual,
    /// The context overflowed.
    Overflow,
    /// A reason this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The progress of a compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhase {
    /// Compacting (the session is busy).
    Running,
    /// Done.
    Done,
    /// Failed; see the error.
    Failed,
    /// A phase this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The progress of an automatic retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RetryPhase {
    /// Waiting before the next attempt.
    Waiting,
    /// Retrying.
    Retrying,
    /// An attempt succeeded.
    Succeeded,
    /// The agent gave up.
    GaveUp,
    /// A phase this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A notice's severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    /// Informational.
    Info,
    /// A warning.
    Warning,
    /// An error.
    Error,
    /// A level this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The kind-specific content of a timeline item, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    /// One run of the agent, from start to settle.
    Run {
        /// What started it.
        trigger: RunTrigger,
        /// Active or settled.
        state: RunState,
        /// How it ended, once settled.
        outcome: Option<RunOutcome>,
        /// The error, when it failed.
        error: Option<String>,
    },
    /// A message.
    Message {
        /// Who it is from.
        role: MessageRole,
        /// Its content.
        content: Vec<ContentPart>,
    },
    /// The agent's reasoning.
    Thinking {
        /// The reasoning text.
        text: String,
    },
    /// A tool call.
    ToolCall {
        /// The agent's id for the call (opaque).
        tool_call_id: String,
        /// The enclosing tool call, for nested calls.
        parent_tool_call_id: Option<String>,
        /// The tool's name, e.g. `"bash"`.
        name: String,
        /// A one-line title, e.g. `"Run cargo test"`.
        title: String,
        /// Its classification.
        tool_kind: ToolKind,
        /// The input as display text (e.g. the command, or pretty-printed arguments).
        input: Option<String>,
        /// Progress.
        status: ToolStatus,
        /// Output so far.
        output: Option<ToolOutput>,
        /// Exit code, for commands.
        exit: Option<i32>,
        /// How long it took, once finished.
        duration_ms: Option<u64>,
    },
    /// A snapshot of the todo list after a change.
    Todo {
        /// The entries.
        items: Vec<Todo>,
    },
    /// A request and, once resolved, its answer. Nested (not flattened) because the request's
    /// own `id` and `kind` would collide with the item's.
    Request {
        /// The request.
        request: Request,
    },
    /// A child session spawned by this one.
    Subagent {
        /// The child.
        child: PeerRef,
        /// Its title.
        title: String,
        /// Its agent key.
        agent: String,
        /// Its current state.
        state: SessionState,
        /// How its last run ended.
        outcome: Option<RunOutcome>,
        /// Its token usage so far.
        usage: Option<TokenUsage>,
        /// Its reported result, once done.
        result: Option<String>,
    },
    /// A background job. Nested (not flattened) because the job's own `id` would collide with
    /// the item's.
    Job {
        /// The job.
        job: Job,
    },
    /// A mailbox message.
    SessionMessage {
        /// The message.
        message_id: MessageId,
        /// Sent or received.
        direction: MessageDirection,
        /// The other session.
        peer: PeerRef,
        /// The text.
        text: String,
        /// The message this replies to.
        reply_to: Option<MessageId>,
        /// Whether the sender awaits a reply (an ask).
        expects_reply: bool,
        /// What happened to it; `null` while in flight.
        outcome: Option<MessageOutcome>,
        /// A reply that arrived after the ask resolved (e.g. with `connection_lost`).
        late: bool,
    },
    /// A usage report.
    Usage {
        /// What it covers.
        scope: UsageScope,
        /// Token counts.
        tokens: TokenUsage,
        /// Cost in USD, when reported.
        cost: Option<f64>,
        /// Context usage after it.
        context: Option<ContextUsage>,
    },
    /// Context compaction.
    Compaction {
        /// Why it ran.
        reason: CompactionReason,
        /// Progress.
        phase: CompactionPhase,
        /// Context tokens before.
        tokens_before: Option<u64>,
        /// Context tokens after.
        tokens_after: Option<u64>,
        /// The error, when it failed.
        error: Option<String>,
    },
    /// An automatic retry after a provider error.
    Retry {
        /// The attempt number, from 1.
        attempt: u32,
        /// The maximum attempts, when known.
        max_attempts: Option<u32>,
        /// Progress.
        phase: RetryPhase,
        /// The delay before the attempt.
        delay_ms: Option<u64>,
        /// The error that caused it.
        error: Option<String>,
    },
    /// A notice from the agent or the daemon.
    Notice {
        /// Severity.
        level: NoticeLevel,
        /// The text.
        text: String,
    },
    /// An error outside any tool call (e.g. an extension error).
    Error {
        /// The text.
        text: String,
    },
    /// Agent session info changed.
    SessionInfo {
        /// The new title.
        title: Option<String>,
        /// The new model label.
        model: Option<String>,
        /// The new thinking level label.
        thinking: Option<String>,
    },
    /// An item kind this build doesn't know; render a placeholder with a raw-view link.
    #[serde(other)]
    Unknown,
}

/// One timeline item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct TimelineItem {
    /// Stable within the session.
    pub id: ItemId,
    /// The first transcript record it was folded from.
    pub seq_from: Seq,
    /// The last transcript record it was folded from.
    pub seq_to: Seq,
    /// Who it belongs to: `"root"` or a child session id.
    #[ts(type = "\"root\" | string")]
    pub actor: Actor,
    /// When it started.
    pub ts: UnixMillis,
    /// Text is still being appended; cleared by an `end` op.
    pub streaming: bool,
    /// The kind-specific content.
    #[serde(flatten)]
    pub body: ItemBody,
}

/// A change to the timeline, tagged by `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TimelineOp {
    /// Insert the item or replace it whole.
    Upsert {
        /// The item.
        item: Box<TimelineItem>,
    },
    /// Append streamed text to an item and extend its range to `seq_to`.
    AppendText {
        /// The item.
        id: ItemId,
        /// The text to append.
        text: String,
        /// The first source record of this delta.
        seq_from: Seq,
        /// The last source record of this delta.
        seq_to: Seq,
    },
    /// Mark a streaming item finished and extend its range to `seq_to`.
    End {
        /// The item.
        id: ItemId,
        /// The first source record of the end.
        seq_from: Seq,
        /// The last source record of the end.
        seq_to: Seq,
    },
    /// An op this build doesn't know; ignore it.
    #[serde(other)]
    Unknown,
}

/// Which way a raw transcript frame went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RawStream {
    /// Written to the agent's stdin.
    ToAgent,
    /// Read from the agent's stdout.
    FromAgent,
    /// The agent's stderr.
    Stderr,
    /// A supervisor or daemon record (process start and exit, annotations).
    Meta,
    /// A stream this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One raw transcript record, for the opt-in raw view. The text is never interpreted on the
/// wire: it is the agent's line as logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RawFrame {
    /// Its transcript position.
    pub seq: Seq,
    /// When it was logged.
    pub ts: UnixMillis,
    /// Which way it went.
    pub stream: RawStream,
    /// The line, without its newline.
    pub text: String,
}
