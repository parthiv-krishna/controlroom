//! One session in memory: its persisted [`SessionData`], its driver, and the state the runtime
//! keeps beside them.
//!
//! - [`Live`] is runtime bookkeeping (the supervisor attachment, in-doubt, lines handed over);
//!   a transaction snapshots it with the data and restores both on failure.
//! - [`Derived`] is observed from the fold only and rebuilt by a re-fold.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use controlroom_drivers::{AgentView, Driver, FoldOut, LineTag};
use controlroom_protocol::agent::{AgentState, SessionInfo};
use controlroom_protocol::host::{ToolOutputParams, ToolWaitingInputParams};
use controlroom_protocol::ids::{HostOpId, ItemId, OpId, RequestId, Seq, UnixMillis};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::session::{
    AgentSession, Attention, AutoContinueStatus, ContextUsage, LiveWork, Request, RequestKind,
    RequestState, SessionDetail, SessionState,
};
use controlroom_protocol::timeline::{RawFrame, RawStream, RunTrigger, TimelineOp};
use controlroom_transcript::{Body, Record};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::AbortHandle;

use crate::hosts::{DoneCall, Machines, OpenCall};
use crate::model::SessionData;
use crate::ports::SupervisorConn;
use crate::preview::{PreviewSource, one_line};
use crate::timeline::TimelineCache;

/// Live updates buffered per session feed before a slow subscriber lags.
const UPDATES_CAPACITY: usize = 1024;
/// Host events buffered per session before a slow extension connection lags.
const EVENTS_CAPACITY: usize = 256;

/// Progress of a running host call, for the agent's extension.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// New output of a `bash` call (`host.output`).
    Output(ToolOutputParams),
    /// A `bash` call started or stopped waiting for terminal input (`host.waiting_input`).
    WaitingInput(ToolWaitingInputParams),
}

/// A change to one session's feed (`session.events`): timeline ops of one record, its raw
/// frame, and the detail when it changed.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionUpdate {
    /// Everything up to this seq is reflected.
    pub head_seq: Seq,
    /// Timeline ops, in order.
    pub ops: Vec<TimelineOp>,
    /// The record as a raw frame (subscribers that asked for raw frames forward it).
    pub raw: Option<RawFrame>,
    /// The new detail, when it changed.
    pub detail: Option<SessionDetail>,
}

/// The connection to the session's running supervisor.
#[derive(Clone)]
pub(crate) struct Attachment {
    /// Distinguishes this attachment's reader from earlier ones.
    pub generation: u64,
    /// Lines for the writer task, in order.
    pub writer: mpsc::Sender<String>,
    pub conn: Arc<dyn SupervisorConn>,
}

/// After an attach or a failed write, the runtime doesn't know what reached the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Doubt {
    /// The barrier's durable seq; `None` while the barrier is pending.
    pub until: Option<Seq>,
    /// Caused by a failed write (the items it carried pause the queue).
    pub write_failed: bool,
}

/// Runtime bookkeeping, snapshotted with the data by transactions.
#[derive(Clone, Default)]
pub(crate) struct Live {
    pub attachment: Option<Attachment>,
    /// A launch task is running.
    pub launching: bool,
    /// The session is being (re)built from the store and its transcript.
    pub loading: bool,
    /// The last launch failed before the agent produced anything.
    pub launch_error: Option<String>,
    /// No writes until this clears.
    pub doubt: Option<Doubt>,
    /// The last record folded.
    pub processed_seq: Seq,
    /// Launch again (with `pending_start`) once the current process is gone.
    pub relaunch: bool,
    /// The idle-stop asked the agent to stop.
    pub stopping: bool,
    /// A `session.cancel` waiting to be written.
    pub cancel_requested: Option<OpId>,
    /// A cancel for the current run is on its way or written.
    pub cancel_sent: bool,
    /// Answer, cancel and refresh lines handed over but not yet seen in the transcript.
    pub handed: HashSet<LineTag>,
    /// Answer, cancel and refresh lines seen in the transcript of the current process.
    pub written: HashSet<LineTag>,
    /// Host requests the session waits for, by request id.
    pub calls: BTreeMap<HostOpId, OpenCall>,
    /// Finished tool calls whose results pi hasn't logged yet (then they are acknowledged).
    pub done: Vec<DoneCall>,
}

impl Live {
    /// The line was handed over or written: never send it again.
    pub(crate) fn sent(&self, tag: &LineTag) -> bool {
        self.handed.contains(tag) || self.written.contains(tag)
    }
}

/// How the agent process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ended {
    pub during_run: bool,
    pub before_ready: bool,
    /// For the preview, e.g. "the agent exited with code 101".
    pub error: String,
}

/// The agent process, as seen in the transcript.
#[derive(Debug, Clone, Default)]
pub(crate) struct Process {
    /// The `meta` record that started it.
    pub meta_seq: Option<Seq>,
    /// Started and not ended.
    pub live: bool,
    /// It answered at least once.
    pub ready: bool,
    pub ended: Option<Ended>,
}

/// State observed from the fold; rebuilt by a re-fold.
#[derive(Debug, Clone, Default)]
pub(crate) struct Derived {
    /// Every request the agent raised, as raised (state updated on close).
    pub requests: BTreeMap<RequestId, Request>,
    pub context: Option<ContextUsage>,
    pub title: Option<String>,
    pub process: Process,
    pub preview: PreviewSource,
    /// Run items with the trigger the runtime recorded for them (see `triggers`).
    pub run_triggers: HashMap<ItemId, RunTrigger>,
}

/// What the machine and session feeds last saw.
#[derive(Debug, Clone, Default)]
pub(crate) struct Emitted {
    pub summary: Option<SessionSummary>,
    pub detail: Option<SessionDetail>,
    pub at: UnixMillis,
    /// A throttled preview change is waiting.
    pub preview_dirty: bool,
}

/// One session in memory.
pub(crate) struct Session {
    pub data: SessionData,
    pub live: Live,
    pub derived: Derived,
    driver: Box<dyn Driver>,
    /// The driver or derived state changed in the current transaction: on failure, rebuild.
    pub needs_reload_on_failure: bool,
    pub timeline: TimelineCache,
    pub updates: broadcast::Sender<SessionUpdate>,
    /// What the agent's extension sees; changed only after a commit.
    pub agent: watch::Sender<Option<AgentState>>,
    /// Where the session's tools run, for the extension; changed only after a commit.
    pub info: watch::Sender<Option<SessionInfo>>,
    /// Progress of the session's running host calls, for the extension.
    pub events: broadcast::Sender<AgentEvent>,
    /// The machines as last reported (names and availability of the workspaces).
    pub machines: Arc<Machines>,
    pub emitted: Emitted,
    /// The attachment's tasks (reader, writer, barrier).
    pub tasks: Vec<AbortHandle>,
}

impl Session {
    pub(crate) fn new(data: SessionData, driver: Box<dyn Driver>, machines: Arc<Machines>) -> Self {
        Session {
            data,
            live: Live::default(),
            derived: Derived::default(),
            driver,
            needs_reload_on_failure: false,
            timeline: TimelineCache::default(),
            updates: broadcast::channel(UPDATES_CAPACITY).0,
            agent: watch::channel(None).0,
            info: watch::channel(None).0,
            events: broadcast::channel(EVENTS_CAPACITY).0,
            machines,
            emitted: Emitted::default(),
            tasks: Vec::new(),
        }
    }

    pub(crate) fn view(&self) -> &AgentView {
        self.driver.view()
    }

    pub(crate) fn fold(&mut self, record: &Record) -> FoldOut {
        self.needs_reload_on_failure = true;
        self.driver.fold(record)
    }

    pub(crate) fn driver_mut(&mut self) -> &mut dyn Driver {
        self.needs_reload_on_failure = true;
        self.driver.as_mut()
    }

    pub(crate) fn driver(&self) -> &dyn Driver {
        self.driver.as_ref()
    }

    /// Stops the attachment's tasks.
    pub(crate) fn abort_tasks(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }

    /// An agent process is running or coming up.
    pub(crate) fn has_process(&self) -> bool {
        self.live.launching
            || self.live.loading
            || self.live.relaunch
            || (self.live.attachment.is_some() && self.derived.process.live)
    }

    /// Lines may be written now.
    pub(crate) fn writable(&self) -> bool {
        !self.data.closed()
            && !self.live.loading
            && self.live.doubt.is_none()
            && self.live.attachment.is_some()
            && self.derived.process.live
    }

    /// A live, ready agent is attached: it can take options and answers (`agent_live`).
    pub(crate) fn agent_live(&self) -> bool {
        !self.data.closed()
            && !self.live.loading
            && self.live.attachment.is_some()
            && self.derived.process.live
            && self.view().ready
    }

    /// A run is active or a prompt (a queue item or an injection) is on its way.
    pub(crate) fn busy(&self) -> bool {
        self.view().run_active
            || self.data.delivering()
            || crate::inject::prompt_on_its_way(&self.data)
    }

    /// The agent session can be reattached after its process stops.
    pub(crate) fn resumable(&self) -> bool {
        self.view().capabilities.resume
            && !self.data.agent_sessions.is_empty()
            && !self.data.resume_failed
    }

    /// Pending requests: the agent's (raised, still open in the agent, and undecided), then
    /// the daemon's own.
    pub(crate) fn pending_requests(&self) -> Vec<Request> {
        let mut pending = self.pending_agent_requests();
        pending.extend(self.pending_raised().cloned());
        pending
    }

    /// The agent's own pending requests (raised in the transcript, still open, undecided).
    pub(crate) fn pending_agent_requests(&self) -> Vec<Request> {
        let mut pending = Vec::new();
        if self.derived.process.live {
            let open = &self.view().open_requests;
            pending.extend(
                self.derived
                    .requests
                    .values()
                    .filter(|r| {
                        r.state == RequestState::Pending
                            && open.contains(&r.id)
                            && !self.data.decisions.contains_key(&r.id)
                    })
                    .cloned(),
            );
        }
        pending
    }

    /// Pending requests the daemon raised.
    pub(crate) fn pending_raised(&self) -> impl Iterator<Item = &Request> {
        self.data
            .raised
            .values()
            .map(|r| &r.request)
            .filter(|r| r.state == RequestState::Pending)
    }

    pub(crate) fn state(&self) -> SessionState {
        if self.data.closed() {
            return SessionState::Closed;
        }
        let restarting = self.live.relaunch && !self.derived.process.live;
        if self.live.loading || self.live.launching || restarting {
            return SessionState::Starting;
        }
        if self.live.attachment.is_some() && self.derived.process.live {
            if !self.view().ready {
                return SessionState::Starting;
            }
            let pending = self.pending_requests();
            if let Some(blocking) = pending.iter().find(|r| r.blocking) {
                return match blocking.kind {
                    RequestKind::Dialog => SessionState::AwaitingInput,
                    _ => SessionState::AwaitingPermission,
                };
            }
            if self.busy() {
                return SessionState::Working;
            }
            if self.data.last_outcome == Some(controlroom_protocol::session::RunOutcome::Failed) {
                return SessionState::Failed;
            }
            return SessionState::Idle;
        }
        let ended = self.derived.process.ended.as_ref();
        if self.live.launch_error.is_some() || ended.is_some_and(|e| e.before_ready) {
            return SessionState::Failed;
        }
        if ended.is_some_and(|e| e.during_run) {
            return SessionState::Interrupted;
        }
        SessionState::Dormant
    }

    /// What the session itself needs from the user.
    pub(crate) fn attention(&self, state: SessionState) -> Option<Attention> {
        let pending = self.pending_requests();
        if pending.iter().any(|r| r.blocking) {
            Some(Attention::Approval)
        } else if !pending.is_empty() {
            Some(Attention::Question)
        } else if matches!(state, SessionState::Failed | SessionState::Interrupted)
            || self.auto_continue_paused()
        {
            Some(Attention::Problem)
        } else if self.unread() {
            Some(Attention::Unread)
        } else {
            None
        }
    }

    /// Auto-continue paused with attention (only blocked entries, stalled, budget, cancel).
    pub(crate) fn auto_continue_paused(&self) -> bool {
        !matches!(
            self.data.auto.status(),
            AutoContinueStatus::On | AutoContinueStatus::Off
        )
    }

    /// Work that continues without the agent running and will wake it (jobs, children,
    /// mailbox asks, host calls waiting for an unreachable host or a late result). Each feature
    /// adds its count here, and auto-continue waits while any is non-zero ("not continuing:
    /// waiting on job").
    pub(crate) fn live_work(&self) -> LiveWork {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let jobs = self.data.jobs.values().filter(|j| j.running()).count();
        let waiting = self
            .live
            .calls
            .values()
            .filter(|c| !self.machines.connected(&c.machine))
            .count();
        LiveWork {
            jobs: count(jobs),
            host_calls: count(waiting + self.data.late.len()),
            ..LiveWork::default()
        }
    }

    /// The first running job, for "waiting on job <name>" (with "(devbox unreachable)" when
    /// its host is away).
    pub(crate) fn running_job(&self) -> Option<String> {
        let job = self.data.jobs.values().find(|j| j.running())?;
        if self.machines.connected(&job.machine_id) {
            return Some(job.name.clone());
        }
        let machine = self.machines.name(&job.machine_id);
        Some(format!("{} ({machine} unreachable)", job.name))
    }

    /// What the agent's extension sees (the `state` notification).
    pub(crate) fn agent_state(&self) -> AgentState {
        AgentState {
            todos: self.data.todos.clone(),
            questions: self
                .pending_raised()
                .filter(|r| r.kind == RequestKind::Question)
                .cloned()
                .collect(),
            auto_continue: crate::auto::info(self),
        }
    }

    pub(crate) fn unread(&self) -> bool {
        self.data.last_turn_end_seq > self.data.read_seq
    }

    /// The preview: pending request, else error, else running tool, else last agent text.
    pub(crate) fn preview(&self, state: SessionState) -> String {
        if let Some(request) = self.pending_requests().first() {
            return one_line(&request.title);
        }
        if matches!(state, SessionState::Failed | SessionState::Interrupted) {
            let error = self
                .live
                .launch_error
                .clone()
                .or_else(|| self.data.last_error.clone())
                .or_else(|| self.derived.process.ended.as_ref().map(|e| e.error.clone()));
            if let Some(error) = error {
                return one_line(&error);
            }
        }
        if self.derived.process.live
            && let Some(tool) = self.derived.preview.running_tool()
        {
            return one_line(tool);
        }
        self.derived
            .preview
            .last_agent_line()
            .map(|line| one_line(&line))
            .unwrap_or_default()
    }

    pub(crate) fn detail(&self) -> SessionDetail {
        SessionDetail {
            agent_sessions: self
                .data
                .agent_sessions
                .iter()
                .map(|row| row.session.clone())
                .collect::<Vec<AgentSession>>(),
            queue: self.data.queue.iter().map(|e| e.to_item()).collect(),
            todos: self.data.todos.clone(),
            auto_continue: crate::auto::info(self),
            jobs: Vec::new(),
            peers: Vec::new(),
        }
    }
}

/// A record as an opaque raw frame.
pub(crate) fn raw_frame(record: &Record) -> RawFrame {
    let (stream, text) = match &record.body {
        Body::In(line) => (RawStream::ToAgent, line.raw.clone()),
        Body::Out(line) => (RawStream::FromAgent, line.raw.clone()),
        Body::Stderr(line) => (RawStream::Stderr, line.raw.clone()),
        Body::Exit(_) | Body::Meta(_) | Body::Unknown(_) => (
            RawStream::Meta,
            serde_json::to_string(record).unwrap_or_default(),
        ),
    };
    RawFrame {
        seq: Seq(record.seq),
        ts: UnixMillis(i64::try_from(record.ts).unwrap_or(i64::MAX)),
        stream,
        text,
    }
}
