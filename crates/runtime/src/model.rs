//! The persisted part of a session: what SQLite stores and the transcript can't say.

use std::collections::BTreeMap;

use controlroom_drivers::{LineTag, NativeRef};
use controlroom_protocol::ids::{OpId, QueueId, RemotePath, RequestId, Seq, SessionId, UnixMillis};
use controlroom_protocol::session::{
    AgentSession, AutoContinueStatus, ContentPart, DriverKind, PeerRef, PromptOrigin, QueueItem,
    Request, RequestKind, RunOutcome, SandboxInfo, SandboxLevel, SandboxSpec, StartedBy, Todo,
    TodoStatus,
};
use controlroom_protocol::timeline::{InjectOrigin, RunTrigger};
use serde::{Deserialize, Serialize};

/// How a queue item was handed to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Via {
    /// As a prompt that starts a run.
    Prompt,
    /// Into the running run.
    Steer,
}

/// Where a queue item is on its way to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Delivery {
    /// Handed to the supervisor; not yet in the transcript.
    Handed(Via),
    /// In the transcript; waiting for the agent's answer.
    Written(Via),
}

impl Delivery {
    pub(crate) fn via(self) -> Via {
        match self {
            Delivery::Handed(via) | Delivery::Written(via) => via,
        }
    }
}

/// One queued prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QueueEntry {
    pub id: QueueId,
    pub content: Vec<ContentPart>,
    pub origin: PromptOrigin,
    /// The command that queued it.
    pub op_id: OpId,
    pub queued_at: UnixMillis,
    /// A direct `session.prompt` to an idle agent: tagged with its op id (the run's trigger is
    /// the user, not the queue).
    pub direct: bool,
    /// Goes ahead of everything and ignores the queue's pause and `auto_send`.
    pub send_now: bool,
    /// "Interrupt now": cancel without waiting for the open tool.
    pub interrupt: bool,
    /// The agent refused a steer; deliver as a prompt after a cancel instead.
    pub no_steer: bool,
    pub delivery: Option<Delivery>,
}

impl QueueEntry {
    /// The tag of the line that delivers it.
    pub(crate) fn tag(&self) -> LineTag {
        if self.direct {
            LineTag::Op {
                op_id: self.op_id.clone(),
                part: 0,
            }
        } else {
            LineTag::Queue {
                queue_id: self.id,
                part: 0,
            }
        }
    }

    /// Whether `tag` delivers this item.
    pub(crate) fn has_tag(&self, tag: &LineTag) -> bool {
        match tag {
            LineTag::Op { op_id, .. } => self.direct && self.op_id == *op_id,
            LineTag::Queue { queue_id, .. } => !self.direct && self.id == *queue_id,
            LineTag::Internal { .. } => false,
        }
    }

    pub(crate) fn to_item(&self) -> QueueItem {
        QueueItem {
            id: self.id,
            content: self.content.clone(),
            origin: self.origin.clone(),
            sending: self.delivery.is_some(),
            queued_at: self.queued_at,
        }
    }
}

/// A recorded resolution of a request: the first decision wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Decision {
    /// The request with its final state and answer.
    pub request: Request,
    /// The deciding command; `None` for requests made moot by a cancel.
    pub op_id: Option<OpId>,
}

impl Decision {
    /// The answer line to write, if this decision needs one.
    pub(crate) fn answer_tag(&self) -> Option<LineTag> {
        let op_id = self.op_id.clone()?;
        self.request.answer.as_ref()?;
        Some(LineTag::Op { op_id, part: 0 })
    }
}

/// A request the daemon raised for the session itself (e.g. a sandbox approval), outside the
/// agent's transcript. Its resolution goes back to whoever raised it, never to the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Raised {
    /// The request with its current state and answer.
    pub request: Request,
    /// The transcript head when it was raised; its timeline item sits there.
    pub seq: Seq,
}

impl Raised {
    /// A question the agent asked (`ask_user`): non-blocking, nobody waits for it, and its
    /// answer is injected into the agent's next turn. Everything else is an approval.
    pub(crate) fn is_question(&self) -> bool {
        self.request.kind == RequestKind::Question
    }
}

/// One agent session, with the native file needed to resume it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentSessionRow {
    pub session: AgentSession,
    pub native_path: Option<String>,
}

impl AgentSessionRow {
    pub(crate) fn native(&self) -> NativeRef {
        NativeRef {
            id: self.session.native_id.clone(),
            path: self.native_path.clone(),
        }
    }
}

/// Everything persisted about a session. Commands and reactions mutate it; the transaction
/// diffs it against its snapshot and saves what changed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionData {
    pub id: SessionId,
    pub agent: String,
    pub driver: DriverKind,
    pub cwd: RemotePath,
    pub name: Option<String>,
    pub created_at: UnixMillis,
    pub closed_at: Option<UnixMillis>,
    pub parent_id: Option<SessionId>,
    pub spawned_by: Option<PeerRef>,
    /// The explicit level, if the user chose one.
    pub sandbox_level: Option<SandboxLevel>,
    /// The defaults from the controller.
    pub sandbox: SandboxSpec,
    pub read_seq: Seq,
    pub last_turn_end_seq: Seq,
    pub last_outcome: Option<RunOutcome>,
    pub last_error: Option<String>,
    pub queue_paused: bool,
    pub auto_send: bool,
    /// How the next native session starts; set when a start is requested, cleared once the
    /// agent reports its native session (or the start fails).
    pub pending_start: Option<StartedBy>,
    /// A history export the next prompt points at.
    pub history_note: Option<RemotePath>,
    /// A resume failed: the agent session can't be reattached.
    pub resume_failed: bool,
    /// Reactions to records up to here are persisted; a re-fold only observes them.
    pub applied_seq: Seq,
    /// The materialized timeline reflects records up to here.
    pub timeline_seq: Seq,
    pub agent_sessions: Vec<AgentSessionRow>,
    pub queue: Vec<QueueEntry>,
    pub decisions: BTreeMap<RequestId, Decision>,
    /// Requests the daemon raised, pending or resolved.
    pub raised: BTreeMap<RequestId, Raised>,
    /// The shared todo list, in display order.
    pub todos: Vec<Todo>,
    /// Items waiting to be delivered into the agent's next turn, in order.
    pub injections: Vec<Injection>,
    pub auto: AutoContinue,
    /// Triggers of the newest run-starting lines, oldest first.
    pub triggers: Vec<TriggerEntry>,
}

impl SessionData {
    /// The level the agent runs at.
    pub(crate) fn sandbox_level(&self) -> SandboxLevel {
        self.sandbox_level.unwrap_or(self.sandbox.level)
    }

    pub(crate) fn sandbox_info(&self) -> SandboxInfo {
        let level = self.sandbox_level();
        SandboxInfo {
            level,
            default_level: self.sandbox.level,
            explicit: self.sandbox_level.is_some(),
            looser_than_default: level.is_looser_than(self.sandbox.level),
            grants: Vec::new(),
        }
    }

    /// Queue items not yet handed to the agent, in order, with their indices.
    pub(crate) fn waiting(&self) -> impl Iterator<Item = (usize, &QueueEntry)> {
        self.queue
            .iter()
            .enumerate()
            .filter(|(_, e)| e.delivery.is_none())
    }

    /// A queue item is on its way to the agent.
    pub(crate) fn delivering(&self) -> bool {
        self.queue.iter().any(|e| e.delivery.is_some())
    }

    /// Where an item jumping the queue goes: after the items already being delivered.
    pub(crate) fn front(&self) -> usize {
        self.queue
            .iter()
            .position(|e| e.delivery.is_none())
            .unwrap_or(self.queue.len())
    }

    /// The current agent session, if any started.
    pub(crate) fn current_agent_session(&mut self) -> Option<&mut AgentSessionRow> {
        self.agent_sessions.last_mut()
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed_at.is_some()
    }
}

/// Key prefix of an injection's line tag: `inject:<injection id>`.
pub(crate) const INJECT: &str = "inject:";

/// An item delivered into the agent's next turn (spec §8, Injection): the single path for
/// answers to questions, user edits of the todo list and auto-continue (later also job wakes,
/// child results and messages). While a run is active it is steered in; while the agent is
/// idle a waking item starts a run (its trigger names the origin) and a non-waking one waits
/// for the next run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Injection {
    /// A ULID; the line tag is `inject:<id>`.
    pub id: String,
    pub origin: InjectOrigin,
    /// Shown as the run's trigger, e.g. "answer: Which database?".
    pub label: String,
    pub content: Vec<ContentPart>,
    /// Starts a run when the agent is idle.
    pub wake: bool,
    pub created_at: UnixMillis,
    /// The agent refused it as a steer: deliver it as a prompt once the run ends.
    pub no_steer: bool,
    /// Times the agent refused it; it is dropped after [`Injection::MAX_ATTEMPTS`].
    pub attempts: u32,
    pub delivery: Option<Delivery>,
}

impl Injection {
    /// Refusals before an injection is dropped.
    pub(crate) const MAX_ATTEMPTS: u32 = 3;

    pub(crate) fn new(
        origin: InjectOrigin,
        label: String,
        text: String,
        wake: bool,
        now: UnixMillis,
    ) -> Self {
        Injection {
            id: ulid::Ulid::generate().to_string(),
            origin,
            label,
            content: vec![ContentPart::text(text)],
            wake,
            created_at: now,
            no_steer: false,
            attempts: 0,
            delivery: None,
        }
    }

    /// The tag of the line that delivers it.
    pub(crate) fn tag(&self) -> LineTag {
        LineTag::Internal {
            key: format!("{INJECT}{}", self.id),
            part: 0,
        }
    }

    /// The run trigger when it starts a run.
    pub(crate) fn trigger(&self) -> RunTrigger {
        match self.origin {
            InjectOrigin::Continue => RunTrigger::AutoContinue,
            origin => RunTrigger::Inject {
                origin,
                label: self.label.clone(),
            },
        }
    }
}

/// Why auto-continue paused (spec §8). Each pause comes with attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Pause {
    /// Only blocked entries remain, or the user cancelled a run.
    Paused,
    /// Runs made no progress.
    Stalled,
    /// The run budget is used up.
    Budget,
}

/// What asks auto-continue to decide whether to start a continue run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Due {
    /// A run settled (continues only after a completed one).
    Settled,
    /// The user turned auto-continue on or resumed it.
    Resumed,
}

/// The per-session auto-continue state (spec §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutoContinue {
    pub enabled: bool,
    /// Continue runs allowed per user instruction.
    pub budget: u32,
    /// Continue runs started since the user last started a run.
    pub runs_used: u32,
    /// Consecutive continue runs that closed no todo entry.
    pub stalls: u32,
    pub pause: Option<(Pause, String)>,
    /// A decision is pending (made by the reconcile loop once the agent is idle).
    pub due: Option<Due>,
    /// The active run was started by auto-continue.
    pub continuing: bool,
    /// A todo entry was closed (done, blocked or skipped) during the active run.
    pub progress: bool,
}

impl AutoContinue {
    /// The default budget of a new session.
    pub(crate) const DEFAULT_BUDGET: u32 = 10;
    /// Continue runs without progress before auto-continue stalls.
    pub(crate) const STALL_RUNS: u32 = 3;

    pub(crate) fn status(&self) -> AutoContinueStatus {
        match (self.enabled, &self.pause) {
            (false, _) => AutoContinueStatus::Off,
            (true, None) => AutoContinueStatus::On,
            (true, Some((Pause::Paused, _))) => AutoContinueStatus::Paused,
            (true, Some((Pause::Stalled, _))) => AutoContinueStatus::Stalled,
            (true, Some((Pause::Budget, _))) => AutoContinueStatus::Budget,
        }
    }
}

impl Default for AutoContinue {
    fn default() -> Self {
        AutoContinue {
            enabled: true,
            budget: Self::DEFAULT_BUDGET,
            runs_used: 0,
            stalls: 0,
            pause: None,
            due: None,
            continuing: false,
            progress: false,
        }
    }
}

/// The trigger of a run-starting line the runtime wrote, by its tag. The driver only knows the
/// tag's kind; the runtime puts the real trigger (device, schedule, injection origin) on the
/// run item. Persisted, so a re-fold after a restart shows the same trigger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TriggerEntry {
    pub tag: String,
    pub trigger: RunTrigger,
}

/// Run triggers kept per session: the newest few (only runs not yet settled and checkpointed
/// can need them).
pub(crate) const TRIGGERS_KEPT: usize = 32;

/// A stable string for a tag (part 0 lines start runs).
pub(crate) fn tag_key(tag: &LineTag) -> String {
    match tag {
        LineTag::Op { op_id, .. } => format!("op:{op_id}"),
        LineTag::Queue { queue_id, .. } => format!("queue:{queue_id}"),
        LineTag::Internal { key, .. } => format!("internal:{key}"),
    }
}

impl SessionData {
    /// The trigger recorded for a run-starting line.
    pub(crate) fn trigger_for(&self, tag: &LineTag) -> Option<&RunTrigger> {
        let key = tag_key(tag);
        self.triggers
            .iter()
            .rev()
            .find(|e| e.tag == key)
            .map(|e| &e.trigger)
    }

    /// Records the trigger of a run-starting line (keeping the newest few).
    pub(crate) fn record_trigger(&mut self, tag: &LineTag, trigger: RunTrigger) {
        self.triggers.push(TriggerEntry {
            tag: tag_key(tag),
            trigger,
        });
        let excess = self.triggers.len().saturating_sub(TRIGGERS_KEPT);
        self.triggers.drain(..excess);
    }

    /// Todo entries still to do: pending or in progress.
    pub(crate) fn open_todos(&self) -> usize {
        self.todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
            .count()
    }
}
