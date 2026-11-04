//! A session's workspaces: creating a session on them, adding and removing one, refreshing
//! them after a host reconnects, and what the agent sees of them (`session.info`).
//!
//! `session.create` and `session.workspace_add` need every workspace's machine connected
//! (`machine_unavailable` otherwise) and open the workspaces on their hosts before they commit
//! (`workspace.open` resolves the directory, writes its sandbox profile and returns the
//! instruction files and skills), so a bad path fails the command and the agent's first run
//! already sees its workspaces. Each workspace gets a default shell: named after the machine
//! when the session has one workspace there, else `<machine>-<dir>`; shells are opened on their
//! hosts right after the commit.

use std::sync::Arc;
use std::time::Duration;

use controlroom_protocol::agent::{AgentShell, SessionInfo, WorkspaceInfo};
use controlroom_protocol::agentd::{
    HostOpRequest, SessionCreateParams, SessionCreateResult, WorkspaceAddParams,
    WorkspaceRemoveParams,
};
use controlroom_protocol::host::{
    HostRequest, WorkspaceCloseParams, WorkspaceOpenParams, WorkspaceOpenResult,
};
use controlroom_protocol::ids::{HostOpId, MachineId, SessionId, WorkspaceId};
use controlroom_protocol::session::{
    JobStatus, SandboxLevel, SandboxSpec, ShellState, Workspace, WorkspaceSpec,
};
use controlroom_protocol::timeline::InjectOrigin;
use controlroom_protocol::{ErrorData, RpcError};

use super::calls::parse;
use super::machines::Machines;
use super::records::{ShellRecord, WorkspaceRecord};
use crate::commands::params_hash;
use crate::core::{Core, Task};
use crate::errors::{busy, invalid_params, invalid_state, not_found};
use crate::model::{Injection, SessionData};
use crate::runtime::{Inner, Runtime};
use crate::session::Session;

/// How long `workspace.open` may take while its machine is connected.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);

impl Runtime {
    /// `session.create`: opens the workspaces, then creates the session and starts its agent.
    pub async fn create(
        &self,
        params: SessionCreateParams,
    ) -> Result<SessionCreateResult, RpcError> {
        const METHOD: &str = "session.create";
        self.recovered().await;
        let inner = self.inner();
        let hash = params_hash(&params);
        if let Some(result) =
            inner.with_core(|core| core.stored_op(METHOD, &params.op_id, &hash))?
        {
            return Ok(result);
        }
        let kind = inner.agents.kind()?;
        if let Some(parent) = params.parent_id {
            inner.with_core(|core| core.session(parent).map(|_| ()))?;
        }
        let id = SessionId::generate();
        let sandbox = effective_sandbox(params.sandbox_level, params.sandbox.as_ref());
        let workspaces = inner.with_core(|core| {
            let now = core.clock.now();
            let specs = dedupe(&params.workspaces);
            if specs.is_empty() {
                return Err(invalid_params("a session needs at least one workspace"));
            }
            new_workspaces(&core.machines, &[], &specs, now)
        })?;
        let workspaces = open_all(inner, id, workspaces, &sandbox).await?;
        let created = self
            .command(METHOD, &params.op_id, &params, |core| {
                core.create(&params, kind, id, workspaces.clone())
            })
            .await;
        if created.is_err() {
            close_all(inner, id, &workspaces);
        }
        created
    }

    /// `session.workspace_add`.
    pub async fn workspace_add(&self, params: WorkspaceAddParams) -> Result<Workspace, RpcError> {
        const METHOD: &str = "session.workspace_add";
        self.recovered().await;
        let inner = self.inner();
        let hash = params_hash(&params);
        if let Some(result) =
            inner.with_core(|core| core.stored_op(METHOD, &params.op_id, &hash))?
        {
            return Ok(result);
        }
        let id = params.id;
        let (record, sandbox) = inner.with_core(|core| {
            let data = &core.session(id)?.data;
            if data.closed() {
                return Err(invalid_state(
                    controlroom_protocol::session::SessionState::Closed,
                    METHOD,
                ));
            }
            let now = core.clock.now();
            let mut records = new_workspaces(
                &core.machines,
                &data.workspaces,
                std::slice::from_ref(&params.workspace),
                now,
            )?;
            Ok((records.remove(0), data.effective_sandbox()))
        })?;
        let mut opened = open_all(inner, id, vec![record], &sandbox).await?;
        let record = opened.remove(0);
        let added = self
            .command(METHOD, &params.op_id, &params, |core| {
                core.add_workspace(id, record.clone())
            })
            .await;
        if added.is_err() {
            close_all(inner, id, std::slice::from_ref(&record));
        }
        added
    }

    /// `session.workspace_remove`.
    pub async fn workspace_remove(&self, params: WorkspaceRemoveParams) -> Result<(), RpcError> {
        self.command("session.workspace_remove", &params.op_id, &params, |core| {
            core.remove_workspace(params.id, params.workspace_id)
        })
        .await
    }

    /// `session.info`: where the session's tools run.
    pub async fn session_info(&self, id: SessionId) -> Result<SessionInfo, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| Ok(core.session(id)?.info()))
    }
}

/// The level the session runs at, with its defaults (the workspaces' profiles get it).
fn effective_sandbox(level: Option<SandboxLevel>, spec: Option<&SandboxSpec>) -> SandboxSpec {
    let mut sandbox = spec.cloned().unwrap_or_else(default_sandbox);
    if let Some(level) = level {
        sandbox.level = level;
    }
    sandbox
}

/// The sandbox defaults of a session created without any: strict, no extra grants.
pub(crate) fn default_sandbox() -> SandboxSpec {
    SandboxSpec {
        level: SandboxLevel::Strict,
        grants: Vec::new(),
        domains: Vec::new(),
        mediated: Vec::new(),
    }
}

impl SessionData {
    /// The sandbox the workspaces' profiles are written with: the defaults at the level the
    /// session runs at.
    pub(crate) fn effective_sandbox(&self) -> SandboxSpec {
        effective_sandbox(self.sandbox_level, Some(&self.sandbox))
    }

    /// The workspace with this id.
    pub(crate) fn workspace(&self, workspace: WorkspaceId) -> Option<&WorkspaceRecord> {
        self.workspaces.iter().find(|w| w.id == workspace)
    }
}

/// The specs without duplicates, in order.
fn dedupe(specs: &[WorkspaceSpec]) -> Vec<WorkspaceSpec> {
    let mut out: Vec<WorkspaceSpec> = Vec::new();
    for spec in specs {
        if !out.contains(spec) {
            out.push(spec.clone());
        }
    }
    out
}

/// New workspace records next to `existing`, with their default shell names. Every machine must
/// be connected.
fn new_workspaces(
    machines: &Machines,
    existing: &[WorkspaceRecord],
    specs: &[WorkspaceSpec],
    now: controlroom_protocol::UnixMillis,
) -> Result<Vec<WorkspaceRecord>, RpcError> {
    let mut records: Vec<WorkspaceRecord> = Vec::new();
    for spec in specs {
        machines.require_connected(&spec.machine_id)?;
        if existing
            .iter()
            .any(|w| w.machine_id == spec.machine_id && w.path == spec.path)
        {
            return Err(invalid_params("the session already has this workspace"));
        }
        records.push(WorkspaceRecord {
            id: WorkspaceId::generate(),
            machine_id: spec.machine_id,
            path: spec.path.clone(),
            root: None,
            home: None,
            added_at: now,
            shell: String::new(),
            context_files: Vec::new(),
            skills: Vec::new(),
        });
    }
    let mut taken: Vec<String> = existing.iter().map(|w| w.shell.clone()).collect();
    let all: Vec<&WorkspaceRecord> = existing.iter().chain(records.iter()).collect();
    let names: Vec<String> = records
        .iter()
        .map(|record| {
            let shared = all
                .iter()
                .filter(|w| w.machine_id == record.machine_id)
                .count()
                > 1;
            let name = unique(shell_name(machines, record, shared), &taken);
            taken.push(name.clone());
            name
        })
        .collect();
    for (record, name) in records.iter_mut().zip(names) {
        record.shell = name;
    }
    Ok(records)
}

/// The machine's name, or `<machine>-<dir>` when the session has several workspaces there.
fn shell_name(machines: &Machines, record: &WorkspaceRecord, shared: bool) -> String {
    let machine = machines.name(&record.machine_id);
    if !shared {
        return machine;
    }
    let dir = record
        .path
        .as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|d| !d.is_empty() && *d != "~")
        .unwrap_or("home");
    format!("{machine}-{dir}")
}

/// `name`, or `name-2`, `name-3`, ... when taken.
pub(crate) fn unique(name: String, taken: &[String]) -> String {
    if !taken.contains(&name) {
        return name;
    }
    (2..)
        .map(|n| format!("{name}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(name)
}

/// Opens the workspaces on their hosts, all at once; on a failure the opened ones are closed
/// again and the first error is returned.
async fn open_all(
    inner: &Arc<Inner>,
    id: SessionId,
    workspaces: Vec<WorkspaceRecord>,
    sandbox: &SandboxSpec,
) -> Result<Vec<WorkspaceRecord>, RpcError> {
    let opens = workspaces
        .iter()
        .map(|w| open_workspace(inner, id, w, sandbox.clone()));
    let results = futures::future::join_all(opens).await;
    let mut opened = Vec::new();
    let mut error = None;
    for (mut record, result) in workspaces.into_iter().zip(results) {
        match result {
            Ok(open) => {
                apply_open(&mut record, open);
                opened.push(record);
            }
            Err(e) => {
                error.get_or_insert(e);
            }
        }
    }
    if let Some(error) = error {
        close_all(inner, id, &opened);
        return Err(error);
    }
    Ok(opened)
}

/// `workspace.open` on the workspace's host, bounded by [`OPEN_TIMEOUT`]; fails with
/// `machine_unavailable` as soon as the machine is no longer connected (see
/// `Core::fail_opens`).
pub(crate) async fn open_workspace(
    inner: &Arc<Inner>,
    id: SessionId,
    workspace: &WorkspaceRecord,
    sandbox: SandboxSpec,
) -> Result<WorkspaceOpenResult, RpcError> {
    let key = HostOpId::new(format!("workspace.open:{}", workspace.id));
    let response = inner.bridge.request(HostOpRequest {
        id: key.clone(),
        machine_id: workspace.machine_id,
        session_id: Some(id),
        request: HostRequest::WorkspaceOpen(WorkspaceOpenParams {
            workspace_id: workspace.id,
            session_id: id,
            path: workspace.path.clone(),
            sandbox,
        }),
    });
    // A drop reported before the request was queued didn't fail it: check again.
    if let Err(e) = inner.with_core(|core| core.machines.require_connected(&workspace.machine_id)) {
        inner.bridge.withdraw(&key);
        return Err(e);
    }
    match tokio::time::timeout(OPEN_TIMEOUT, response).await {
        Ok(Ok(outcome)) => parse(outcome?),
        Ok(Err(_)) => Err(RpcError::new(ErrorData::Busy, "the open was withdrawn")),
        Err(_) => {
            inner.bridge.withdraw(&key);
            Err(RpcError::new(
                ErrorData::Timeout,
                format!("opening {} timed out", workspace.path),
            ))
        }
    }
}

/// Takes what `workspace.open` found.
pub(crate) fn apply_open(record: &mut WorkspaceRecord, open: WorkspaceOpenResult) {
    record.root = Some(open.root);
    record.home = Some(open.home);
    record.context_files = open.context_files;
    record.skills = open.skills;
}

fn close_all(inner: &Arc<Inner>, id: SessionId, workspaces: &[WorkspaceRecord]) {
    for workspace in workspaces {
        inner
            .bridge
            .send(close_request(id, workspace.machine_id, workspace.id));
    }
}

/// `workspace.close` (stops the workspace's shells and jobs on its host).
pub(crate) fn close_request(
    id: SessionId,
    machine: MachineId,
    workspace: WorkspaceId,
) -> HostOpRequest {
    HostOpRequest {
        id: HostOpId::new(format!("workspace.close:{workspace}")),
        machine_id: machine,
        session_id: Some(id),
        request: HostRequest::WorkspaceClose(WorkspaceCloseParams {
            workspace_id: workspace,
        }),
    }
}

impl Core {
    /// Adds an opened workspace and its default shell, and tells the agent.
    fn add_workspace(
        &mut self,
        id: SessionId,
        record: WorkspaceRecord,
    ) -> Result<Workspace, RpcError> {
        let machine = self.machines.name(&record.machine_id);
        let shell = ShellRecord::new(
            record.shell.clone(),
            record.id,
            record.root.clone().unwrap_or_else(|| record.path.clone()),
        );
        let text = format!(
            "[controlroom] The user added the workspace {machine}:{} (root {machine}:{}, default \
             shell \"{}\"). Your system prompt lists it from your next turn on.",
            record.path,
            record.root.as_ref().unwrap_or(&record.path),
            record.shell
        );
        let session = self.session_mut(id)?;
        session.data.workspaces.push(record.clone());
        session.data.shells.push(shell);
        self.task(Task::OpenShells(id));
        self.workspace_notice(id, text)?;
        let availability = self.machines.availability(&record.machine_id);
        Ok(record.to_workspace(false, availability))
    }

    /// Drops a workspace that isn't busy (no running command or job), closing it on its host.
    fn remove_workspace(&mut self, id: SessionId, workspace: WorkspaceId) -> Result<(), RpcError> {
        let data = &self.session(id)?.data;
        let index = data
            .workspaces
            .iter()
            .position(|w| w.id == workspace)
            .ok_or_else(|| not_found(controlroom_protocol::Resource::Workspace, workspace))?;
        if index == 0 {
            return Err(invalid_params("the primary workspace can't be removed"));
        }
        let shell_busy = data.shells.iter().any(|s| {
            s.shell.workspace_id == workspace
                && matches!(
                    s.shell.state,
                    ShellState::Running { .. } | ShellState::WaitingInput { .. }
                )
        });
        let job_busy = data
            .jobs
            .values()
            .any(|j| j.workspace_id == workspace && j.status() == JobStatus::Running);
        if shell_busy || job_busy {
            return Err(busy("a command or job is running in this workspace"));
        }
        let record = data.workspaces[index].clone();
        let machine = self.machines.name(&record.machine_id);
        let session = self.session_mut(id)?;
        session.data.workspaces.remove(index);
        session
            .data
            .shells
            .retain(|s| s.shell.workspace_id != workspace);
        self.effect(crate::core::HostEffect::Send(close_request(
            id,
            record.machine_id,
            workspace,
        )));
        self.workspace_notice(
            id,
            format!(
                "[controlroom] The user removed the workspace {machine}:{} and its shells; don't \
                 use paths there any more.",
                record.path
            ),
        )
    }

    /// A notice for the agent's next turn about its workspaces (doesn't wake it).
    pub(crate) fn workspace_notice(&mut self, id: SessionId, text: String) -> Result<(), RpcError> {
        let now = self.clock.now();
        let injection = Injection::new(
            InjectOrigin::Workspace,
            "workspaces".into(),
            text,
            false,
            now,
        );
        self.inject(id, injection)
    }

    /// Takes a refreshed `workspace.open` (after a host reconnected).
    pub(crate) fn workspace_refreshed(
        &mut self,
        id: SessionId,
        workspace: WorkspaceId,
        open: WorkspaceOpenResult,
    ) -> Result<(), RpcError> {
        let Ok(session) = self.session_mut(id) else {
            return Ok(());
        };
        if let Some(record) = session
            .data
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
        {
            apply_open(record, open);
        }
        Ok(())
    }
}

impl Session {
    /// What the agent sees of its workspaces and shells.
    pub(crate) fn info(&self) -> SessionInfo {
        let machines = &self.machines;
        let data = &self.data;
        SessionInfo {
            workspaces: data
                .workspaces
                .iter()
                .enumerate()
                .map(|(index, w)| WorkspaceInfo {
                    machine: machines.name(&w.machine_id),
                    path: w.path.clone(),
                    root: w.root.clone(),
                    home: w.home.clone(),
                    primary: index == 0,
                    shell: w.shell.clone(),
                    availability: machines.availability(&w.machine_id),
                    context_files: w.context_files.clone(),
                    skills: w.skills.clone(),
                })
                .collect(),
            shells: data
                .shells
                .iter()
                .map(|s| AgentShell {
                    machine: data
                        .workspace(s.shell.workspace_id)
                        .map(|w| machines.name(&w.machine_id))
                        .unwrap_or_default(),
                    shell: s.shell.clone(),
                })
                .collect(),
        }
    }

    /// The workspaces as clients see them.
    pub(crate) fn workspaces(&self) -> Vec<Workspace> {
        self.data
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, w)| w.to_workspace(index == 0, self.machines.availability(&w.machine_id)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::agentd::HostMachine;
    use controlroom_protocol::ids::{RemotePath, UnixMillis};
    use controlroom_protocol::session::Availability;
    use controlroom_protocol::ui::Connectivity;

    use super::*;

    fn machines(names: &[&str]) -> (Machines, Vec<MachineId>) {
        let ids: Vec<MachineId> = names.iter().map(|_| MachineId::generate()).collect();
        let list = names
            .iter()
            .zip(&ids)
            .map(|(name, id)| {
                (
                    *id,
                    HostMachine {
                        machine_id: *id,
                        name: (*name).into(),
                        availability: Availability {
                            connectivity: Connectivity::Connected,
                            since: UnixMillis(1),
                        },
                        epoch: None,
                        grace_s: 600,
                    },
                )
            })
            .collect();
        (
            Machines {
                list,
                unknown_since: UnixMillis(0),
            },
            ids,
        )
    }

    fn spec(machine: MachineId, path: &str) -> WorkspaceSpec {
        WorkspaceSpec {
            machine_id: machine,
            path: RemotePath::new(path),
        }
    }

    #[test]
    fn default_shells_are_named_after_machines_and_dirs() {
        let (machines, ids) = machines(&["devbox", "buildbox"]);
        let records = new_workspaces(
            &machines,
            &[],
            &[
                spec(ids[0], "~/src/api"),
                spec(ids[1], "~/work/web"),
                spec(ids[0], "~/src/web/"),
            ],
            UnixMillis(5),
        )
        .unwrap();
        let names: Vec<&str> = records.iter().map(|r| r.shell.as_str()).collect();
        assert_eq!(names, ["devbox-api", "buildbox", "devbox-web"]);
        let more = new_workspaces(
            &machines,
            &records,
            &[spec(ids[1], "/srv/web")],
            UnixMillis(6),
        )
        .unwrap();
        assert_eq!(more[0].shell, "buildbox-web");
        assert!(
            new_workspaces(
                &machines,
                &records,
                &[spec(ids[1], "~/work/web")],
                UnixMillis(6)
            )
            .is_err()
        );
        let err = new_workspaces(
            &machines,
            &[],
            &[spec(MachineId::generate(), "/")],
            UnixMillis(6),
        )
        .unwrap_err();
        assert!(matches!(
            err.data,
            Some(ErrorData::MachineUnavailable { .. })
        ));
        assert_eq!(unique("a".into(), &["a".into(), "a-2".into()]), "a-3");
    }
}
