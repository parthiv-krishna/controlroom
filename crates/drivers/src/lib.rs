//! Agent drivers: the only code that understands an agent's wire format (spec §6.2–§6.4).
//!
//! A driver is a **pure, deterministic state machine** over one session's transcript:
//!
//! - [`Driver::fold`] consumes transcript records in seq order and produces timeline ops (§7)
//!   plus [`DriverEvent`]s for the runtime. Re-folding the same records from scratch must give
//!   the same ops, item ids and request ids (the runtime rebuilds state this way after a
//!   restart). No clock, no IO, no randomness: timestamps come from the records.
//! - [`Driver::view`] is the agent-neutral snapshot the runtime's reconcile loop decides on.
//! - [`Driver::encode`] turns an [`Intent`] into the lines to write to the agent's stdin. Every
//!   line carries its [`LineTag`] in the agent's own correlation field, so the fold can tell,
//!   from the transcript alone, which op or queue item a line and the agent's answer belong to
//!   (exactly-once after a crash: the runtime never re-sends a tag the transcript already has).
//! - [`Driver::launch_args`] gives the agent command's arguments for a start or a resume.
//!
//! The runtime owns everything else: the queue, op dedup, in-doubt handling, persistence,
//! send-now policy timing, injection origins, auto-continue.
//!
//! Implementations: `pi` (native `pi --mode rpc`, P3) and `fake` (in-process, for runtime unit
//! tests). This module is the contract between the drivers and the runtime; change it only with
//! a reason recorded in the workstream report.

use controlroom_protocol::daemon::OptionValue;
use controlroom_protocol::ids::{OpId, QueueId, RequestId};
use controlroom_protocol::session::{
    ContentPart, ContextUsage, DriverKind, Request, Response, RunOutcome, SessionOption,
};
use controlroom_protocol::timeline::{InjectOrigin, TimelineOp};
use controlroom_protocol::{Seq, UnixMillis};
use controlroom_transcript::Record;

/// Identifies what a written line belongs to. Embedded in the line (for pi: the RPC `id`) and
/// echoed in the agent's response, so both sides are visible in the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LineTag {
    /// A user command (`session.prompt`, `request.respond`, `session.set_option`, ...).
    Op {
        /// The command's op id.
        op_id: OpId,
        /// Index of the line within the intent (an intent may encode to several lines).
        part: u16,
    },
    /// A queued prompt being delivered (normally or as a send-now).
    Queue {
        /// The queue item.
        queue_id: QueueId,
        /// Index of the line within the intent.
        part: u16,
    },
    /// A daemon-internal line (state queries, auto-continue, injections). `key` is
    /// deterministic for the cause (e.g. `continue:<run item id>`), never random.
    Internal {
        /// The deterministic key.
        key: String,
        /// Index of the line within the intent.
        part: u16,
    },
}

/// What the runtime wants the agent to do. Content is neutral; the driver translates it.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Start a new run with this prompt (the agent must be idle).
    Prompt {
        /// The prompt.
        content: Vec<ContentPart>,
    },
    /// Deliver into the running turn as soon as possible (pi: `steer`). Used for send-now and
    /// for injections while busy.
    Steer {
        /// The text.
        content: Vec<ContentPart>,
    },
    /// Deliver after the current run settles, without a new user turn in between
    /// (pi: `follow_up`).
    FollowUp {
        /// The text.
        content: Vec<ContentPart>,
    },
    /// An injected item (job wake, child result, message, answer, todo edit, continuation).
    /// The driver picks prompt/steer/follow-up from [`AgentView::run_active`].
    Inject {
        /// Where it came from.
        origin: InjectOrigin,
        /// The text delivered to the agent.
        content: Vec<ContentPart>,
    },
    /// Stop the current run (pi: `abort`; the driver also cancels any open dialog, since pi's
    /// abort waits for it).
    Cancel,
    /// Answer an agent request (dialog, permission) the fold reported.
    Answer {
        /// The request.
        request: RequestId,
        /// The answer.
        response: Response,
    },
    /// Change an agent option reported in [`AgentView::options`].
    SetOption {
        /// The option id.
        id: String,
        /// The new value.
        value: OptionValue,
    },
    /// Start a fresh agent session in the same process when supported (pi: `new_session`).
    NewSession {
        /// Link the previous native session as the parent.
        parent: Option<String>,
    },
    /// Ask the agent for its current state (pi: `get_state`), e.g. after a reattach.
    Refresh,
}

/// How a start relates to earlier agent sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartMode {
    /// A new native session with this id (pi: `--session-id`).
    New {
        /// The native session id to create.
        native_id: String,
    },
    /// Reattach to an earlier native session (pi: `--session <file>`).
    Resume {
        /// What [`DriverEvent::NativeSession`] reported for it.
        native: NativeRef,
    },
}

/// The agent's own handle on a session, enough to resume it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeRef {
    /// The agent's session id.
    pub id: String,
    /// Where the agent stores it (pi: the absolute session file), when it has a file.
    pub path: Option<String>,
}

/// What the runtime learns from folding a record, besides timeline ops.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// The agent process is up and answering (e.g. its first response arrived).
    Ready,
    /// The agent reported its native session.
    NativeSession(NativeRef),
    /// A run began. `cause` is the tag of the line that started it, when known.
    RunStarted {
        /// The line that caused it.
        cause: Option<LineTag>,
    },
    /// The run ended (pi: `agent_settled`). Retries inside one run don't end it.
    RunSettled {
        /// How it ended.
        outcome: RunOutcome,
        /// The error text, for failures.
        error: Option<String>,
    },
    /// The agent accepted a tagged line.
    Accepted {
        /// The line.
        tag: LineTag,
    },
    /// The agent rejected a tagged line (e.g. "already processing").
    Rejected {
        /// The line.
        tag: LineTag,
        /// The agent's error.
        error: String,
    },
    /// The agent raised a request (dialog, permission). Its id is deterministic.
    RequestOpened(Box<Request>),
    /// A request ended without an answer from us (agent-side timeout or withdrawal).
    RequestClosed {
        /// The request.
        id: RequestId,
    },
    /// Context usage changed.
    Context(ContextUsage),
    /// The agent's options (model, thinking level, ...) changed.
    Options(Vec<SessionOption>),
    /// The agent's session title changed.
    Title(String),
    /// The agent process ended (from the supervisor's exit record).
    Exited {
        /// Exit code, if it exited normally.
        code: Option<i32>,
        /// Signal number, if it was killed.
        signal: Option<i32>,
        /// Whether a run was in flight (the session becomes `interrupted`).
        during_run: bool,
    },
}

/// The result of folding one record.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FoldOut {
    /// Timeline changes, in order.
    pub ops: Vec<TimelineOp>,
    /// Runtime-relevant events, in order.
    pub events: Vec<DriverEvent>,
}

/// What the driver can do with its agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Native mid-run steering (send-now without cancelling).
    pub steer: bool,
    /// Native delivery after the run settles.
    pub follow_up: bool,
    /// Resume an earlier native session after the process died.
    pub resume: bool,
    /// Start a new native session in the running process.
    pub new_session: bool,
}

/// The agent-neutral snapshot the runtime's reconcile loop decides on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentView {
    /// The process is up and answering.
    pub ready: bool,
    /// A run is active (pi: from `agent_start` to `agent_settled`, across retries).
    pub run_active: bool,
    /// Tool calls currently running (for send-now "wait for the tool" decisions).
    pub open_tools: usize,
    /// Requests awaiting an answer.
    pub open_requests: Vec<RequestId>,
    /// How the last run ended.
    pub last_outcome: Option<RunOutcome>,
    /// The current native session.
    pub native: Option<NativeRef>,
    /// Current agent options.
    pub options: Vec<SessionOption>,
    /// Tags written but not yet accepted or rejected by the agent.
    pub unanswered: Vec<LineTag>,
    /// The seq of the last record folded.
    pub last_seq: Option<Seq>,
    /// The ts of the last record folded.
    pub last_ts: Option<UnixMillis>,
    /// What the driver supports.
    pub capabilities: Capabilities,
}

/// Why an intent can't be encoded right now.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The agent isn't in a state that accepts it (e.g. `Prompt` while a run is active).
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// The driver can't do this at all.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Unknown request, option or value.
    #[error("invalid: {0}")]
    Invalid(String),
}

/// One agent driver for one session (see the module docs).
pub trait Driver: Send {
    /// Which driver this is.
    fn kind(&self) -> DriverKind;

    /// Folds the next transcript record (seq order, every `dir`). Must ignore non-JSON and
    /// truncated `out` lines (login-shell noise) without failing.
    fn fold(&mut self, record: &Record) -> FoldOut;

    /// The current snapshot.
    fn view(&self) -> &AgentView;

    /// Encodes an intent into stdin lines, each carrying `tag` (with `part` 0, 1, ...).
    fn encode(&mut self, intent: &Intent, tag: &LineTag) -> Result<Vec<String>, EncodeError>;

    /// Arguments appended to the agent command for this start.
    fn launch_args(&self, mode: &StartMode) -> Vec<String>;
}
