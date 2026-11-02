//! Session lifecycle and session state commands.

use std::time::Duration;

use controlroom_drivers::{Intent, LineTag};
use controlroom_protocol::daemon::{
    IfBusy, MachineEvent, OptionValue, PromptResult, ReadCursor, SessionCreateParams,
    SessionMarkReadParams, SessionOpParams, SessionPromptParams, SessionSetOptionParams,
    SessionUpdateParams,
};
use controlroom_protocol::ids::{QueueId, RemotePath, Seq, SessionId};
use controlroom_protocol::session::{
    DriverKind, OptionControl, SandboxLevel, SandboxSpec, SessionState, StartedBy,
};
use controlroom_protocol::{ErrorData, RpcError};
use std::collections::BTreeMap;

use super::validate_content;
use crate::core::{Core, Task};
use crate::errors::{busy, internal, invalid_params, invalid_state, unsupported};
use crate::model::{Delivery, QueueEntry, SessionData, Via};
use crate::session::Session;

/// How long a closing or restarting agent gets before SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(5);

impl Core {
    /// `session.create`, with the agent's driver and the expanded cwd resolved by the caller.
    pub(crate) fn create(
        &mut self,
        params: &SessionCreateParams,
        driver: DriverKind,
        cwd: RemotePath,
    ) -> Result<controlroom_protocol::daemon::SessionCreateResult, RpcError> {
        if let Some(parent) = params.parent_id {
            self.session(parent)?;
        }
        let id = SessionId::generate();
        let data = SessionData {
            id,
            agent: params.agent.clone(),
            driver,
            cwd,
            name: params.name.clone().filter(|n| !n.is_empty()),
            created_at: self.clock.now(),
            closed_at: None,
            parent_id: params.parent_id,
            spawned_by: params.spawned_by.clone(),
            sandbox_level: params.sandbox_level,
            sandbox: params.sandbox.clone().unwrap_or(SandboxSpec {
                level: SandboxLevel::Strict,
                grants: Vec::new(),
                domains: Vec::new(),
                mediated: Vec::new(),
            }),
            read_seq: Seq(0),
            last_turn_end_seq: Seq(0),
            last_outcome: None,
            last_error: None,
            queue_paused: false,
            auto_send: true,
            pending_start: None,
            history_note: None,
            resume_failed: false,
            applied_seq: Seq(0),
            timeline_seq: Seq(0),
            agent_sessions: Vec::new(),
            queue: Vec::new(),
            decisions: BTreeMap::new(),
        };
        let driver = self.agents.driver(driver);
        self.insert_session(Session::new(data, driver));
        self.request_start(id, StartedBy::New)?;
        Ok(controlroom_protocol::daemon::SessionCreateResult {
            session: self.summary(id),
        })
    }

    /// `session.prompt`: send to an idle agent, queue behind a busy one, start a stopped one.
    pub(crate) fn prompt(
        &mut self,
        params: &SessionPromptParams,
    ) -> Result<PromptResult, RpcError> {
        validate_content(&params.content)?;
        let id = params.id;
        let session = self.session(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "session.prompt"));
        }
        let mut entry = QueueEntry {
            id: QueueId::generate(),
            content: params.content.clone(),
            origin: params.origin.clone(),
            op_id: params.op_id.clone(),
            queued_at: self.clock.now(),
            direct: false,
            send_now: false,
            interrupt: false,
            no_steer: false,
            delivery: None,
        };
        let queue_id = entry.id;
        if !session.has_process() {
            let by = start_mode_of(session)?;
            entry.send_now = true;
            self.enqueue(id, entry, true)?;
            self.request_start(id, by)?;
            return Ok(PromptResult::Queued { queue_id });
        }
        if session.busy() {
            if params.if_busy == IfBusy::Skip {
                return Ok(PromptResult::Skipped);
            }
            self.enqueue(id, entry, false)?;
            return Ok(PromptResult::Queued { queue_id });
        }
        // Idle, or still coming up: it goes first.
        entry.direct = session.writable() && session.view().ready;
        entry.send_now = true;
        self.enqueue(id, entry, true)?;
        self.reconcile(id);
        Ok(match self.delivery(id, queue_id) {
            Some(Via::Prompt) => PromptResult::Sent,
            Some(Via::Steer) => PromptResult::Steered,
            None => PromptResult::Queued { queue_id },
        })
    }

    /// Adds a queue item at the back or the front (behind items being delivered).
    pub(crate) fn enqueue(
        &mut self,
        id: SessionId,
        entry: QueueEntry,
        front: bool,
    ) -> Result<(), RpcError> {
        let data = &mut self.session_mut(id)?.data;
        let at = if front {
            data.front()
        } else {
            data.queue.len()
        };
        data.queue.insert(at, entry);
        Ok(())
    }

    /// How a queue item is being delivered, if it is.
    pub(crate) fn delivery(&self, id: SessionId, queue_id: QueueId) -> Option<Via> {
        let session = self.sessions.get(&id)?;
        let entry = session.data.queue.iter().find(|e| e.id == queue_id);
        match entry {
            Some(entry) => entry.delivery.map(Delivery::via),
            // Already accepted and gone (only possible within one transaction for steers).
            None => None,
        }
    }

    /// `session.cancel`: stop the active run; its pending requests are withdrawn.
    pub(crate) fn cancel(&mut self, params: &SessionOpParams) -> Result<(), RpcError> {
        let session = self.session_mut(params.id)?;
        if !session.busy() {
            return Ok(());
        }
        session.live.cancel_requested = Some(params.op_id.clone());
        self.reconcile(params.id);
        Ok(())
    }

    /// `session.close`: stop the agent, keep the history.
    pub(crate) fn close(&mut self, params: &SessionOpParams) -> Result<(), RpcError> {
        let now = self.clock.now();
        let session = self.session_mut(params.id)?;
        if session.data.closed() {
            return Ok(());
        }
        session.data.closed_at = Some(now);
        session.live.relaunch = false;
        if let Some(attachment) = &session.live.attachment {
            let conn = attachment.conn.clone();
            self.task(Task::Terminate(conn, STOP_GRACE));
        }
        Ok(())
    }

    /// `session.reopen`: a closed session becomes dormant.
    pub(crate) fn reopen(&mut self, params: &SessionOpParams) -> Result<(), RpcError> {
        self.session_mut(params.id)?.data.closed_at = None;
        Ok(())
    }

    /// `session.delete`: stop the agent and forget the session and its files.
    pub(crate) fn delete(&mut self, id: SessionId) -> Result<(), RpcError> {
        let session = self.session(id)?;
        let conn = session.live.attachment.as_ref().map(|a| a.conn.clone());
        self.remove_session(id);
        self.store.delete_session(id).map_err(internal)?;
        self.emit(MachineEvent::SessionRemoved { id })
            .map_err(internal)?;
        self.task(Task::Remove(id, conn));
        Ok(())
    }

    /// `session.resume`: reattach a stopped agent session.
    pub(crate) fn resume(
        &mut self,
        id: SessionId,
        sandbox: Option<&SandboxSpec>,
    ) -> Result<(), RpcError> {
        let session = self.session(id)?;
        if session.data.closed() || session.has_process() {
            return Err(invalid_state(session.state(), "session.resume"));
        }
        let by = start_mode_of(session)?;
        let session = self.session_mut(id)?;
        if let Some(spec) = sandbox {
            session.data.sandbox = spec.clone();
        }
        self.request_start(id, by)
    }

    /// `session.start_fresh`: a new agent session; `history` is the export the next prompt
    /// points at.
    pub(crate) fn start_fresh(
        &mut self,
        id: SessionId,
        op_id: &controlroom_protocol::ids::OpId,
        sandbox: Option<&SandboxSpec>,
        history: Option<RemotePath>,
    ) -> Result<(), RpcError> {
        let session = self.session(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "session.start_fresh"));
        }
        if session.live.launching || session.live.loading {
            return Err(busy("the agent is starting; try again"));
        }
        let live = session.has_process();
        let in_process = live
            && session.writable()
            && session.view().ready
            && session.view().capabilities.new_session;
        let parent = session.view().native.as_ref().map(|n| n.id.clone());
        let session = self.session_mut(id)?;
        if let Some(spec) = sandbox {
            session.data.sandbox = spec.clone();
        }
        session.data.history_note = history;
        session.data.pending_start = Some(StartedBy::Fresh);
        session.data.resume_failed = false;
        if in_process {
            let tag = LineTag::Op {
                op_id: op_id.clone(),
                part: 0,
            };
            let lines = session
                .driver_mut()
                .encode(&Intent::NewSession { parent }, &tag)
                .map_err(|e| unsupported(e.to_string()))?;
            session.live.handed.insert(tag);
            for line in lines {
                self.stage_line(id, line);
            }
            return Ok(());
        }
        if live {
            session.live.relaunch = true;
            if let Some(attachment) = &session.live.attachment {
                let conn = attachment.conn.clone();
                self.task(Task::Terminate(conn, STOP_GRACE));
            }
            return Ok(());
        }
        self.request_start(id, StartedBy::Fresh)
    }

    /// `session.update`.
    pub(crate) fn update(&mut self, params: &SessionUpdateParams) -> Result<(), RpcError> {
        if params.auto_continue.is_some() || params.auto_continue_budget.is_some() {
            return Err(unsupported(
                "auto-continue isn't available on this daemon yet",
            ));
        }
        let data = &mut self.session_mut(params.id)?.data;
        if let Some(name) = &params.name {
            data.name = Some(name.clone()).filter(|n| !n.is_empty());
        }
        if let Some(auto_send) = params.auto_send {
            data.auto_send = auto_send;
        }
        self.reconcile(params.id);
        Ok(())
    }

    /// `session.mark_read`: the cursor only moves forward, and never past the transcript.
    pub(crate) fn mark_read(
        &mut self,
        params: &SessionMarkReadParams,
    ) -> Result<ReadCursor, RpcError> {
        let head = self.head(params.id);
        let session = self.session_mut(params.id)?;
        let data = &mut session.data;
        data.read_seq = data.read_seq.max(params.seq.min(head));
        Ok(ReadCursor {
            read_seq: data.read_seq,
            unread: session.unread(),
        })
    }

    /// `session.mark_unread`: back to just before the last run's end.
    pub(crate) fn mark_unread(&mut self, id: SessionId) -> Result<ReadCursor, RpcError> {
        let session = self.session_mut(id)?;
        let data = &mut session.data;
        if data.last_turn_end_seq.0 > 0 {
            data.read_seq = Seq(data.last_turn_end_seq.0 - 1);
        }
        Ok(ReadCursor {
            read_seq: data.read_seq,
            unread: session.unread(),
        })
    }

    /// `session.set_option`: any live state, including during a run.
    pub(crate) fn set_option(&mut self, params: &SessionSetOptionParams) -> Result<(), RpcError> {
        let session = self.session(params.id)?;
        if session.live.loading || session.live.doubt.is_some() {
            return Err(busy("the session is reconnecting; try again"));
        }
        if !session.writable() || !session.view().ready {
            return Err(invalid_state(session.state(), "session.set_option"));
        }
        let options = &session.view().options;
        if options.is_empty() {
            return Err(unsupported("the agent reported no options"));
        }
        let option = options
            .iter()
            .find(|o| o.id == params.option_id)
            .ok_or_else(|| invalid_params(format!("no option {:?}", params.option_id)))?;
        let valid = match (&option.control, &params.value) {
            (OptionControl::Select { choices, .. }, OptionValue::Choice(value)) => {
                choices.iter().any(|c| c.value == *value)
            }
            (OptionControl::Bool { .. }, OptionValue::Bool(_)) => true,
            _ => false,
        };
        if !valid {
            return Err(invalid_params(format!(
                "{:?} doesn't allow this value",
                params.option_id
            )));
        }
        let intent = Intent::SetOption {
            id: params.option_id.clone(),
            value: params.value.clone(),
        };
        let tag = LineTag::Op {
            op_id: params.op_id.clone(),
            part: 0,
        };
        let session = self.session_mut(params.id)?;
        let lines = session
            .driver_mut()
            .encode(&intent, &tag)
            .map_err(|e| invalid_params(e.to_string()))?;
        session.live.handed.insert(tag);
        for line in lines {
            self.stage_line(params.id, line);
        }
        Ok(())
    }
}

/// How a stopped session's agent starts next: a pending start, a new session if none started
/// yet, else a resume (`not_resumable` when that's impossible).
pub(super) fn start_mode_of(session: &Session) -> Result<StartedBy, RpcError> {
    if let Some(by) = session.data.pending_start {
        return Ok(by);
    }
    if session.data.agent_sessions.is_empty() {
        return Ok(StartedBy::New);
    }
    if session.resumable() {
        return Ok(StartedBy::Resume);
    }
    Err(RpcError::new(
        ErrorData::NotResumable,
        "the agent can't resume this session; start fresh instead",
    ))
}
