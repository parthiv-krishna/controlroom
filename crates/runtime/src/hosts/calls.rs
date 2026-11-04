//! Tool calls on hosts: the agent's `host.call` and `host.cancel`, and every other host request
//! a session waits for.
//!
//! - **Routing.** The agent names a machine (one of the session's workspaces, else `not_found`
//!   `workspace`) and, for `bash`, a shell. The call runs in the shell's workspace, or the
//!   workspace on that machine whose root holds the path (else the machine's first one). Job
//!   ops are refused: jobs go through `job.*`. A shell not yet opened in the host's current
//!   epoch is opened first (`shell.open` is idempotent and reopens a lost shell).
//! - **Link loss pauses.** A call stays open while its host is unreachable: the request waits on
//!   the bridge and the controller resends it after the reconnect (the host's journal makes
//!   that safe). Each drop of the link while the call is open is counted (`link_drops`).
//! - **Grace.** When the machine has been unavailable for its grace period (checked by the
//!   tick), the call fails with `connection_lost`, its request is withdrawn, and a `tool` call
//!   becomes *late*: its result is fetched with `tool.attach` once the host is back and injected
//!   (see `late`).
//! - **Journal acks.** A result is acknowledged (`journal.ack`) once pi has logged the tool
//!   call's end in the transcript, so a resend after the extension reconnects still finds the
//!   result in the host's journal.

use std::sync::Arc;

use controlroom_protocol::agent::{HostCallParams, HostCallResult, HostCancelParams};
use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{
    HostOp, HostRequest, JournalAckParams, ShellOpenParams, ToolCallRef, ToolRunParams,
};
use controlroom_protocol::ids::{CallId, HostOpId, MachineId, RemotePath, SessionId, WorkspaceId};
use controlroom_protocol::session::SessionState;
use controlroom_protocol::{ErrorData, Resource, RpcError};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::watch;

use super::records::LateCall;
use crate::core::{Core, HostEffect, Task};
use crate::errors::{invalid_params, invalid_state, not_found, unsupported};
use crate::runtime::{Inner, Runtime};

/// Commands are cut to this many characters in call descriptions.
const WHAT_CHARS: usize = 60;

/// A host request a session waits for.
#[derive(Clone)]
pub(crate) struct OpenCall {
    pub machine: MachineId,
    /// The agent's tool call, for `tool.run`.
    pub tool: Option<ToolCall>,
    /// Times the machine's link dropped while the call was open.
    pub link_drops: u32,
    /// Fails the waiters (past the grace period).
    pub expire: watch::Sender<Option<RpcError>>,
}

/// What an open `tool.run` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub call_id: CallId,
    /// For late results, e.g. ``bash `make test` ``.
    pub what: String,
    /// For `bash`: the shell and the command (a wait for terminal input names them).
    pub shell: Option<(String, String)>,
}

/// A finished call whose result pi hasn't logged yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoneCall {
    pub machine: MachineId,
    pub call_id: CallId,
}

/// Where a `host.call` goes.
struct CallPlan {
    machine: MachineId,
    workspace: WorkspaceId,
    tool: ToolCall,
    /// The shell to open first: (name, its last directory).
    open_shell: Option<(String, RemotePath)>,
}

impl Runtime {
    /// `host.call`: runs one host operation of a tool (see the module docs).
    pub async fn host_call(
        &self,
        id: SessionId,
        params: HostCallParams,
    ) -> Result<HostCallResult, RpcError> {
        self.recovered().await;
        let inner = self.inner();
        let plan = inner.transact(|core| core.plan_call(id, &params))?;
        if let Some((name, cwd)) = plan.open_shell {
            open_shell(inner, id, plan.machine, plan.workspace, name, Some(cwd)).await?;
        }
        let request = HostRequest::ToolRun(ToolRunParams {
            call_id: params.call_id.clone(),
            workspace_id: plan.workspace,
            op: params.op,
        });
        let call_key = HostOpId::new(params.call_id.as_str());
        let outcome =
            session_request(inner, id, plan.machine, call_key, request, Some(plan.tool)).await;
        if !matches!(&outcome, Err(e) if matches!(e.data, Some(ErrorData::ConnectionLost { .. }))) {
            let done = DoneCall {
                machine: plan.machine,
                call_id: params.call_id,
            };
            let _ = inner.transact(|core| {
                core.session_mut(id)?.live.done.push(done);
                Ok(())
            });
        }
        let (value, link_drops) = outcome?;
        Ok(HostCallResult {
            result: parse(value)?,
            link_drops,
        })
    }

    /// `host.cancel`: interrupts a running call (a `bash` gets ETX in its shell).
    pub async fn host_cancel(
        &self,
        id: SessionId,
        params: HostCancelParams,
    ) -> Result<(), RpcError> {
        self.recovered().await;
        self.inner().transact(|core| {
            core.session(id)?;
            core.cancel_calls(id, Some(&params.call_id));
            Ok(())
        })
    }
}

/// Opens (or reopens) a shell on its host and records it.
pub(crate) async fn open_shell(
    inner: &Arc<Inner>,
    id: SessionId,
    machine: MachineId,
    workspace: WorkspaceId,
    name: String,
    cwd: Option<RemotePath>,
) -> Result<(), RpcError> {
    let request = HostRequest::ShellOpen(ShellOpenParams {
        workspace_id: workspace,
        name: name.clone(),
        cwd,
    });
    let key = HostOpId::new(format!("shell.open:{id}/{name}"));
    let (value, _) = session_request(inner, id, machine, key, request, None).await?;
    let shell = parse(value)?;
    inner.transact(|core| core.shell_opened(id, machine, &shell))
}

/// Sends a session's host request and waits for its outcome; fails with `connection_lost` when
/// its machine stays unavailable past the grace period. Returns the result and the link drops.
pub(crate) async fn session_request(
    inner: &Arc<Inner>,
    id: SessionId,
    machine: MachineId,
    key: HostOpId,
    request: HostRequest,
    tool: Option<ToolCall>,
) -> Result<(Value, u32), RpcError> {
    let mut expired = inner.transact(|core| core.open_call(id, &key, machine, tool))?;
    let response = inner.bridge.request(HostOpRequest {
        id: key.clone(),
        machine_id: machine,
        session_id: Some(id),
        request,
    });
    let outcome = tokio::select! {
        response = response => response.ok(),
        error = wait_expired(&mut expired) => Some(Err(error)),
    };
    // `None`: withdrawn, because the grace expired (or the session went away).
    let outcome = outcome.unwrap_or_else(|| {
        let expired = expired.borrow().clone();
        Err(expired
            .unwrap_or_else(|| RpcError::new(ErrorData::Busy, "the host request was withdrawn")))
    });
    let link_drops = inner
        .transact(|core| Ok(core.close_call(id, &key)))
        .unwrap_or(0);
    outcome.map(|value| (value, link_drops))
}

async fn wait_expired(expired: &mut watch::Receiver<Option<RpcError>>) -> RpcError {
    let error = match expired.wait_for(Option::is_some).await {
        Ok(error) => error.clone(),
        Err(_) => None,
    };
    match error {
        Some(error) => error,
        // The registration is gone: only the response can end the wait.
        None => std::future::pending().await,
    }
}

/// A host result of the expected type; a newer host's shape fails only this call.
pub(crate) fn parse<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|e| {
        RpcError::new(
            ErrorData::InvalidResponse,
            format!("the host's result didn't parse: {e}"),
        )
    })
}

fn connection_lost(machine_id: MachineId, since: controlroom_protocol::UnixMillis) -> RpcError {
    RpcError::new(
        ErrorData::ConnectionLost { machine_id, since },
        "the machine has been unreachable longer than its grace period; the call may still be \
         running there, and its result will be delivered when the machine is back",
    )
}

impl Core {
    /// Registers a waiter of host request `key` (joining a pending one).
    fn open_call(
        &mut self,
        id: SessionId,
        key: &HostOpId,
        machine: MachineId,
        tool: Option<ToolCall>,
    ) -> Result<watch::Receiver<Option<RpcError>>, RpcError> {
        let session = self.session_mut(id)?;
        if let Some(open) = session.live.calls.get(key) {
            return Ok(open.expire.subscribe());
        }
        let (expire, expired) = watch::channel(None);
        session.live.calls.insert(
            key.clone(),
            OpenCall {
                machine,
                tool,
                link_drops: 0,
                expire,
            },
        );
        self.touch(id);
        Ok(expired)
    }

    /// Forgets a finished request (and its wait for terminal input); returns its link drops.
    fn close_call(&mut self, id: SessionId, key: &HostOpId) -> u32 {
        let Some(call) = self
            .session_mut(id)
            .ok()
            .and_then(|s| s.live.calls.remove(key))
        else {
            return 0;
        };
        if let Some(tool) = &call.tool {
            let input =
                controlroom_protocol::ids::RequestId::new(format!("input-{}", tool.call_id));
            if let Err(e) = self.end_input(id, &input) {
                tracing::warn!("session {id}: ending a wait for input: {e}");
            }
        }
        call.link_drops
    }

    /// Validates and routes a `host.call` (see the module docs).
    fn plan_call(&mut self, id: SessionId, params: &HostCallParams) -> Result<CallPlan, RpcError> {
        let session = self.session(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "host.call"));
        }
        if let Some(late) = session
            .data
            .late
            .iter()
            .find(|l| l.call_id == params.call_id)
        {
            return Err(connection_lost(late.machine_id, late.failed_at));
        }
        let path = match &params.op {
            HostOp::JobRun { .. }
            | HostOp::JobFollow { .. }
            | HostOp::JobUnfollow { .. }
            | HostOp::JobKill { .. }
            | HostOp::JobStatus { .. } => {
                return Err(invalid_params("jobs go through job.*, not host.call"));
            }
            HostOp::Unknown => return Err(unsupported("this operation is unknown")),
            HostOp::Read { path, .. }
            | HostOp::ReadBytes { path, .. }
            | HostOp::Stat { path }
            | HostOp::Write { path, .. } => Some(path),
            HostOp::Ls { path, .. } | HostOp::Find { path, .. } | HostOp::Grep { path, .. } => {
                path.as_ref()
            }
            HostOp::Bash { .. } => None,
        };
        let machine = self
            .machines
            .by_name(&params.machine)
            .filter(|m| session.data.workspaces.iter().any(|w| w.machine_id == *m))
            .ok_or_else(|| not_found(Resource::Workspace, &params.machine))?;
        let tool = ToolCall {
            call_id: params.call_id.clone(),
            what: describe(&params.op),
            shell: match &params.op {
                HostOp::Bash { shell, command, .. } => Some((shell.clone(), command.clone())),
                _ => None,
            },
        };
        if let HostOp::Bash { shell, .. } = &params.op {
            let record = session
                .data
                .shells
                .iter()
                .find(|s| s.shell.name == *shell)
                .ok_or_else(|| super::shells::shell_gone(shell))?;
            let workspace = record.shell.workspace_id;
            if session.data.workspace(workspace).map(|w| w.machine_id) != Some(machine) {
                return Err(invalid_params(format!(
                    "shell {shell:?} is not on {}",
                    params.machine
                )));
            }
            let epoch = self.machines.epoch(&machine);
            let open_shell = (record.opened.is_none() || record.opened != epoch)
                .then(|| (shell.clone(), record.shell.cwd.clone()));
            return Ok(CallPlan {
                machine,
                workspace,
                tool,
                open_shell,
            });
        }
        let on_machine = || {
            session
                .data
                .workspaces
                .iter()
                .filter(|w| w.machine_id == machine)
        };
        let workspace = path
            .and_then(|p| on_machine().find(|w| w.contains(p.as_str())))
            .or_else(|| on_machine().next())
            .map(|w| w.id)
            .ok_or_else(|| not_found(Resource::Workspace, &params.machine))?;
        Ok(CallPlan {
            machine,
            workspace,
            tool,
            open_shell: None,
        })
    }

    /// Interrupts the session's running tool calls (all of them, or one).
    pub(crate) fn cancel_calls(&mut self, id: SessionId, only: Option<&CallId>) {
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let cancels: Vec<(MachineId, CallId)> = session
            .live
            .calls
            .values()
            .filter_map(|c| Some((c.machine, c.tool.as_ref()?.call_id.clone())))
            .filter(|(_, call)| only.is_none_or(|o| o == call))
            .collect();
        for (machine, call_id) in cancels {
            self.host_send(
                machine,
                Some(id),
                HostOpId::new(format!("cancel:{call_id}")),
                HostRequest::ToolCancel(ToolCallRef { call_id }),
            );
        }
    }

    /// Fails the calls whose machine has been unavailable past its grace period (the tick).
    pub(crate) fn expire_calls(&mut self) -> Result<(), RpcError> {
        let now = self.clock.now();
        let machines = self.machines.clone();
        let mut due = Vec::new();
        for session in self.sessions.values() {
            for (key, call) in &session.live.calls {
                if let Some(since) = machines.down_since(&call.machine)
                    && now.0 - since.0 >= machines.grace_ms(&call.machine)
                {
                    due.push((session.data.id, key.clone(), since));
                }
            }
        }
        for (id, key, since) in due {
            let Some(call) = self.session_mut(id)?.live.calls.remove(&key) else {
                continue;
            };
            tracing::info!("session {id}: {key} failed after the grace period");
            // The waiters learn why before their request disappears.
            self.effect(HostEffect::Expire(
                call.expire,
                connection_lost(call.machine, since),
            ));
            self.effect(HostEffect::Withdraw(key));
            if let Some(tool) = call.tool {
                let late = LateCall {
                    call_id: tool.call_id,
                    machine_id: call.machine,
                    what: tool.what,
                    failed_at: now,
                };
                self.session_mut(id)?.data.late.push(late.clone());
                self.task(Task::Attach(id, late));
            }
        }
        Ok(())
    }

    /// pi logged the end of tool call `tool_call_id`: its host steps' results may be released.
    pub(crate) fn tool_finished(&mut self, id: SessionId, tool_call_id: &str) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        let prefix = format!("{tool_call_id}/");
        let (finished, open): (Vec<DoneCall>, Vec<DoneCall>) =
            std::mem::take(&mut session.live.done)
                .into_iter()
                .partition(|d| {
                    d.call_id.as_str() == tool_call_id || d.call_id.as_str().starts_with(&prefix)
                });
        session.live.done = open;
        for done in finished {
            self.acks
                .entry(done.machine)
                .or_default()
                .push(done.call_id);
        }
    }

    /// The agent process ended: whatever it didn't log is released anyway.
    pub(crate) fn release_done_calls(&mut self, id: SessionId) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        for done in std::mem::take(&mut session.live.done) {
            self.acks
                .entry(done.machine)
                .or_default()
                .push(done.call_id);
        }
    }

    /// Sends the collected journal acks of connected machines, one request per machine at a
    /// time (the tick).
    pub(crate) fn send_acks(&mut self) {
        let machines: Vec<MachineId> = self.acks.keys().copied().collect();
        for machine in machines {
            let key = HostOpId::new(format!("journal.ack:{machine}"));
            if !self.machines.connected(&machine) || self.bridge.is_pending(&key) {
                continue;
            }
            let call_ids = self.acks.remove(&machine).unwrap_or_default();
            if !call_ids.is_empty() {
                self.host_send(
                    machine,
                    None,
                    key,
                    HostRequest::JournalAck(JournalAckParams { call_ids }),
                );
            }
        }
    }

    /// Sends a host request after the commit, with nobody waiting for its outcome.
    pub(crate) fn host_send(
        &mut self,
        machine: MachineId,
        session: Option<SessionId>,
        key: HostOpId,
        request: HostRequest,
    ) {
        self.effect(HostEffect::Send(HostOpRequest {
            id: key,
            machine_id: machine,
            session_id: session,
            request,
        }));
    }
}

/// A short description of an op, for late results.
fn describe(op: &HostOp) -> String {
    let path = |p: Option<&RemotePath>| p.map_or(".".to_owned(), |p| p.to_string());
    match op {
        HostOp::Bash { command, .. } => {
            let line = command.lines().next().unwrap_or_default();
            let mut short: String = line.chars().take(WHAT_CHARS).collect();
            if short.len() < command.len() {
                short.push('…');
            }
            format!("bash `{short}`")
        }
        HostOp::Read { path: p, .. } | HostOp::ReadBytes { path: p, .. } => format!("read {p}"),
        HostOp::Stat { path: p } => format!("stat {p}"),
        HostOp::Write { path: p, .. } => format!("write {p}"),
        HostOp::Ls { path: p, .. } => format!("ls {}", path(p.as_ref())),
        HostOp::Find { pattern, .. } => format!("find {pattern}"),
        HostOp::Grep { pattern, .. } => format!("grep {pattern}"),
        _ => "a call".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_described_shortly() {
        let bash = HostOp::Bash {
            shell: "devbox".into(),
            command: format!("{}\nsecond line", "x".repeat(100)),
            timeout_s: None,
        };
        let what = describe(&bash);
        assert!(
            what.starts_with("bash `xxx") && what.ends_with("…`"),
            "{what}"
        );
        let write = HostOp::Write {
            path: RemotePath::new("~/a.txt"),
            content: String::new(),
            encoding: Default::default(),
            expect_sha256: None,
        };
        assert_eq!(describe(&write), "write ~/a.txt");
    }
}
