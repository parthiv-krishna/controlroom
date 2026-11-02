//! Sandbox defaults (spec §11.6): `sandbox.profiles`, `sandbox.set_profile`, and the defaults
//! the controller fills into session create, resume and start fresh.
//!
//! A session's defaults combine its machine's profile and the profiles of every project it
//! belongs to (by location prefix or through a linked task): the strictest level (`strict`
//! when none is set), and the union of grants, domains and mediated commands. An explicit
//! session level is the daemon's to apply on top, in either direction.

use std::sync::Arc;

use controlroom_protocol::RemotePath;
use controlroom_protocol::RpcError;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, ProjectId, TaskId};
use controlroom_protocol::session::{MediationAction, SandboxLevel, SandboxSpec};
use controlroom_protocol::ui::{
    LocationPrefix, ProfileScope, SandboxProfilesResult, SandboxSetProfileParams,
};

use super::Conn;
use crate::errors::{invalid_params, not_found, store_error};
use crate::hub::Hub;

/// `sandbox.profiles`.
pub(super) fn profiles(conn: &Conn) -> Result<SandboxProfilesResult, RpcError> {
    Ok(SandboxProfilesResult {
        profiles: conn
            .hub
            .repo()
            .sandbox_profiles()
            .list()
            .map_err(store_error)?,
    })
}

/// `sandbox.set_profile`: applies to new sessions and restarts.
pub(super) fn set_profile(
    conn: &Conn,
    p: SandboxSetProfileParams,
) -> Result<SandboxProfilesResult, RpcError> {
    let repo = conn.hub.repo();
    match &p.scope {
        ProfileScope::Machine { machine_id } => {
            repo.machines()
                .get(*machine_id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(Resource::Machine, machine_id))?;
        }
        ProfileScope::Project { project_id } => {
            repo.projects()
                .get(*project_id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(Resource::Project, project_id))?;
        }
        ProfileScope::Unknown => return Err(invalid_params("unknown profile scope")),
    }
    repo.sandbox_profiles()
        .change(&p.scope, &p.change)
        .map_err(store_error)?;
    profiles(conn)
}

/// Resolves a leading `~` against the machine's home, and drops trailing slashes.
fn normalize(path: &str, home: Option<&str>) -> String {
    let expanded = match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.trim_end_matches('/'))
        }
        _ => path.to_owned(),
    };
    match expanded.trim_end_matches('/') {
        "" if expanded.starts_with('/') => "/".to_owned(),
        trimmed => trimmed.to_owned(),
    }
}

/// Whether a project location matches `cwd` on `machine` (a directory and everything below).
pub(crate) fn prefix_matches(
    prefix: &LocationPrefix,
    machine: MachineId,
    cwd: &RemotePath,
    home: Option<&str>,
) -> bool {
    if prefix.machine_id.is_some_and(|m| m != machine) {
        return false;
    }
    if prefix.path_prefix.as_str().is_empty() {
        return true;
    }
    let prefix = normalize(prefix.path_prefix.as_str(), home);
    let cwd = normalize(cwd.as_str(), home);
    prefix == "/" || cwd == prefix || cwd.starts_with(&format!("{prefix}/"))
}

/// The projects a session belongs to: by location, and through its linked tasks.
fn matching_projects(
    hub: &Hub,
    machine: MachineId,
    cwd: &RemotePath,
    tasks: &[TaskId],
) -> Result<Vec<ProjectId>, RpcError> {
    let repo = hub.repo();
    let home = hub
        .lock()
        .machines
        .get(&machine)
        .and_then(|e| e.record.home.clone());
    let mut out: Vec<ProjectId> = repo
        .projects()
        .list()
        .map_err(store_error)?
        .into_iter()
        .filter(|p| {
            p.location_prefixes
                .iter()
                .any(|l| prefix_matches(l, machine, cwd, home.as_deref()))
        })
        .map(|p| p.id)
        .collect();
    for task in tasks {
        if let Some(task) = repo.tasks().get(*task).map_err(store_error)? {
            for project in task.project_ids {
                if !out.contains(&project) {
                    out.push(project);
                }
            }
        }
    }
    Ok(out)
}

/// The sandbox defaults of a session at `cwd` on `machine`, linked to `tasks`.
pub(crate) fn defaults(
    hub: &Arc<Hub>,
    machine: MachineId,
    cwd: &RemotePath,
    tasks: &[TaskId],
) -> Result<SandboxSpec, RpcError> {
    let projects = matching_projects(hub, machine, cwd, tasks)?;
    let profiles = hub.repo().sandbox_profiles().list().map_err(store_error)?;
    let applies = |scope: &ProfileScope| match scope {
        ProfileScope::Machine { machine_id } => *machine_id == machine,
        ProfileScope::Project { project_id } => projects.contains(project_id),
        ProfileScope::Unknown => false,
    };
    let mut spec = SandboxSpec {
        level: SandboxLevel::Strict,
        grants: Vec::new(),
        domains: Vec::new(),
        mediated: Vec::new(),
    };
    let mut level: Option<SandboxLevel> = None;
    for profile in profiles.into_iter().filter(|p| applies(&p.scope)) {
        if let Some(l) = profile.level {
            level = Some(match level {
                Some(current) if l.is_looser_than(current) => current,
                _ => l,
            });
        }
        for grant in profile.grants {
            if !spec.grants.iter().any(|g| g.id == grant.id) {
                spec.grants.push(grant);
            }
        }
        for domain in profile.domains {
            if !spec.domains.contains(&domain) {
                spec.domains.push(domain);
            }
        }
        for mediated in profile.mediated {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(machine: Option<MachineId>, path: &str) -> LocationPrefix {
        LocationPrefix {
            machine_id: machine,
            path_prefix: path.into(),
        }
    }

    #[test]
    fn location_prefixes_match_directories_and_below() {
        let (m, other) = (MachineId::generate(), MachineId::generate());
        let home = Some("/home/alice");
        let cwd = RemotePath::from("/home/alice/src/app/web");
        assert!(prefix_matches(&prefix(None, "~/src/app"), m, &cwd, home));
        assert!(prefix_matches(
            &prefix(Some(m), "/home/alice/src/"),
            m,
            &cwd,
            home
        ));
        assert!(prefix_matches(&prefix(Some(m), ""), m, &cwd, home));
        assert!(!prefix_matches(&prefix(Some(other), ""), m, &cwd, home));
        assert!(!prefix_matches(&prefix(None, "~/src/ap"), m, &cwd, home));
        assert!(prefix_matches(
            &prefix(None, "~/src"),
            m,
            &"~/src/x".into(),
            None
        ));
        assert!(prefix_matches(&prefix(None, "/"), m, &cwd, home));
    }
}
