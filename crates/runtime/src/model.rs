//! The persisted part of a session: what SQLite stores and the transcript can't say.

use std::collections::BTreeMap;

use controlroom_drivers::{LineTag, NativeRef};
use controlroom_protocol::ids::{OpId, QueueId, RemotePath, RequestId, Seq, SessionId, UnixMillis};
use controlroom_protocol::session::{
    AgentSession, ContentPart, DriverKind, PeerRef, PromptOrigin, QueueItem, Request, RunOutcome,
    SandboxInfo, SandboxLevel, SandboxSpec, StartedBy,
};
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
