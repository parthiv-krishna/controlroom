//! Session lifecycle and session state commands.

use std::time::Duration;

use controlroom_drivers::{Intent, LineTag};
use controlroom_protocol::agentd::{
    IfBusy, OptionValue, PromptResult, ReadCursor, SessionCreateParams, SessionCreateResult,
    SessionMarkReadParams, SessionOpParams, SessionPromptParams, SessionSetOptionParams,
    SessionUpdateParams, SessionsEvent,
};
use controlroom_protocol::ids::{QueueId, RemotePath, Seq, SessionId};
use controlroom_protocol::session::{
    DriverKind, OptionControl, RequestState, SandboxSpec, SessionState, StartedBy,
};
use controlroom_protocol::{ErrorData, Resource, RpcError};
use std::collections::BTreeMap;

use super::validate_content;
use crate::core::{Core, HostEffect, Task};
use crate::errors::{busy, internal, invalid_params, invalid_state, not_found, unsupported};
use crate::hosts::{ShellRecord, WorkspaceRecord, default_sandbox};
use crate::model::{Delivery, QueueEntry, SessionData, Via};
use crate::session::Session;

/// How long a closing or restarting agent gets before SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(5);

impl Core {
    /// `session.create` with its workspaces already opened on their hosts (by the caller),
    /// each with its default shell. A first prompt is queued in the same transaction, tagged
    /// with the create's op id, so it reaches the agent exactly once, as a user prompt.
    pub(crate) fn create(
        &mut self,
        params: &SessionCreateParams,
        driver: DriverKind,
        id: SessionId,
        workspaces: Vec<WorkspaceRecord>,
    ) -> Result<SessionCreateResult, RpcError> {
        if let Some(parent) = params.parent_id {
            self.session(parent)?;
        }
        if let Some(prompt) = &params.prompt {
            validate_content(&prompt.content)?;
        }
        let shells = workspaces
            .iter()
            .map(|w| {
                let cwd = w.root.clone().unwrap_or_else(|| w.path.clone());
                ShellRecord::new(w.shell.clone(), w.id, cwd)
            })
            .collect();
        let data = SessionData {
            id,
            driver,
            name: params.name.clone().filter(|n| !n.is_empty()),
            created_at: self.clock.now(),
            closed_at: None,
            parent_id: params.parent_id,
            workspaces,
            shells,
            late: Vec::new(),
            outages: BTreeMap::new(),
            sandbox_level: params.sandbox_level,
            sandbox: params.sandbox.clone().unwrap_or_else(default_sandbox),
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
            raised: BTreeMap::new(),
            jobs: BTreeMap::new(),
            todos: Vec::new(),
            injections: Vec::new(),
            auto: Default::default(),
            triggers: Vec::new(),
        };
        let driver = self.agents.driver(driver);
        self.insert_session(Session::new(data, driver, self.machines.clone()));
        self.task(Task::OpenShells(id));
        let prompt = match &params.prompt {
            Some(prompt) => {
                let entry = QueueEntry {
                    id: QueueId::generate(),
                    content: prompt.content.clone(),
                    origin: prompt.origin.clone(),
                    op_id: params.op_id.clone(),
                    queued_at: self.clock.now(),
                    direct: true,
                    send_now: true,
                    interrupt: false,
                    no_steer: false,
                    delivery: None,
                };
                let queue_id = entry.id;
                self.enqueue(id, entry, true)?;
                Some(PromptResult::Queued { queue_id })
            }
            None => None,
        };
        self.request_start(id, StartedBy::New)?;
        Ok(SessionCreateResult {
            session: self.summary(id),
            prompt,
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
        self.cancel_calls(params.id, None);
        self.auto_continue_cancelled(params.id)?;
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
        self.kill_session_jobs(params.id)?;
        self.cancel_calls(params.id, None);
        self.close_shells(params.id)?;
        self.end_pending_raised(params.id, RequestState::Withdrawn, true)
    }

    /// `session.reopen`: a closed session becomes dormant (its shells reopen with it).
    pub(crate) fn reopen(&mut self, params: &SessionOpParams) -> Result<(), RpcError> {
        let session = self.session_mut(params.id)?;
        if session.data.closed_at.take().is_some() {
            self.task(Task::OpenShells(params.id));
        }
        Ok(())
    }

    /// `session.delete`: stop the agent, close the workspaces on their hosts (their shells and
    /// jobs stop) and forget the session and its files.
    pub(crate) fn delete(&mut self, id: SessionId) -> Result<(), RpcError> {
        let session = self.session(id)?;
        let conn = session.live.attachment.as_ref().map(|a| a.conn.clone());
        let workspaces: Vec<_> = session
            .data
            .workspaces
            .iter()
            .map(|w| (w.machine_id, w.id))
            .collect();
        let calls: Vec<_> = session.live.calls.keys().cloned().collect();
        for key in calls {
            self.effect(HostEffect::Withdraw(key));
        }
        for (machine, workspace) in workspaces {
            self.effect(HostEffect::Send(crate::hosts::close_request(
                id, machine, workspace,
            )));
        }
        self.remove_session(id);
        self.store.delete_session(id).map_err(internal)?;
        self.emit(SessionsEvent::SessionRemoved { id })
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
        self.set_sandbox(id, sandbox)?;
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
        self.set_sandbox(id, sandbox)?;
        let session = self.session_mut(id)?;
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

    /// New sandbox defaults for the next start; the workspaces' profiles are rewritten.
    fn set_sandbox(
        &mut self,
        id: SessionId,
        sandbox: Option<&SandboxSpec>,
    ) -> Result<(), RpcError> {
        if let Some(spec) = sandbox {
            self.session_mut(id)?.data.sandbox = spec.clone();
            self.task(Task::Refresh(id));
        }
        Ok(())
    }

    /// `session.update`.
    pub(crate) fn update(&mut self, params: &SessionUpdateParams) -> Result<(), RpcError> {
        self.update_auto_continue(params)?;
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
            .ok_or_else(|| not_found(Resource::Option, &params.option_id))?;
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
pub(crate) fn start_mode_of(session: &Session) -> Result<StartedBy, RpcError> {
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
