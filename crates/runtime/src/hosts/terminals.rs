//! The user's own terminals (`session.terminal_open`, `session.terminal_close`): shells owned
//! by the user on a workspace's host, kept with the session's shell records so that they end
//! with the session (close, delete) and their workspace, exactly like the agent's shells.
//!
//! - **Private.** The agent never sees them: `session.info` and `shell.list` show the agent's
//!   shells only, and its `bash`, `shell_new` and `shell.close` don't find them. Their names
//!   (`you@<workspace's default shell>[-<n>]`) have an `@`, which agent shell names can't, so
//!   the two never collide.
//! - **Unsandboxed** ones need `confirm: true`. Every opening is logged with its device.
//! - **Not reopened.** A terminal whose process ends is reported `gone` by its host and its
//!   record goes. After every new host connection the open terminals are checked against the
//!   host's `shell.list`: one the host no longer has (it restarted without the supervisor, or
//!   the `gone` report was lost with the link) goes too.

use std::sync::Arc;

use controlroom_protocol::Resource;
use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::{TerminalCloseParams, TerminalOpenParams};
use controlroom_protocol::host::{
    HostRequest, HostShell, ShellCloseParams, ShellListParams, ShellListResult, ShellOpenParams,
};
use controlroom_protocol::ids::{HostOpId, MachineId, SessionId, ShellId, WorkspaceId};
use controlroom_protocol::session::{
    DeviceRef, SessionState, Shell, ShellOwner, ShellSandbox, ShellState,
};

use super::calls::{parse, session_request};
use super::records::ShellRecord;
use super::shells::shell_gone;
use crate::commands::params_hash;
use crate::core::Core;
use crate::errors::{invalid_params, invalid_state, not_found};
use crate::runtime::{Inner, Runtime};

/// The name prefix of the user's terminals.
const PREFIX: &str = "you@";

impl Runtime {
    /// `session.terminal_open`: opens a terminal of the user's own (see the module docs).
    pub async fn terminal_open(&self, params: TerminalOpenParams) -> Result<Shell, RpcError> {
        const METHOD: &str = "session.terminal_open";
        self.recovered().await;
        let inner = self.inner();
        let hash = params_hash(&params);
        if let Some(result) =
            inner.with_core(|core| core.stored_op(METHOD, &params.op_id, &hash))?
        {
            return Ok(result);
        }
        let id = params.id;
        let (machine, name) = inner.transact(|core| core.reserve_terminal(&params))?;
        let request = HostRequest::ShellOpen(ShellOpenParams {
            owner: ShellOwner::User,
            sandbox: params.sandbox,
            by: params.by.clone(),
            ..ShellOpenParams::agent(params.workspace_id, name.clone(), None)
        });
        let key = HostOpId::new(format!("shell.open:{id}/{name}"));
        let opened = session_request(inner, id, machine, key, request, None)
            .await
            .and_then(|(value, _)| parse::<HostShell>(value));
        let host = match opened {
            Ok(host) => host,
            Err(e) => {
                let _ = inner.transact(|core| {
                    core.forget_terminal(id, &name);
                    Ok(())
                });
                return Err(e);
            }
        };
        tracing::info!(
            session = %id,
            shell = %name,
            sandbox = ?params.sandbox,
            device = %device(params.by.as_ref()),
            "user terminal opened"
        );
        self.command(METHOD, &params.op_id, &params, |core| {
            core.shell_opened(id, machine, &host)?;
            core.session(id)?
                .data
                .shells
                .iter()
                .find(|s| s.shell.name == name)
                .map(|s| s.shell.clone())
                .ok_or_else(|| shell_gone(&name))
        })
        .await
    }

    /// `session.terminal_close`: ends one of the user's terminals.
    pub async fn terminal_close(&self, params: TerminalCloseParams) -> Result<(), RpcError> {
        self.command("session.terminal_close", &params.op_id, &params, |core| {
            core.close_terminal(params.id, &params.name)
        })
        .await
    }
}

/// The device of a terminal's opening, for the log.
fn device(by: Option<&DeviceRef>) -> String {
    by.map_or_else(
        || "an unknown device".to_owned(),
        |d| format!("{} ({})", d.label, d.device_id),
    )
}

/// Checks the session's open terminals against their hosts' `shell.list` (after a new host
/// connection) and drops the ones a host no longer has.
pub(crate) async fn verify_terminals(inner: Arc<Inner>, id: SessionId) {
    // Only terminals already opened: one being opened right now may not be listed yet.
    let due: Vec<(MachineId, WorkspaceId, Vec<String>)> = inner.with_core(|core| {
        let Some(session) = core.sessions.get(&id) else {
            return Vec::new();
        };
        session
            .data
            .workspaces
            .iter()
            .filter(|w| core.machines.connected(&w.machine_id))
            .filter_map(|w| {
                let names: Vec<String> = session
                    .data
                    .shells
                    .iter()
                    .filter(|s| !s.is_agents() && s.opened.is_some())
                    .filter(|s| s.shell.workspace_id == w.id)
                    .map(|s| s.shell.name.clone())
                    .collect();
                (!names.is_empty()).then_some((w.machine_id, w.id, names))
            })
            .collect()
    });
    for (machine, workspace, names) in due {
        let request = HostRequest::ShellList(ShellListParams {
            workspace_id: Some(workspace),
        });
        let key = HostOpId::new(format!("shell.list:{id}/{workspace}"));
        let listed = session_request(&inner, id, machine, key, request, None)
            .await
            .and_then(|(value, _)| parse::<ShellListResult>(value));
        let listed = match listed {
            Ok(listed) => listed.shells,
            Err(e) => {
                tracing::warn!("session {id}: checking the user's terminals: {e}");
                continue;
            }
        };
        let _ = inner.transact(|core| {
            core.terminals_listed(id, machine, &names, &listed);
            Ok(())
        });
    }
}

impl Core {
    /// Validates `session.terminal_open` and reserves the terminal's name with its record.
    fn reserve_terminal(
        &mut self,
        params: &TerminalOpenParams,
    ) -> Result<(MachineId, String), RpcError> {
        let data = &self.session(params.id)?.data;
        if data.closed() {
            return Err(invalid_state(SessionState::Closed, "session.terminal_open"));
        }
        match params.sandbox {
            ShellSandbox::Workspace => {}
            ShellSandbox::Unsandboxed if params.confirm => {}
            ShellSandbox::Unsandboxed => {
                return Err(invalid_params(
                    "an unsandboxed terminal needs confirm: true",
                ));
            }
            ShellSandbox::Unknown => return Err(invalid_params("unknown sandbox mode")),
        }
        let workspace = data
            .workspace(params.workspace_id)
            .ok_or_else(|| not_found(Resource::Workspace, params.workspace_id))?;
        self.machines.require_connected(&workspace.machine_id)?;
        let base = format!("{PREFIX}{}", workspace.shell);
        let taken = |name: &str| data.shells.iter().any(|s| s.shell.name == name);
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|name| !taken(name))
            .expect("some suffix is free");
        let record = ShellRecord::user(
            params.sandbox,
            name.clone(),
            workspace.id,
            workspace
                .root
                .clone()
                .unwrap_or_else(|| workspace.path.clone()),
        );
        let machine = workspace.machine_id;
        self.session_mut(params.id)?.data.shells.push(record);
        Ok((machine, name))
    }

    /// Drops a terminal's record (it didn't open, or it ended).
    fn forget_terminal(&mut self, id: SessionId, name: &str) {
        if let Ok(session) = self.session_mut(id) {
            session
                .data
                .shells
                .retain(|s| s.is_agents() || s.shell.name != name);
        }
    }

    /// Ends one of the user's terminals on its host.
    fn close_terminal(&mut self, id: SessionId, name: &str) -> Result<(), RpcError> {
        let data = &self.session(id)?.data;
        let record = data
            .shells
            .iter()
            .find(|s| s.shell.name == name)
            .ok_or_else(|| shell_gone(name))?;
        if record.is_agents() {
            return Err(invalid_params(
                "only your own terminals can be closed; the agent's shells stay open",
            ));
        }
        let machine = data
            .workspace(record.shell.workspace_id)
            .map(|w| w.machine_id)
            .ok_or_else(|| not_found(Resource::Workspace, record.shell.workspace_id))?;
        self.forget_terminal(id, name);
        tracing::info!(session = %id, shell = name, "user terminal closed");
        self.host_send(
            machine,
            Some(id),
            HostOpId::new(format!("shell.close:{id}/{name}")),
            HostRequest::ShellClose(ShellCloseParams {
                shell_id: ShellId::of(id, name),
            }),
        );
        Ok(())
    }

    /// A host's `shell.list` for terminals `names`: the listed ones are current, the others
    /// ended.
    fn terminals_listed(
        &mut self,
        id: SessionId,
        machine: MachineId,
        names: &[String],
        listed: &[HostShell],
    ) {
        for name in names {
            match listed
                .iter()
                .find(|h| h.shell.name == *name && h.shell.state != ShellState::Gone)
            {
                Some(host) => {
                    if let Err(e) = self.shell_opened(id, machine, host) {
                        tracing::debug!("session {id}: terminal {name}: {e}");
                    }
                }
                None => {
                    tracing::info!(session = %id, shell = %name, "user terminal gone from its host");
                    self.forget_terminal(id, name);
                }
            }
        }
    }
}
