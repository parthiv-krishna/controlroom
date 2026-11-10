//! A session's named shells: the agent's `shell_new`, `shell_list` and closing a shell, and the
//! hosts' `shell.changed` reports (cwd, state, foreground process, reset).
//!
//! A shell is a PTY on its workspace's host (`<session>/<name>` there). Its record follows the
//! host's reports; `opened` remembers the host epoch it was last opened in, so a host restart
//! leads to `shell.open` (which reopens it in its last directory) before its next command.
//!
//! The user's own terminals are shell records too (see `terminals`); everything here that the
//! agent reaches looks at the agent's shells only.

use std::sync::Arc;

use controlroom_protocol::agent::{AgentShell, AgentShells, ShellEndParams, ShellNewParams};
use controlroom_protocol::host::{HostRequest, HostShell, ShellCloseParams};
use controlroom_protocol::ids::{HostOpId, MachineId, SessionId, ShellId};
use controlroom_protocol::session::{SessionState, ShellOwner, ShellState};
use controlroom_protocol::{ErrorData, Resource, RpcError};

use super::calls::open_shell;
use super::records::ShellRecord;
use crate::agent::op_id;
use crate::core::Core;
use crate::errors::{invalid_params, invalid_state, not_found};
use crate::runtime::{Inner, Runtime};

/// The longest shell name.
const MAX_NAME: usize = 64;

impl Runtime {
    /// `shell.open` (`shell_new`): opens a named shell in a workspace; an open shell with the
    /// name is returned unchanged.
    pub async fn shell_open(
        &self,
        id: SessionId,
        params: ShellNewParams,
    ) -> Result<AgentShell, RpcError> {
        self.recovered().await;
        let inner = self.inner();
        let (machine, workspace, cwd, exists) = inner.with_core(|core| {
            let session = core.session(id)?;
            if session.data.closed() {
                return Err(invalid_state(SessionState::Closed, "shell.open"));
            }
            check_name(&params.name)?;
            let machine = core
                .machines
                .by_name(&params.machine)
                .filter(|m| session.data.workspaces.iter().any(|w| w.machine_id == *m))
                .ok_or_else(|| not_found(Resource::Workspace, &params.machine))?;
            if let Some(shell) = session
                .data
                .shells
                .iter()
                .find(|s| s.is_agents() && s.shell.name == params.name)
            {
                let on = session.data.workspace(shell.shell.workspace_id);
                if on.map(|w| w.machine_id) != Some(machine) {
                    return Err(invalid_params(format!(
                        "the shell {:?} already exists on another machine",
                        params.name
                    )));
                }
                return Ok((machine, shell.shell.workspace_id, None, true));
            }
            let on_machine = || {
                session
                    .data
                    .workspaces
                    .iter()
                    .filter(|w| w.machine_id == machine)
            };
            let workspace = params
                .cwd
                .as_ref()
                .and_then(|cwd| on_machine().find(|w| w.contains(cwd.as_str())))
                .or_else(|| on_machine().next())
                .ok_or_else(|| not_found(Resource::Workspace, &params.machine))?;
            Ok((machine, workspace.id, params.cwd.clone(), false))
        })?;
        if !exists {
            open_shell(inner, id, machine, workspace, params.name.clone(), cwd).await?;
        }
        inner.with_core(|core| {
            let session = core.session(id)?;
            let shell = session
                .data
                .shells
                .iter()
                .find(|s| s.is_agents() && s.shell.name == params.name)
                .ok_or_else(|| shell_gone(&params.name))?;
            Ok(AgentShell {
                machine: core.machines.name(&machine),
                shell: shell.shell.clone(),
            })
        })
    }

    /// `shell.list` (`shell_list`).
    pub async fn shell_list(&self, id: SessionId) -> Result<AgentShells, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            Ok(AgentShells {
                shells: core.session(id)?.info().shells,
            })
        })
    }

    /// `shell.close`: ends a shell (a workspace's default shell can't be closed).
    pub async fn shell_close(&self, id: SessionId, params: ShellEndParams) -> Result<(), RpcError> {
        let op = op_id(id, params.call_id.as_str());
        self.command("agent.shell.close", &op, &params, |core| {
            core.close_shell(id, &params.name)
        })
        .await
    }
}

/// `shell_gone`: the session has no shell with this name.
pub(crate) fn shell_gone(name: &str) -> RpcError {
    RpcError::new(
        ErrorData::ShellGone {
            shell: name.to_owned(),
        },
        format!("no shell {name:?}; open one with shell_new"),
    )
}

fn check_name(name: &str) -> Result<(), RpcError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(invalid_params(
            "a shell name has 1 to 64 letters, digits, '-', '_' or '.'",
        ))
    }
}

/// Opens the session's agent shells not yet opened in their host's current epoch (the user's
/// terminals are never reopened).
pub(crate) async fn open_shells(inner: Arc<Inner>, id: SessionId) {
    let due = inner.with_core(|core| {
        let Some(session) = core.sessions.get(&id) else {
            return Vec::new();
        };
        session
            .data
            .shells
            .iter()
            .filter(|s| s.is_agents())
            .filter_map(|s| {
                let machine = session.data.workspace(s.shell.workspace_id)?.machine_id;
                (s.opened.is_none() || s.opened != core.machines.epoch(&machine)).then(|| {
                    (
                        machine,
                        s.shell.workspace_id,
                        s.shell.name.clone(),
                        s.shell.cwd.clone(),
                    )
                })
            })
            .collect::<Vec<_>>()
    });
    let opens = due.into_iter().map(|(machine, workspace, name, cwd)| {
        let inner = inner.clone();
        async move {
            if let Err(e) =
                open_shell(&inner, id, machine, workspace, name.clone(), Some(cwd)).await
            {
                tracing::warn!("session {id}: opening shell {name}: {e}");
            }
        }
    });
    futures::future::join_all(opens).await;
}

impl Core {
    /// Records a shell the host opened (new or reopened). A user's terminal must still have its
    /// record (`shell_gone` if it was closed meanwhile).
    pub(crate) fn shell_opened(
        &mut self,
        id: SessionId,
        machine: MachineId,
        host: &HostShell,
    ) -> Result<(), RpcError> {
        let epoch = self.machines.epoch(&machine);
        let session = self.session_mut(id)?;
        let name = host.shell.name.clone();
        let shells = &mut session.data.shells;
        let index = match shells.iter().position(|s| s.shell.name == name) {
            Some(index) => index,
            None if host.shell.owner != ShellOwner::Agent => return Err(shell_gone(&name)),
            None => {
                shells.push(ShellRecord::new(
                    name,
                    host.shell.workspace_id,
                    host.shell.cwd.clone(),
                ));
                shells.len() - 1
            }
        };
        shells[index].update(host);
        shells[index].opened = epoch;
        Ok(())
    }

    /// `shell.changed` from a host. A user's terminal that is `gone` ended for good: its record
    /// goes.
    pub(crate) fn shell_changed(&mut self, host: &HostShell) -> Result<(), RpcError> {
        let Some((session, name)) = host.shell_id.as_str().split_once('/') else {
            return Ok(());
        };
        let Ok(id) = session.parse::<SessionId>() else {
            return Ok(());
        };
        let Ok(session) = self.session_mut(id) else {
            return Ok(());
        };
        let shells = &mut session.data.shells;
        let Some(index) = shells.iter().position(|s| s.shell.name == name) else {
            return Ok(());
        };
        if !shells[index].is_agents() && host.shell.state == ShellState::Gone {
            tracing::info!(session = %id, shell = name, "user terminal ended");
            shells.remove(index);
        } else {
            shells[index].update(host);
        }
        Ok(())
    }

    fn close_shell(&mut self, id: SessionId, name: &str) -> Result<(), RpcError> {
        let data = &self.session(id)?.data;
        let index = data
            .shells
            .iter()
            .position(|s| s.is_agents() && s.shell.name == name)
            .ok_or_else(|| shell_gone(name))?;
        if data.workspaces.iter().any(|w| w.shell == name) {
            return Err(invalid_params(
                "a workspace's default shell can't be closed",
            ));
        }
        let workspace = data.shells[index].shell.workspace_id;
        let machine = data
            .workspace(workspace)
            .map(|w| w.machine_id)
            .ok_or_else(|| not_found(Resource::Workspace, workspace))?;
        self.session_mut(id)?.data.shells.remove(index);
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

    /// Ends every shell of a closing session (the user's terminals are forgotten; the agent's
    /// reopen with the session).
    pub(crate) fn close_shells(&mut self, id: SessionId) -> Result<(), RpcError> {
        let data = &self.session(id)?.data;
        let shells: Vec<(MachineId, String)> = data
            .shells
            .iter()
            .filter_map(|s| {
                let machine = data.workspace(s.shell.workspace_id)?.machine_id;
                Some((machine, s.shell.name.clone()))
            })
            .collect();
        for (machine, name) in shells {
            self.host_send(
                machine,
                Some(id),
                HostOpId::new(format!("shell.close:{id}/{name}")),
                HostRequest::ShellClose(ShellCloseParams {
                    shell_id: ShellId::of(id, &name),
                }),
            );
        }
        let shells = &mut self.session_mut(id)?.data.shells;
        shells.retain(|s| s.is_agents());
        for shell in shells {
            shell.opened = None;
            shell.shell.state = ShellState::Gone;
        }
        Ok(())
    }
}
