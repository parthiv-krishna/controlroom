//! Requests from a session's hosts, shown as blocking requests on the session:
//!
//! - **Sandbox approvals** (spec §11.4): nono asks its host's webhook, the host raises an
//!   approval (`approvals.subscribe`, `approval.request`), and the agent daemon shows it as a
//!   `sandbox` request on the workspace's session (`approval-<approval id>`). The user's choice
//!   goes back with `approval.respond`; a withdrawal on the session (cancel, close) denies it.
//!   `approval.closed` (nono's timeout, a host restart) ends it without an answer. After every
//!   new host connection the pending list replaces what the session showed for that host.
//! - **Waiting for terminal input**: a `bash` call's command waits for the terminal
//!   (`tool.waiting_input`); the session shows a `terminal_input` request
//!   (`input-<call id>`) until the command moves on. The user types in the terminal; the agent
//!   can't.

use controlroom_protocol::RpcError;
use controlroom_protocol::host::options::{ALLOW_ALWAYS, DENY, NEVER};
use controlroom_protocol::host::{
    ApprovalRespondParams, HostApproval, HostRequest, ToolWaitingInputParams,
};
use controlroom_protocol::ids::{ApprovalId, HostOpId, MachineId, RequestId, SessionId};
use controlroom_protocol::session::{
    OptionStyle, Request, RequestBody, RequestKind, RequestOption, RequestState, Response,
};

use crate::core::Core;
use crate::model::{HostRequestRef, Raised};

/// The option of a `terminal_input` request.
const INPUT_DONE: &str = "done";

fn approval_request_id(approval: &ApprovalId) -> RequestId {
    RequestId::new(format!("approval-{approval}"))
}

impl Core {
    /// The host's pending approvals after a new connection: raises new ones and expires the
    /// ones the host no longer has.
    pub(crate) fn host_approvals(
        &mut self,
        machine: MachineId,
        pending: Vec<HostApproval>,
    ) -> Result<(), RpcError> {
        let listed: Vec<RequestId> = pending.iter().map(|a| approval_request_id(&a.id)).collect();
        let listed = &listed;
        let gone: Vec<(SessionId, Request)> = self
            .sessions
            .values()
            .flat_map(|s| {
                s.data.raised.values().filter_map(move |r| match &r.host {
                    Some(HostRequestRef::Approval { machine_id, .. })
                        if *machine_id == machine
                            && r.request.state == RequestState::Pending
                            && !listed.contains(&r.request.id) =>
                    {
                        Some((s.data.id, r.request.clone()))
                    }
                    _ => None,
                })
            })
            .collect();
        for (id, mut request) in gone {
            request.state = RequestState::Expired;
            self.settle_raised(id, request)?;
        }
        for approval in pending {
            self.host_approval(machine, approval)?;
        }
        Ok(())
    }

    /// A new approval from a host.
    pub(crate) fn host_approval(
        &mut self,
        machine: MachineId,
        approval: HostApproval,
    ) -> Result<(), RpcError> {
        let id = approval.session_id;
        let open = self.sessions.get(&id).is_some_and(|s| !s.data.closed());
        if !open {
            // Nobody can answer it: deny at once.
            self.respond_approval(machine, &approval.id, &approval.options, None);
            return Ok(());
        }
        // The user's own terminal asked: the same flow, marked as theirs.
        let title = if approval.from_user_terminal {
            format!("Your terminal: {}", approval.title)
        } else {
            approval.title
        };
        let (body, options) = self.on_machine(machine, approval.body, approval.options);
        let request = Request {
            id: approval_request_id(&approval.id),
            kind: RequestKind::Sandbox,
            blocking: true,
            state: RequestState::Pending,
            title,
            detail: approval.detail,
            body: Some(body),
            options,
            questions: Vec::new(),
            answer: None,
            origin_child: None,
            created_at: approval.created_at,
        };
        let host = HostRequestRef::Approval {
            machine_id: machine,
            approval_id: approval.id,
        };
        self.raise_host(id, request, host)
    }

    /// A file request names the machine that asked: "Always allow" and "Never" change that
    /// machine's sandbox defaults, so their labels (and the answer line) say which, e.g.
    /// `Always allow (devbox)`.
    fn on_machine(
        &self,
        machine: MachineId,
        mut body: RequestBody,
        mut options: Vec<RequestOption>,
    ) -> (RequestBody, Vec<RequestOption>) {
        if let RequestBody::Path {
            machine_id,
            machine: name,
            ..
        } = &mut body
        {
            let display = self.machines.name(&machine);
            for option in &mut options {
                if option.id == ALLOW_ALWAYS || option.id == NEVER {
                    option.label = format!("{} ({display})", option.label);
                }
            }
            *machine_id = Some(machine);
            *name = Some(display);
        }
        (body, options)
    }

    /// `approval.closed`: the host ended an approval without an answer.
    pub(crate) fn host_approval_closed(
        &mut self,
        approval: &ApprovalId,
        state: RequestState,
    ) -> Result<(), RpcError> {
        let request_id = approval_request_id(approval);
        let found = self.sessions.values().find_map(|s| {
            let raised = s.data.raised.get(&request_id)?;
            (raised.request.state == RequestState::Pending)
                .then(|| (s.data.id, raised.request.clone()))
        });
        if let Some((id, mut request)) = found {
            // Settled by the host: nothing to send back.
            request.state = match state {
                RequestState::Withdrawn => RequestState::Withdrawn,
                _ => RequestState::Expired,
            };
            let session = self.session_mut(id)?;
            if let Some(raised) = session.data.raised.get_mut(&request_id) {
                raised.request = request;
                let raised = raised.clone();
                self.publish_raised(id, &raised)?;
            }
        }
        Ok(())
    }

    /// `tool.waiting_input`: raises or withdraws the call's `terminal_input` request.
    pub(crate) fn waiting_input(
        &mut self,
        params: &ToolWaitingInputParams,
    ) -> Result<(), RpcError> {
        let Some((id, machine, shell, command)) = self.sessions.values().find_map(|s| {
            s.live.calls.values().find_map(|c| {
                let tool = c.tool.as_ref()?;
                let (shell, command) = tool.shell.clone()?;
                (tool.call_id == params.call_id).then_some((s.data.id, c.machine, shell, command))
            })
        }) else {
            return Ok(());
        };
        let request_id = RequestId::new(format!("input-{}", params.call_id));
        if !params.waiting {
            return self.end_input(id, &request_id);
        }
        let pending = self
            .session(id)?
            .data
            .raised
            .get(&request_id)
            .is_some_and(|r| r.request.state == RequestState::Pending);
        if pending {
            return Ok(());
        }
        // A wait after an earlier one: raise it again.
        self.session_mut(id)?.data.raised.remove(&request_id);
        let machine = self.machines.name(&machine);
        let request = Request {
            id: request_id,
            kind: RequestKind::TerminalInput,
            blocking: true,
            state: RequestState::Pending,
            title: format!("Waiting for input in shell {shell} on {machine}"),
            detail: Some(format!(
                "The agent's command waits for terminal input. Type it in the terminal of shell \
                 `{shell}`; the agent can't."
            )),
            body: Some(RequestBody::TerminalInput {
                machine,
                shell,
                command,
            }),
            options: vec![RequestOption {
                id: INPUT_DONE.into(),
                label: "Done".into(),
                style: OptionStyle::Neutral,
            }],
            questions: Vec::new(),
            answer: None,
            origin_child: None,
            created_at: self.clock.now(),
        };
        let host = HostRequestRef::Input {
            call_id: params.call_id.clone(),
        };
        self.raise_host(id, request, host)
    }

    /// Withdraws a pending `terminal_input` request (the command moved on or ended).
    pub(crate) fn end_input(
        &mut self,
        id: SessionId,
        request_id: &RequestId,
    ) -> Result<(), RpcError> {
        let Some(raised) = self.session(id)?.data.raised.get(request_id) else {
            return Ok(());
        };
        if raised.request.state != RequestState::Pending {
            return Ok(());
        }
        let mut request = raised.request.clone();
        request.state = RequestState::Withdrawn;
        self.settle_raised(id, request)
    }

    /// A raised host request was resolved on the session: an approval's answer (or denial, when
    /// withdrawn or expired here) goes back to its host.
    pub(crate) fn host_request_settled(&mut self, _id: SessionId, raised: &Raised) {
        let Some(HostRequestRef::Approval {
            machine_id,
            approval_id,
        }) = &raised.host
        else {
            return;
        };
        let request = &raised.request;
        let choice = match request.answer.as_ref().map(|a| &a.response) {
            Some(Response::Choice { option_id, reason }) => {
                Some((option_id.clone(), reason.clone()))
            }
            _ => None,
        };
        match (request.state, choice) {
            (RequestState::Answered, Some((option_id, reason))) => {
                self.send_approval_response(*machine_id, approval_id, option_id, reason);
            }
            (RequestState::Withdrawn, _) => {
                self.respond_approval(*machine_id, approval_id, &request.options, None);
            }
            _ => {}
        }
    }

    /// Denies an approval with its plain deny option (never "Never", which would also deny the
    /// path from then on).
    fn respond_approval(
        &mut self,
        machine: MachineId,
        approval: &ApprovalId,
        options: &[RequestOption],
        reason: Option<String>,
    ) {
        let deny = options
            .iter()
            .find(|o| o.id == DENY)
            .or_else(|| {
                options
                    .iter()
                    .find(|o| o.style == OptionStyle::Deny && o.id != NEVER)
            })
            .map_or_else(|| DENY.to_owned(), |o| o.id.clone());
        self.send_approval_response(machine, approval, deny, reason);
    }

    fn send_approval_response(
        &mut self,
        machine: MachineId,
        approval: &ApprovalId,
        option_id: String,
        reason: Option<String>,
    ) {
        self.host_send(
            machine,
            None,
            HostOpId::new(format!("approval.respond:{approval}")),
            HostRequest::ApprovalRespond(ApprovalRespondParams {
                approval_id: *approval,
                option_id,
                reason,
            }),
        );
    }
}
