//! `profiles.move` (C13): move machine entries, projects (with their tasks) and sessions to one
//! profile in one store transaction, after checking everything; with `dry_run` only the preview.
//!
//! - **Machines** take the target as their only profile, or add it with `share` (never with a
//!   private profile, and never while the entry runs at level off).
//! - **Projects** move with their tasks; a task also in a project that stays behind blocks it.
//! - **Sessions** move when every workspace's machine is in the target afterwards, and not while
//!   a run is active (`busy`). The agent daemon then moves each one (`session.move`, restarting a
//!   live agent). An agent daemon before profiles answers `unsupported`: that is logged and the
//!   move stands.
//! - **Task links and contact grants** that would cross profiles are removed.
//! - **Stranded:** open sessions that stay behind with a workspace on a machine that leaves their
//!   profile.
//!
//! Moving what is already in the target changes nothing, so a retry is safe.

use std::collections::BTreeSet;
use std::sync::Arc;

use controlroom_client_core::daemon::CallError;
use controlroom_protocol::agentd::SessionMoveParams;
use controlroom_protocol::agentd::methods::SessionMove;
use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{MachineId, OpId, ProfileId, SessionId, TaskId};
use controlroom_protocol::session::{SessionState, SessionSummary};
use controlroom_protocol::ui::{Profile, ProfilesMoveParams, ProfilesMoveResult, TaskLink};
use controlroom_protocol::{ErrorData, RpcError};

use super::{Conn, membership, passthrough, sandbox};
use crate::errors::{not_found, policy_denied, store_error};
use crate::hub::{Hub, HubState};
use crate::store::MoveWrite;

/// A checked move: what it writes and what it reports.
struct Plan {
    write: MoveWrite,
    result: ProfilesMoveResult,
    /// Machines that end up shared (checked for level off before anything is written).
    shared: Vec<MachineId>,
}

/// `profiles.move`.
pub(super) async fn run(
    conn: &Arc<Conn>,
    p: ProfilesMoveParams,
) -> Result<ProfilesMoveResult, RpcError> {
    let hub = &conn.hub;
    let target = hub.profile(p.profile_id)?;
    let plan = plan(hub, &target, &p)?;
    for machine in &plan.shared {
        membership::check_shareable(hub, target.id, *machine)?;
    }
    if p.dry_run {
        return Ok(plan.result);
    }
    apply(hub, &plan)?;
    tracing::info!(
        profile = %target.id,
        machines = plan.write.machines.len(),
        projects = plan.write.project_ids.len(),
        sessions = plan.result.session_ids.len(),
        device = %sandbox::device_label(conn.device_ref().as_ref()),
        "moved to a profile"
    );
    move_sessions(hub, target.id, &plan.result.session_ids).await?;
    Ok(plan.result)
}

/// Checks the move against the current state and works out what changes.
fn plan(hub: &Hub, target: &Profile, p: &ProfilesMoveParams) -> Result<Plan, RpcError> {
    let repo = hub.repo();
    let projects = repo.projects().list().map_err(store_error)?;
    let tasks = repo.tasks().list().map_err(store_error)?;
    let grants = repo.grants().list().map_err(store_error)?;
    let profiles = repo.profiles().list().map_err(store_error)?;
    let st = hub.lock();

    // Machines: their new profiles.
    let mut machines: Vec<(MachineId, Vec<ProfileId>)> = Vec::new();
    let mut shared = Vec::new();
    for id in unique(&p.machine_ids) {
        let current = st
            .machines
            .get(&id)
            .ok_or_else(|| not_found(Resource::Machine, id))?
            .record
            .profile_ids
            .clone();
        let new = if p.share {
            let mut new = current.clone();
            if !new.contains(&target.id) {
                new.push(target.id);
            }
            new
        } else {
            vec![target.id]
        };
        if new.len() > 1 {
            let private = new
                .iter()
                .filter_map(|id| profiles.iter().find(|p| p.id == *id))
                .find(|p| p.private);
            if let Some(private) = private {
                return Err(policy_denied(
                    private.id,
                    format!("{} is private; its machines can't be shared", private.name),
                ));
            }
            shared.push(id);
        }
        if new != current {
            machines.push((id, new));
        }
    }
    let after = |machine: MachineId| -> Vec<ProfileId> {
        machines
            .iter()
            .find(|(m, _)| *m == machine)
            .map_or_else(|| st.machine_profiles(machine).to_vec(), |(_, p)| p.clone())
    };

    // Projects and their tasks.
    let mut project_ids = Vec::new();
    for id in unique(&p.project_ids) {
        let project = projects
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| not_found(Resource::Project, id))?;
        if project.profile_id != Some(target.id) {
            project_ids.push(id);
        }
    }
    let mut task_ids: Vec<TaskId> = Vec::new();
    for task in tasks
        .iter()
        .filter(|t| t.project_ids.iter().any(|p| project_ids.contains(p)))
    {
        let behind = task.project_ids.iter().find_map(|id| {
            projects
                .iter()
                .find(|p| p.id == *id && !project_ids.contains(id))
                .filter(|p| p.profile_id != Some(target.id))
        });
        if let Some(behind) = behind {
            return Err(policy_denied(
                target.id,
                format!(
                    "the task {:?} is also in the project {:?}, which stays; unassign it from \
                     one first",
                    task.title, behind.name
                ),
            ));
        }
        if task.profile_id != Some(target.id) {
            task_ids.push(task.id);
        }
    }

    // Sessions.
    let mut session_ids: Vec<SessionId> = Vec::new();
    for id in unique(&p.session_ids) {
        let session = st.global.sessions.get(&id).ok_or_else(|| {
            RpcError::new(ErrorData::UnknownSession { id }, format!("no session {id}"))
        })?;
        if st.session_profile(session) == Some(target.id) {
            continue;
        }
        for w in &session.workspaces {
            if !after(w.machine_id).contains(&target.id) {
                let name = st.machine_label(w.machine_id);
                return Err(policy_denied(
                    target.id,
                    format!("{name} isn't in {}; move or share it too", target.name),
                ));
            }
        }
        if busy(session) {
            return Err(RpcError::new(
                ErrorData::Busy,
                format!("session {id} is running; try again once it is idle"),
            ));
        }
        session_ids.push(id);
    }

    // What would cross profiles afterwards.
    let session_after = |id: SessionId| -> Option<ProfileId> {
        if session_ids.contains(&id) {
            return Some(target.id);
        }
        st.global
            .sessions
            .get(&id)
            .and_then(|s| st.session_profile(s))
    };
    let mut unlinked = Vec::new();
    for task in &tasks {
        let task_moves = task_ids.contains(&task.id);
        let task_after = if task_moves {
            Some(target.id)
        } else {
            task.profile_id
        };
        for session in &task.links {
            if !task_moves && !session_ids.contains(session) {
                continue;
            }
            if let (Some(t), Some(s)) = (task_after, session_after(*session))
                && t != s
            {
                unlinked.push(TaskLink {
                    task_id: task.id,
                    session_id: *session,
                });
            }
        }
    }
    let revoked_grants: Vec<_> = grants
        .iter()
        .filter(|g| session_ids.contains(&g.from) || session_ids.contains(&g.to))
        .filter(|g| match (session_after(g.from), session_after(g.to)) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        })
        .map(|g| g.id)
        .collect();
    let stranded = stranded(&st, &machines, &session_ids);

    Ok(Plan {
        write: MoveWrite {
            profile_id: target.id,
            machines,
            project_ids,
            task_ids: task_ids.clone(),
            unlinked: unlinked.clone(),
            revoked_grants: revoked_grants.clone(),
        },
        result: ProfilesMoveResult {
            task_ids,
            session_ids,
            unlinked,
            revoked_grants,
            stranded,
        },
        shared,
    })
}

/// Open sessions staying behind with a workspace on a machine that leaves their profile.
fn stranded(
    st: &HubState,
    machines: &[(MachineId, Vec<ProfileId>)],
    moving: &[SessionId],
) -> Vec<SessionId> {
    st.global
        .sessions
        .values()
        .filter(|s| s.state != SessionState::Closed && !moving.contains(&s.id))
        .filter(|s| {
            let Some(profile) = st.session_profile(s) else {
                return false;
            };
            s.workspaces.iter().any(|w| {
                machines
                    .iter()
                    .any(|(m, after)| *m == w.machine_id && !after.contains(&profile))
            })
        })
        .map(|s| s.id)
        .collect()
}

/// Whether a run is active or the agent is starting (a session can't move then).
fn busy(session: &SessionSummary) -> bool {
    matches!(
        session.state,
        SessionState::Starting
            | SessionState::Working
            | SessionState::AwaitingPermission
            | SessionState::AwaitingInput
    )
}

/// `ids` without repeats, in order.
fn unique<T: Copy + Ord>(ids: &[T]) -> Vec<T> {
    let mut seen = BTreeSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// Writes the move and emits what changed; the agent daemon learns the machines' new profiles.
fn apply(hub: &Hub, plan: &Plan) -> Result<(), RpcError> {
    {
        let mut st = hub.lock();
        let repo = hub.repo();
        repo.moves().apply(&plan.write).map_err(store_error)?;
        for id in &plan.write.project_ids {
            if let Some(project) = repo.projects().get(*id).map_err(store_error)? {
                hub.emit_project(&mut st, &project);
            }
        }
        let changed: BTreeSet<TaskId> = plan
            .write
            .task_ids
            .iter()
            .copied()
            .chain(plan.write.unlinked.iter().map(|l| l.task_id))
            .collect();
        for id in changed {
            let task = hub.task(id)?;
            hub.emit_task(&mut st, &task);
        }
        for grant in &plan.write.revoked_grants {
            st.emit_grant_removed(*grant);
        }
    }
    for (machine, _) in &plan.write.machines {
        hub.reload_record(*machine)?;
    }
    Ok(())
}

/// Moves the sessions on the agent daemon, once it knows the machines' new profiles.
async fn move_sessions(
    hub: &Hub,
    profile: ProfileId,
    sessions: &[SessionId],
) -> Result<(), RpcError> {
    if sessions.is_empty() {
        return Ok(());
    }
    hub.outbound.barrier().await;
    let agentd = hub.agentd()?;
    for id in sessions {
        let params = SessionMoveParams {
            op_id: OpId::generate(),
            id: *id,
            profile_id: profile,
        };
        match agentd.call::<SessionMove>(&params).await {
            Ok(_) => {}
            Err(CallError::Rpc(e)) if matches!(e.data(), Some(ErrorData::Unsupported)) => {
                tracing::warn!(session = %id, "the agent daemon can't move sessions yet");
            }
            Err(e) => return Err(passthrough::agentd_error(e)),
        }
    }
    Ok(())
}
