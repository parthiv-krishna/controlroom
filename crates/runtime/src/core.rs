//! The runtime's state behind one lock, and its transactions.
//!
//! Every entry point (a command, a record from a supervisor, a tick) runs as one
//! [`Core::transact`]: one SQLite transaction plus a snapshot of each session it touches. On
//! success the transaction diffs every touched session against its snapshot and saves what
//! changed, appends machine-feed changes to the rev log, commits, and only then hands staged
//! lines to the supervisors and broadcasts. On failure (the command's error or a failed commit)
//! it rolls back, restores the snapshots and discards the staged lines and broadcasts; a session
//! whose driver already folded or encoded something is rebuilt from the store and its
//! transcript ([`Task::Reload`]). Host requests a transaction sends or withdraws are staged
//! the same way ([`HostEffect`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use controlroom_protocol::RpcError;
use controlroom_protocol::Seq;
use controlroom_protocol::agent::{AgentState, SessionInfo};
use controlroom_protocol::agentd::{DaemonSettings, HostOpRequest, SessionsChange, SessionsEvent};
use controlroom_protocol::host::HostRequest;
use controlroom_protocol::ids::{CallId, DaemonId, HostOpId, MachineId, Rev, SessionId};
use controlroom_protocol::session::{AttentionRollup, SessionDetail, SessionSummary};
use tokio::sync::{broadcast, watch};

use crate::errors::{internal, unknown_session};
use crate::hosts::{Bridge, HostSync, LateCall, Machines};
use crate::model::SessionData;
use crate::ports::{Agents, Clock, SupervisorConn};
use crate::session::{Live, Session, SessionUpdate};
use crate::store::Store;

/// Sessions-feed changes buffered before a slow subscriber lags.
const SESSIONS_CAPACITY: usize = 4096;
/// Preview-only summary changes are sent at most this often.
pub(crate) const PREVIEW_THROTTLE_MS: i64 = 1000;

/// Async work a transaction asks for, started after the lock is released.
pub(crate) enum Task {
    /// Launch the session's agent (with its `pending_start`).
    Launch(SessionId),
    /// Stop an agent.
    Terminate(Arc<dyn SupervisorConn>, Duration),
    /// Rebuild the session from the store and its transcript, then reattach.
    Reload(SessionId),
    /// Stop the agent, if one is attached, then delete the session's files.
    Remove(SessionId, Option<Arc<dyn SupervisorConn>>),
    /// Run a barrier for an attachment that is in doubt.
    Barrier {
        id: SessionId,
        generation: u64,
        conn: Arc<dyn SupervisorConn>,
    },
    /// Renew a host's subscriptions after a new connection.
    Sync(MachineId),
    /// Fetch a late call's result once its host is back.
    Attach(SessionId, LateCall),
    /// Open the session's shells its hosts don't have yet.
    OpenShells(SessionId),
    /// Reopen the session's workspaces (its sandbox changed) and shells.
    Refresh(SessionId),
}

/// A host request change a transaction asks for, applied after the commit.
pub(crate) enum HostEffect {
    /// Queue a request nobody waits for.
    Send(HostOpRequest),
    /// Drop a pending request.
    Withdraw(HostOpId),
    /// Fail an open call's waiters.
    Expire(watch::Sender<Option<RpcError>>, RpcError),
    /// Fail the pending `workspace.open`s of a machine that dropped (nobody would answer them
    /// before it is back).
    FailOpens(MachineId, RpcError),
}

/// What a transaction does once it has committed.
#[derive(Default)]
struct Effects {
    lines: Vec<(SessionId, String)>,
    feed: Vec<SessionsChange>,
    updates: Vec<(SessionId, SessionUpdate)>,
    /// What the feeds will have seen: (session, new summary, new detail).
    emitted: Vec<(SessionId, Option<SessionSummary>, Option<SessionDetail>)>,
    /// New agent states and session infos, for the agents' extensions.
    agent: Vec<(SessionId, AgentState)>,
    info: Vec<(SessionId, SessionInfo)>,
    host: Vec<HostEffect>,
    tasks: Vec<Task>,
}

/// Undo information for the running transaction.
#[derive(Default)]
struct TxState {
    snapshots: HashMap<SessionId, (SessionData, Live)>,
    created: Vec<SessionId>,
    removed: Vec<Session>,
    settings: Option<DaemonSettings>,
    rev: Option<Rev>,
    /// Sessions whose summary may have changed.
    touched: BTreeSet<SessionId>,
    /// Write the timeline cache of these sessions.
    checkpoint: BTreeSet<SessionId>,
    fx: Effects,
}

/// The runtime's state.
pub(crate) struct Core {
    pub store: Store,
    pub sessions: BTreeMap<SessionId, Session>,
    pub settings: DaemonSettings,
    pub daemon_id: DaemonId,
    pub rev: Rev,
    pub sessions_tx: broadcast::Sender<SessionsChange>,
    pub clock: Arc<dyn Clock>,
    pub agents: Arc<dyn Agents>,
    /// The controller's machines, as last reported.
    pub machines: Arc<Machines>,
    /// Pending host requests (outside the lock; staged by transactions).
    pub bridge: Arc<Bridge>,
    /// Per host: results to release with `journal.ack`.
    pub acks: BTreeMap<MachineId, Vec<CallId>>,
    /// Per host: the subscriptions of the current connection.
    pub sync: BTreeMap<MachineId, HostSync>,
    next_generation: u64,
    tx: TxState,
}

impl Core {
    pub(crate) fn new(
        store: Store,
        settings: DaemonSettings,
        daemon_id: DaemonId,
        rev: Rev,
        clock: Arc<dyn Clock>,
        agents: Arc<dyn Agents>,
        bridge: Arc<Bridge>,
    ) -> Self {
        Core {
            store,
            sessions: BTreeMap::new(),
            settings,
            daemon_id,
            rev,
            sessions_tx: broadcast::channel(SESSIONS_CAPACITY).0,
            clock,
            agents,
            machines: Arc::new(Machines::default()),
            bridge,
            acks: BTreeMap::new(),
            sync: BTreeMap::new(),
            next_generation: 1,
            tx: TxState::default(),
        }
    }

    /// Runs `f` as one transaction; returns its result and the tasks to start.
    pub(crate) fn transact<R>(
        &mut self,
        f: impl FnOnce(&mut Core) -> Result<R, RpcError>,
    ) -> (Result<R, RpcError>, Vec<Task>) {
        self.tx = TxState::default();
        if let Err(e) = self.store.begin() {
            return (Err(internal(e)), Vec::new());
        }
        let result = f(self).and_then(|r| {
            self.finish().map_err(internal)?;
            self.store.commit().map_err(internal)?;
            Ok(r)
        });
        let tasks = match &result {
            Ok(_) => self.dispatch(),
            Err(e) => {
                tracing::debug!("transaction rolled back: {e}");
                self.abort()
            }
        };
        for session in self.sessions.values_mut() {
            session.needs_reload_on_failure = false;
        }
        (result, tasks)
    }

    // -----------------------------------------------------------------------------------------
    // Access inside a transaction
    // -----------------------------------------------------------------------------------------

    /// The session; `deleted` if retention purged it, else `unknown_session`.
    pub(crate) fn session(&self, id: SessionId) -> Result<&Session, RpcError> {
        self.sessions.get(&id).ok_or_else(|| self.missing(id))
    }

    fn missing(&self, id: SessionId) -> RpcError {
        if self.store.is_tombstoned(id).unwrap_or(false) {
            return RpcError::new(
                controlroom_protocol::ErrorData::Deleted { id },
                format!("session {id} was purged by the retention policy"),
            );
        }
        unknown_session(id)
    }

    /// The session, snapshotted for rollback and marked for a summary refresh.
    pub(crate) fn session_mut(&mut self, id: SessionId) -> Result<&mut Session, RpcError> {
        if !self.sessions.contains_key(&id) {
            return Err(self.missing(id));
        }
        let session = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| unknown_session(id))?;
        if !self.tx.created.contains(&id) {
            self.tx
                .snapshots
                .entry(id)
                .or_insert_with(|| (session.data.clone(), session.live.clone()));
        }
        self.tx.touched.insert(id);
        Ok(session)
    }

    /// Adds a new session (saved whole at commit).
    pub(crate) fn insert_session(&mut self, session: Session) {
        let id = session.data.id;
        self.tx.created.push(id);
        self.tx.touched.insert(id);
        self.sessions.insert(id, session);
    }

    /// Removes a session from memory (the caller deletes its rows).
    pub(crate) fn remove_session(&mut self, id: SessionId) -> Option<()> {
        let mut session = self.sessions.remove(&id)?;
        session.abort_tasks();
        self.tx.touched.remove(&id);
        self.tx.removed.push(session);
        Some(())
    }

    pub(crate) fn settings_mut(&mut self) -> &mut DaemonSettings {
        self.tx
            .settings
            .get_or_insert_with(|| self.settings.clone());
        &mut self.settings
    }

    /// Stages a line for the session's supervisor.
    pub(crate) fn stage_line(&mut self, id: SessionId, line: String) {
        self.tx.fx.lines.push((id, line));
    }

    pub(crate) fn task(&mut self, task: Task) {
        self.tx.fx.tasks.push(task);
    }

    /// Stages a host request change.
    pub(crate) fn effect(&mut self, effect: HostEffect) {
        self.tx.fx.host.push(effect);
    }

    /// Publishes one record's timeline ops and raw frame.
    pub(crate) fn publish(&mut self, id: SessionId, update: SessionUpdate) {
        self.tx.fx.updates.push((id, update));
    }

    pub(crate) fn checkpoint(&mut self, id: SessionId) {
        self.tx.checkpoint.insert(id);
    }

    pub(crate) fn touch(&mut self, id: SessionId) {
        self.tx.touched.insert(id);
    }

    pub(crate) fn next_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    /// Appends a sessions-feed change to the rev log.
    pub(crate) fn emit(&mut self, event: SessionsEvent) -> rusqlite::Result<()> {
        self.tx.rev.get_or_insert(self.rev);
        self.rev = Rev(self.rev.0 + 1);
        let change = SessionsChange {
            rev: self.rev,
            event,
        };
        self.store.append_rev(self.rev, &change)?;
        self.tx.fx.feed.push(change);
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Commit and rollback
    // -----------------------------------------------------------------------------------------

    fn finish(&mut self) -> rusqlite::Result<()> {
        let checkpoints = std::mem::take(&mut self.tx.checkpoint);
        for id in checkpoints {
            let Some(session) = self.sessions.get_mut(&id) else {
                continue;
            };
            if session.live.loading {
                continue;
            }
            // The flush empties the cache: if the commit fails, rebuild it by a re-fold.
            session.needs_reload_on_failure = true;
            session.timeline.flush(&self.store, id)?;
            if session.data.timeline_seq != session.live.processed_seq {
                self.tx
                    .snapshots
                    .entry(id)
                    .or_insert_with(|| (session.data.clone(), session.live.clone()));
                session.data.timeline_seq = session.live.processed_seq;
            }
        }
        for id in &self.tx.created {
            if let Some(session) = self.sessions.get(id) {
                self.store.save_session(None, &session.data)?;
            }
        }
        for (id, (before, _)) in &self.tx.snapshots {
            if let Some(session) = self.sessions.get(id)
                && session.data != *before
            {
                self.store.save_session(Some(before), &session.data)?;
            }
        }
        if let Some(before) = &self.tx.settings
            && *before != self.settings
        {
            self.store.kv_set("settings", &self.settings)?;
            self.emit(SessionsEvent::SettingsChanged {
                settings: self.settings.clone(),
            })?;
        }
        self.emit_summaries()
    }

    /// Emits the summaries (and details) of touched sessions and their ancestors.
    fn emit_summaries(&mut self) -> rusqlite::Result<()> {
        let mut ids = BTreeSet::new();
        for id in std::mem::take(&mut self.tx.touched) {
            let mut next = Some(id);
            while let Some(id) = next {
                if !ids.insert(id) {
                    break;
                }
                next = self.sessions.get(&id).and_then(|s| s.data.parent_id);
            }
        }
        for id in ids {
            self.emit_session(id, false)?;
        }
        Ok(())
    }

    /// Emits the session's summary if it changed; preview-only changes are throttled unless
    /// `force`.
    pub(crate) fn emit_session(&mut self, id: SessionId, force: bool) -> rusqlite::Result<()> {
        let now = self.clock.now();
        let Some(session) = self.sessions.get(&id) else {
            return Ok(());
        };
        if session.live.loading {
            return Ok(());
        }
        let mut summary = self.summary(id);
        let emitted = &session.emitted;
        let changed = match &emitted.summary {
            None => true,
            Some(last) if *last == summary => false,
            Some(last) => {
                let mut without_preview = summary.clone();
                without_preview.preview.clone_from(&last.preview);
                without_preview.context = last.context;
                without_preview.usage = last.usage;
                force || without_preview != *last || now.0 - emitted.at.0 >= PREVIEW_THROTTLE_MS
            }
        };
        let agent = session.agent_state();
        if session.agent.borrow().as_ref() != Some(&agent) {
            self.tx.fx.agent.push((id, agent));
        }
        let info = session.info();
        if session.info.borrow().as_ref() != Some(&info) {
            self.tx.fx.info.push((id, info));
        }
        let detail = self.detail(session);
        let detail_changed = emitted.detail.as_ref() != Some(&detail);
        let head = session.live.processed_seq;
        let preview_pending = !changed && emitted.summary.as_ref() != Some(&summary);
        if let Some(session) = self.sessions.get_mut(&id) {
            session.emitted.preview_dirty = preview_pending;
        }
        if detail_changed {
            match self.tx.fx.updates.iter_mut().rev().find(|(s, _)| *s == id) {
                Some((_, update)) => update.detail = Some(detail.clone()),
                None => self.tx.fx.updates.push((
                    id,
                    SessionUpdate {
                        head_seq: head,
                        ops: Vec::new(),
                        raw: None,
                        detail: Some(detail.clone()),
                    },
                )),
            }
        }
        if changed {
            summary.updated_at = now;
            self.emit(SessionsEvent::SessionUpserted {
                session: Box::new(summary.clone()),
            })?;
        }
        if changed || detail_changed {
            self.tx.fx.emitted.push((
                id,
                changed.then_some(summary),
                detail_changed.then_some(detail),
            ));
        }
        Ok(())
    }

    /// Hands lines over and broadcasts; returns the tasks to start.
    fn dispatch(&mut self) -> Vec<Task> {
        let fx = std::mem::take(&mut self.tx.fx);
        let now = self.clock.now();
        let mut tasks = fx.tasks;
        for (id, line) in fx.lines {
            let Some(session) = self.sessions.get_mut(&id) else {
                continue;
            };
            let Some(attachment) = &session.live.attachment else {
                continue;
            };
            if attachment.writer.try_send(line).is_err() && session.live.doubt.is_none() {
                tracing::warn!("session {id}: the writer is gone or full; reconnecting");
                session.live.doubt = Some(crate::session::Doubt {
                    until: None,
                    write_failed: true,
                });
                tasks.push(Task::Barrier {
                    id,
                    generation: attachment.generation,
                    conn: attachment.conn.clone(),
                });
            }
        }
        for change in fx.feed {
            let _ = self.sessions_tx.send(change);
        }
        for effect in fx.host {
            match effect {
                HostEffect::Send(request) => self.bridge.send(request),
                HostEffect::Withdraw(id) => self.bridge.withdraw(&id),
                HostEffect::Expire(expire, error) => {
                    expire.send_replace(Some(error));
                }
                HostEffect::FailOpens(machine, error) => self.bridge.fail(
                    |r| {
                        r.machine_id == machine
                            && matches!(r.request, HostRequest::WorkspaceOpen(_))
                    },
                    &error,
                ),
            }
        }
        for (id, update) in fx.updates {
            if let Some(session) = self.sessions.get(&id) {
                let _ = session.updates.send(update);
            }
        }
        for (id, state) in fx.agent {
            if let Some(session) = self.sessions.get(&id) {
                session.agent.send_replace(Some(state));
            }
        }
        for (id, info) in fx.info {
            if let Some(session) = self.sessions.get(&id) {
                session.info.send_replace(Some(info));
            }
        }
        for (id, summary, detail) in fx.emitted {
            let Some(session) = self.sessions.get_mut(&id) else {
                continue;
            };
            if let Some(summary) = summary {
                session.emitted.summary = Some(summary);
                session.emitted.at = now;
            }
            if let Some(detail) = detail {
                session.emitted.detail = Some(detail);
            }
        }
        self.tx = TxState::default();
        tasks
    }

    fn abort(&mut self) -> Vec<Task> {
        self.store.rollback();
        let tx = std::mem::take(&mut self.tx);
        for id in tx.created {
            if let Some(mut session) = self.sessions.remove(&id) {
                session.abort_tasks();
            }
        }
        for session in tx.removed {
            self.sessions.insert(session.data.id, session);
        }
        let mut tasks = Vec::new();
        for (id, (data, live)) in tx.snapshots {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.data = data;
                session.live = live;
            }
        }
        for (id, session) in &self.sessions {
            if session.needs_reload_on_failure {
                tasks.push(Task::Reload(*id));
            }
        }
        if let Some(settings) = tx.settings {
            self.settings = settings;
        }
        if let Some(rev) = tx.rev {
            self.rev = rev;
        }
        tasks
    }

    // -----------------------------------------------------------------------------------------
    // Summaries
    // -----------------------------------------------------------------------------------------

    /// The session's summary, with roll-ups over its descendants.
    pub(crate) fn summary(&self, id: SessionId) -> SessionSummary {
        let session = &self.sessions[&id];
        let data = &session.data;
        let state = session.state();
        let mut rollup = AttentionRollup::default();
        let mut requests = session.pending_requests();
        for other in self.sessions.values() {
            if !self.is_descendant(other.data.id, id) {
                continue;
            }
            use controlroom_protocol::session::Attention;
            match other.attention(other.state()) {
                Some(Attention::Approval) => rollup.approvals += 1,
                Some(Attention::Question) => rollup.questions += 1,
                Some(Attention::Problem) => rollup.problems += 1,
                Some(Attention::Unread) => rollup.unread += 1,
                Some(Attention::Unknown) | None => {}
            }
            requests.extend(other.pending_requests().into_iter().map(|mut r| {
                r.origin_child = Some(other.data.id);
                r
            }));
        }
        let view = session.view();
        let send_now_pending = data.waiting().next().is_some_and(|(_, head)| {
            head.send_now
                && view.run_active
                && !session.live.cancel_sent
                && !(view.capabilities.steer && !head.no_steer)
        });
        SessionSummary {
            id,
            name: data.name.clone(),
            title: session.derived.title.clone(),
            workspaces: session.workspaces(),
            shells: data.shells.iter().map(|s| s.shell.clone()).collect(),
            created_at: data.created_at,
            updated_at: session
                .emitted
                .summary
                .as_ref()
                .map_or(data.created_at, |s| s.updated_at),
            closed_at: data.closed_at,
            parent_id: data.parent_id,
            state,
            last_outcome: data.last_outcome,
            last_error: data.last_error.clone(),
            read_seq: data.read_seq,
            last_turn_end_seq: data.last_turn_end_seq,
            unread: session.unread(),
            preview: session.preview(state),
            attention: session.attention(state),
            rollup,
            context: session.derived.context,
            usage: session.derived.usage,
            queue: controlroom_protocol::session::QueueStatus {
                len: u32::try_from(data.queue.len()).unwrap_or(u32::MAX),
                paused: data.queue_paused,
                auto_send: data.auto_send,
                send_now_pending,
            },
            requests,
            todo: crate::todos::rollup(data),
            live_work: session.live_work(),
            options: view.options.clone(),
            agent_live: session.agent_live(),
            sandbox: data.sandbox_info(),
            resumable: session.resumable(),
        }
    }

    /// Whether `id` is a strict descendant of `ancestor`.
    fn is_descendant(&self, id: SessionId, ancestor: SessionId) -> bool {
        let mut next = self.sessions.get(&id).and_then(|s| s.data.parent_id);
        let mut hops = 0;
        while let Some(parent) = next {
            if parent == ancestor {
                return true;
            }
            hops += 1;
            if hops > self.sessions.len() {
                return false;
            }
            next = self.sessions.get(&parent).and_then(|s| s.data.parent_id);
        }
        false
    }

    /// Writes the session's timeline cache now (before serving it from the store).
    pub(crate) fn flush_timeline(&mut self, id: SessionId) -> Result<(), RpcError> {
        self.session_mut(id)?;
        let Some(session) = self.sessions.get_mut(&id) else {
            return Ok(());
        };
        if session.live.loading {
            return Ok(());
        }
        session.needs_reload_on_failure = true;
        session.timeline.flush(&self.store, id).map_err(internal)?;
        session.data.timeline_seq = session.live.processed_seq;
        Ok(())
    }

    /// The seq the session's feed has reached.
    pub(crate) fn head(&self, id: SessionId) -> Seq {
        self.sessions
            .get(&id)
            .map_or(Seq(0), |s| s.live.processed_seq)
    }
}
