//! Projects and tasks (`projects.*`, `tasks.*`). Each handler writes the changed columns and
//! emits the result under the hub lock, so the feed order matches the store's.

use controlroom_protocol::error::Resource;
use controlroom_protocol::ids::{ProjectId, TaskId};
use controlroom_protocol::ui::{
    ChecklistAddParams, ChecklistRemoveParams, ChecklistUpdateParams, Priority, Project,
    ProjectRef, ProjectsCreateParams, ProjectsUpdateParams, StateEvent, Task, TaskRef, TaskStatus,
    TasksCreateParams, TasksLinkParams, TasksMoveParams, TasksUpdateParams,
};
use controlroom_protocol::{Ack, RpcError};

use super::Conn;
use crate::errors::{invalid_params, not_found, store_error};
use crate::store::{NewTask, StoreResult, Tasks};

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

pub(super) fn projects_create(conn: &Conn, p: ProjectsCreateParams) -> Result<Project, RpcError> {
    let mut st = conn.hub.lock();
    let project = conn
        .hub
        .repo()
        .projects()
        .insert(&p.name, &p.color, &p.location_prefixes)
        .map_err(store_error)?;
    st.emit(StateEvent::ProjectUpserted {
        project: project.clone(),
    });
    Ok(project)
}

pub(super) fn projects_update(conn: &Conn, p: ProjectsUpdateParams) -> Result<Project, RpcError> {
    let mut st = conn.hub.lock();
    let updated = conn
        .hub
        .repo()
        .projects()
        .update(
            p.project_id,
            p.name.as_deref(),
            p.color.as_deref(),
            p.location_prefixes.as_deref(),
        )
        .map_err(store_error)?;
    if !updated {
        return Err(not_found(Resource::Project, p.project_id));
    }
    let project = project(conn, p.project_id)?;
    st.emit(StateEvent::ProjectUpserted {
        project: project.clone(),
    });
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
        st.emit(StateEvent::TaskUpserted {
            task: conn.hub.task(task)?,
        });
    }
    st.emit(StateEvent::ProjectRemoved {
        project_id: p.project_id,
    });
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
    st.emit(StateEvent::TaskUpserted { task: task.clone() });
    Ok(task)
}

pub(super) fn tasks_create(conn: &Conn, p: TasksCreateParams) -> Result<Task, RpcError> {
    check_projects(conn, &p.project_ids)?;
    let order = p.order.map(finite).transpose()?;
    let mut st = conn.hub.lock();
    let task = conn
        .hub
        .repo()
        .tasks()
        .insert(NewTask {
            title: p.title,
            status: p.status.unwrap_or(TaskStatus::Backlog),
            order,
            priority: p.priority.unwrap_or(Priority::P2),
            project_ids: p.project_ids,
            notes: p.notes,
            checklist: p.checklist,
            links: p.links,
        })
        .map_err(store_error)?;
    st.emit(StateEvent::TaskUpserted { task: task.clone() });
    Ok(task)
}

pub(super) fn tasks_update(conn: &Conn, p: TasksUpdateParams) -> Result<Task, RpcError> {
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
    st.emit(StateEvent::TaskRemoved { task_id: p.task_id });
    Ok(Ack {})
}

/// Links can dangle: the machine or session need not exist.
pub(super) fn tasks_link(conn: &Conn, p: TasksLinkParams, link: bool) -> Result<Task, RpcError> {
    edit_task(conn, p.task_id, |tasks| {
        if link {
            tasks.link(p.task_id, &p.link)
        } else {
            tasks.unlink(p.task_id, &p.link)
        }
    })
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
