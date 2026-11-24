//! Sandbox defaults (spec §11.6): `sandbox.defaults`, `sandbox.set_defaults`, and the defaults
//! the controller fills into session create, resume and start fresh.
//!
//! A session's defaults combine the defaults of its workspaces' machines and of every project
//! it belongs to (through its linked tasks): the strictest level (`strict` when none is set),
//! and the union of grants, domains and mediated commands. An explicit session level is the
//! daemon's to apply on top, in either direction.

use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, ProjectId, TaskId};
use controlroom_protocol::session::DeviceRef;
use controlroom_protocol::session::{MediationAction, SandboxLevel, SandboxSpec};
use controlroom_protocol::ui::{
    DefaultsChange, DefaultsScope, SandboxDefaultsResult, SandboxSetDefaultsParams,
};

use super::{Conn, defaults_paths};
use crate::errors::{invalid_params, not_found, store_error, unsupported};
use crate::hub::Hub;

/// `sandbox.defaults`.
pub(super) fn list(conn: &Conn) -> Result<SandboxDefaultsResult, RpcError> {
    Ok(SandboxDefaultsResult {
        defaults: conn
            .hub
            .repo()
            .sandbox_defaults()
            .list()
            .map_err(store_error)?,
    })
}

/// `sandbox.set_defaults`: applies to new sessions and restarts.
pub(super) fn set(
    conn: &Conn,
    p: SandboxSetDefaultsParams,
) -> Result<SandboxDefaultsResult, RpcError> {
    let repo = conn.hub.repo();
    match &p.scope {
        DefaultsScope::Machine { machine_id } => {
            repo.machines()
                .get(*machine_id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(Resource::Machine, machine_id))?;
        }
        DefaultsScope::Project { project_id } => {
            repo.projects()
                .get(*project_id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(Resource::Project, project_id))?;
        }
        DefaultsScope::Profile { .. } => {
            return Err(unsupported(
                "profile sandbox defaults are not supported yet",
            ));
        }
        DefaultsScope::Unknown => return Err(invalid_params("unknown sandbox defaults scope")),
    }
    let change = defaults_paths::check_change(p.change)?;
    let device = conn.device_ref();
    if matches!(
        change,
        DefaultsChange::AddGrant { .. }
            | DefaultsChange::AddNever { .. }
            | DefaultsChange::RemoveNever { .. }
    ) {
        tracing::info!(scope = ?p.scope, ?change, device = %device_label(device.as_ref()), "sandbox defaults change");
    }
    repo.sandbox_defaults()
        .change(&p.scope, &change, device.as_ref())
        .map_err(store_error)?;
    list(conn)
}

/// The projects of `tasks`, in order.
fn task_projects(hub: &Hub, tasks: &[TaskId]) -> Result<Vec<ProjectId>, RpcError> {
    let mut out: Vec<ProjectId> = Vec::new();
    for task in tasks {
        if let Some(task) = hub.repo().tasks().get(*task).map_err(store_error)? {
            for project in task.project_ids {
                if !out.contains(&project) {
                    out.push(project);
                }
            }
        }
    }
    Ok(out)
}

/// The sandbox defaults of a session with workspaces on `machines`, linked to `tasks`.
pub(crate) fn defaults(
    hub: &Arc<Hub>,
    machines: &[MachineId],
    tasks: &[TaskId],
) -> Result<SandboxSpec, RpcError> {
    let projects = task_projects(hub, tasks)?;
    let entries = hub.repo().sandbox_defaults().list().map_err(store_error)?;
    let applies = |scope: &DefaultsScope| match scope {
        DefaultsScope::Machine { machine_id } => machines.contains(machine_id),
        DefaultsScope::Project { project_id } => projects.contains(project_id),
        // Sessions have no profile yet.
        DefaultsScope::Profile { .. } | DefaultsScope::Unknown => false,
    };
    let mut spec = SandboxSpec {
        level: SandboxLevel::Strict,
        grants: Vec::new(),
        domains: Vec::new(),
        mediated: Vec::new(),
        never: Vec::new(),
    };
    let mut level: Option<SandboxLevel> = None;
    for entry in entries.into_iter().filter(|e| applies(&e.scope)) {
        if let Some(l) = entry.level {
            level = Some(match level {
                Some(current) if l.is_looser_than(current) => current,
                _ => l,
            });
        }
        for grant in entry.grants {
            if !spec.grants.iter().any(|g| g.id == grant.id) {
                spec.grants.push(grant);
            }
        }
        for domain in entry.domains {
            if !spec.domains.contains(&domain) {
                spec.domains.push(domain);
            }
        }
        for path in entry.never {
            if !spec.never.contains(&path) {
                spec.never.push(path);
            }
        }
        for mediated in entry.mediated {
            match spec
                .mediated
                .iter_mut()
                .find(|m| m.command == mediated.command)
            {
                // Conflicting actions: the stricter one (deny) wins.
                Some(existing) => {
                    if mediated.action == MediationAction::Deny {
                        existing.action = MediationAction::Deny;
                    }
                }
                None => spec.mediated.push(mediated),
            }
        }
    }
    spec.level = level.unwrap_or(SandboxLevel::Strict);
    Ok(spec)
}

/// A device for the log: its label and id, or that there is none.
pub(super) fn device_label(device: Option<&DeviceRef>) -> String {
    device.map_or_else(
        || "an unregistered device".to_owned(),
        |d| format!("{} ({})", d.label, d.device_id),
    )
}
