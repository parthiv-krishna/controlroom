//! Projects and tasks (`projects.*`, `tasks.*`). Each handler writes the changed columns and
//! emits the result to the state feed under the hub lock, so the feed order matches the
//! store's.
//!
//! Projects and tasks belong to one profile each (the only one when omitted): a task's projects
//! are all in its profile (C3), and it links only sessions of its profile (C4). They change
//! profile only with `profiles.move`.

use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{ProfileId, ProjectId, SessionId, TaskId};
use controlroom_protocol::ui::{
    ChecklistAddParams, ChecklistRemoveParams, ChecklistUpdateParams, Priority, Profile, Project,
    ProjectRef, ProjectsCreateParams, Task, TaskRef, TaskStatus, TasksCreateParams,
    TasksMoveParams, TasksUpdateParams,
};
use controlroom_protocol::{Ack, RpcError};

use super::{Conn, membership};
use crate::errors::{invalid_params, not_found, policy_denied, store_error};
use crate::store::{NewTask, StoreResult, Tasks};

fn project(conn: &Conn, id: ProjectId) -> Result<Project, RpcError> {
    conn.hub
        .repo()
        .projects()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Project, id))
}

/// Checks every project exists and is in `profile` (C3).
fn check_projects(conn: &Conn, profile: &Profile, ids: &[ProjectId]) -> Result<(), RpcError> {
    for id in ids {
        let project = project(conn, *id)?;
        if project.profile_id != Some(profile.id) {
            return Err(policy_denied(
                profile.id,
                format!("the project {:?} isn't in {}", project.name, profile.name),
            ));
        }
    }
    Ok(())
}

/// A new task's profile: the one given, else its first project's, else the only one.
fn new_task_profile(
    conn: &Conn,
    profile_id: Option<ProfileId>,
    projects: &[ProjectId],
) -> Result<Profile, RpcError> {
    match (profile_id, projects.first()) {
        (None, Some(first)) => {
            let first = project(conn, *first)?;
            membership::profile_or_only(&conn.hub, first.profile_id)
        }
        (id, _) => membership::profile_or_only(&conn.hub, id),
    }
}

fn finite(order: f64) -> Result<f64, RpcError> {
    if order.is_finite() {
        Ok(order)
    } else {
        Err(invalid_params("order must be a finite number"))
    }
}

pub(super) fn projects_create(conn: &Conn, p: ProjectsCreateParams) -> Result<Project, RpcError> {
    let profile = membership::profile_or_only(&conn.hub, p.profile_id)?;
    let mut st = conn.hub.lock();
    let project = conn
        .hub
        .repo()
        .projects()
        .insert(&p.name, &p.color, profile.id)
        .map_err(store_error)?;
    conn.hub.emit_project(&mut st, &project);
    Ok(project)
}

pub(super) fn projects_update(
    conn: &Conn,
    id: ProjectId,
    name: Option<&str>,
    color: Option<&str>,
) -> Result<Project, RpcError> {
    let mut st = conn.hub.lock();
    let updated = conn
        .hub
        .repo()
        .projects()
        .update(id, name, color)
        .map_err(store_error)?;
    if !updated {
        return Err(not_found(Resource::Project, id));
    }
    let project = project(conn, id)?;
    conn.hub.emit_project(&mut st, &project);
    Ok(project)
}

/// Deleting a project unassigns its tasks; each changed task is emitted.
pub(super) fn projects_delete(conn: &Conn, p: ProjectRef) -> Result<Ack, RpcError> {
    let mut st = conn.hub.lock();
    let repo = conn.hub.repo();
    let assigned = repo.tasks().in_project(p.project_id).map_err(store_error)?;
    if !repo.projects().delete(p.project_id).map_err(store_error)? {
        return Err(not_found(Resource::Project, p.project_id));
    }
    for task in assigned {
        let task = conn.hub.task(task)?;
        conn.hub.emit_task(&mut st, &task);
    }
    conn.hub.emit_project_removed(&mut st, p.project_id);
    Ok(Ack {})
}

/// Applies a task write, then re-reads and emits the task, all under the hub lock;
/// `not_found` if the write found no task.
fn edit_task(
    conn: &Conn,
    id: TaskId,
    write: impl FnOnce(&Tasks<'_>) -> StoreResult<bool>,
) -> Result<Task, RpcError> {
    let mut st = conn.hub.lock();
    let repo = conn.hub.repo();
    if !write(&repo.tasks()).map_err(store_error)? {
        return Err(not_found(Resource::Task, id));
    }
    let task = conn.hub.task(id)?;
    conn.hub.emit_task(&mut st, &task);
    Ok(task)
}

pub(super) fn tasks_create(conn: &Conn, p: TasksCreateParams) -> Result<Task, RpcError> {
    let profile = new_task_profile(conn, p.profile_id, &p.project_ids)?;
    check_projects(conn, &profile, &p.project_ids)?;
    for session in &p.links {
        membership::check_session_in(&conn.hub, &profile, *session)?;
    }
    let task = NewTask {
        title: p.title,
        status: p.status.unwrap_or(TaskStatus::Backlog),
        order: p.order.map(finite).transpose()?,
        priority: p.priority.unwrap_or(Priority::P2),
        project_ids: p.project_ids,
        profile_id: profile.id,
        notes: p.notes,
        checklist: p.checklist,
        links: p.links,
    };
    let mut st = conn.hub.lock();
    let task = conn.hub.repo().tasks().insert(task).map_err(store_error)?;
    conn.hub.emit_task(&mut st, &task);
    Ok(task)
}

pub(super) fn tasks_update(conn: &Conn, p: TasksUpdateParams) -> Result<Task, RpcError> {
    if let Some(projects) = &p.project_ids {
        let profile = membership::task_profile(&conn.hub, &conn.hub.task(p.task_id)?)?;
        check_projects(conn, &profile, projects)?;
    }
    edit_task(conn, p.task_id, |tasks| {
        tasks.update(
            p.task_id,
            p.title.as_deref(),
            p.priority,
            p.notes.as_deref(),
            p.project_ids.as_deref(),
        )
    })
}

pub(super) fn tasks_move(conn: &Conn, p: TasksMoveParams) -> Result<Task, RpcError> {
    let order = finite(p.order)?;
    edit_task(conn, p.task_id, |tasks| {
        tasks.move_to(p.task_id, p.status, order)
    })
}

pub(super) fn tasks_delete(conn: &Conn, p: TaskRef) -> Result<Ack, RpcError> {
    let mut st = conn.hub.lock();
    if !conn
        .hub
        .repo()
        .tasks()
        .delete(p.task_id)
        .map_err(store_error)?
    {
        return Err(not_found(Resource::Task, p.task_id));
    }
    conn.hub.emit_task_removed(&mut st, p.task_id);
    Ok(Ack {})
}

/// Links can dangle: the session need not exist. A known session must be in the task's
/// profile (C4).
pub(super) fn tasks_link(conn: &Conn, task: TaskId, session: SessionId) -> Result<Task, RpcError> {
    let profile = membership::task_profile(&conn.hub, &conn.hub.task(task)?)?;
    membership::check_session_in(&conn.hub, &profile, session)?;
    edit_task(conn, task, |tasks| tasks.link(task, session))
}

pub(super) fn tasks_unlink(
    conn: &Conn,
    task: TaskId,
    session: SessionId,
) -> Result<Task, RpcError> {
    edit_task(conn, task, |tasks| tasks.unlink(task, session))
}

pub(super) fn checklist_add(conn: &Conn, p: ChecklistAddParams) -> Result<Task, RpcError> {
    edit_task(conn, p.task_id, |tasks| {
        tasks.checklist_add(p.task_id, &p.text, p.index)
    })
}

pub(super) fn checklist_update(conn: &Conn, p: ChecklistUpdateParams) -> Result<Task, RpcError> {
    conn.hub.task(p.task_id)?;
    edit_task(conn, p.task_id, |tasks| {
        tasks.checklist_update(p.task_id, p.item_id, p.text.as_deref(), p.done)
    })
    .map_err(|_| not_found(Resource::ChecklistItem, p.item_id))
}

pub(super) fn checklist_remove(conn: &Conn, p: ChecklistRemoveParams) -> Result<Task, RpcError> {
    conn.hub.task(p.task_id)?;
    edit_task(conn, p.task_id, |tasks| {
        tasks.checklist_remove(p.task_id, p.item_id)
    })
    .map_err(|_| not_found(Resource::ChecklistItem, p.item_id))
}
