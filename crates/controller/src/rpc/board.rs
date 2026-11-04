//! Projects and tasks (`projects.*`, `tasks.*`) for both UI APIs. Each handler writes the
//! changed columns and emits the result to both feeds under the hub lock, so the feed order
//! matches the store's; it returns the stored row, which the dispatcher renders in its API's
//! shape.

use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{ProjectId, TaskId};
use controlroom_protocol::ui::next::Project;
use controlroom_protocol::ui::{
    ChecklistAddParams, ChecklistRemoveParams, ChecklistUpdateParams, ProjectRef, TaskRef,
    TasksMoveParams, TasksUpdateParams,
};
use controlroom_protocol::{Ack, RpcError};

use super::Conn;
use crate::errors::{invalid_params, not_found, store_error};
use crate::store::{NewTask, StoreResult, TaskLink, TaskRow, Tasks};

fn project(conn: &Conn, id: ProjectId) -> Result<Project, RpcError> {
    conn.hub
        .repo()
        .projects()
        .get(id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(Resource::Project, id))
}

/// Checks every project exists.
fn check_projects(conn: &Conn, ids: &[ProjectId]) -> Result<(), RpcError> {
    for id in ids {
        project(conn, *id)?;
    }
    Ok(())
}

fn finite(order: f64) -> Result<f64, RpcError> {
    if order.is_finite() {
        Ok(order)
    } else {
        Err(invalid_params("order must be a finite number"))
    }
}

pub(super) fn projects_create(conn: &Conn, name: &str, color: &str) -> Result<Project, RpcError> {
    let mut st = conn.hub.lock();
    let project = conn
        .hub
        .repo()
        .projects()
        .insert(name, color)
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
) -> Result<TaskRow, RpcError> {
    let mut st = conn.hub.lock();
    let repo = conn.hub.repo();
    if !write(&repo.tasks()).map_err(store_error)? {
        return Err(not_found(Resource::Task, id));
    }
    let task = conn.hub.task(id)?;
    conn.hub.emit_task(&mut st, &task);
    Ok(task)
}

pub(super) fn tasks_create(conn: &Conn, task: NewTask) -> Result<TaskRow, RpcError> {
    check_projects(conn, &task.project_ids)?;
    let task = NewTask {
        order: task.order.map(finite).transpose()?,
        ..task
    };
    let mut st = conn.hub.lock();
    let task = conn.hub.repo().tasks().insert(task).map_err(store_error)?;
    conn.hub.emit_task(&mut st, &task);
    Ok(task)
}

pub(super) fn tasks_update(conn: &Conn, p: TasksUpdateParams) -> Result<TaskRow, RpcError> {
    if let Some(projects) = &p.project_ids {
        check_projects(conn, projects)?;
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

pub(super) fn tasks_move(conn: &Conn, p: TasksMoveParams) -> Result<TaskRow, RpcError> {
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

/// Links can dangle: the session need not exist.
pub(super) fn tasks_link(conn: &Conn, task: TaskId, link: TaskLink) -> Result<TaskRow, RpcError> {
    edit_task(conn, task, |tasks| tasks.link(task, &link))
}

pub(super) fn tasks_unlink(
    conn: &Conn,
    task: TaskId,
    session: controlroom_protocol::ids::SessionId,
) -> Result<TaskRow, RpcError> {
    edit_task(conn, task, |tasks| tasks.unlink(task, session))
}

pub(super) fn checklist_add(conn: &Conn, p: ChecklistAddParams) -> Result<TaskRow, RpcError> {
    edit_task(conn, p.task_id, |tasks| {
        tasks.checklist_add(p.task_id, &p.text, p.index)
    })
}

pub(super) fn checklist_update(conn: &Conn, p: ChecklistUpdateParams) -> Result<TaskRow, RpcError> {
    conn.hub.task(p.task_id)?;
    edit_task(conn, p.task_id, |tasks| {
        tasks.checklist_update(p.task_id, p.item_id, p.text.as_deref(), p.done)
    })
    .map_err(|_| not_found(Resource::ChecklistItem, p.item_id))
}

pub(super) fn checklist_remove(conn: &Conn, p: ChecklistRemoveParams) -> Result<TaskRow, RpcError> {
    conn.hub.task(p.task_id)?;
    edit_task(conn, p.task_id, |tasks| {
        tasks.checklist_remove(p.task_id, p.item_id)
    })
    .map_err(|_| not_found(Resource::ChecklistItem, p.item_id))
}
