//! Projects and tasks (the kanban board).
//!
//! **Deprecated** with the old UI method table: location prefixes and machine-scoped
//! [`SessionLink`]s go away. [`crate::ui::next`] has projects as task groups and tasks that link
//! global session ids. [`TaskStatus`], [`Priority`], [`ChecklistItem`] and the checklist and
//! move params stay.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{
    ChecklistItemId, MachineId, ProjectId, RemotePath, SessionId, TaskId, UnixMillis,
};

/// One location filter of a project; matches the directory and everything below it.
///
/// - machine + path: that directory on that machine;
/// - no machine + path (e.g. `~/src/app`): that path on any machine;
/// - machine + empty path: the whole machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct LocationPrefix {
    /// The machine, or `null` for any machine.
    pub machine_id: Option<MachineId>,
    /// The path prefix; empty for the whole machine.
    pub path_prefix: RemotePath,
}

/// A project: a saved view over sessions and tasks. Tasks belong to it when assigned to it;
/// sessions when their location matches a prefix or they are linked to one of its tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Project {
    /// The project.
    pub id: ProjectId,
    /// Display name.
    pub name: String,
    /// A CSS color, e.g. `"#a78bfa"`.
    pub color: String,
    /// Sessions at these locations belong to the project.
    pub location_prefixes: Vec<LocationPrefix>,
}

/// Params of `projects.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectsCreateParams {
    /// Display name.
    pub name: String,
    /// A CSS color.
    pub color: String,
    /// Location filters.
    pub location_prefixes: Vec<LocationPrefix>,
}

/// Params of `projects.update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectsUpdateParams {
    /// The project.
    pub project_id: ProjectId,
    /// New name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// New location filters (replaces the list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_prefixes: Option<Vec<LocationPrefix>>,
}

/// Params of `projects.delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectRef {
    /// The project.
    pub project_id: ProjectId,
}

/// A task's kanban column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Backlog.
    Backlog,
    /// Up next.
    Todo,
    /// In progress.
    Doing,
    /// In review.
    Review,
    /// Done.
    Done,
    /// A column this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A task's priority, `p0` (highest) to `p3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Highest.
    P0,
    /// High.
    P1,
    /// Normal.
    P2,
    /// Low.
    P3,
    /// A priority this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One checklist entry of a task. Can seed a session's todo list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChecklistItem {
    /// The entry.
    pub id: ChecklistItemId,
    /// The text.
    pub text: String,
    /// Whether it's ticked.
    pub done: bool,
}

/// A link from a controller object (a task, a contact grant) to a session. Never
/// cascade-deleted: a link whose session or machine is gone renders as "missing session".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct SessionLink {
    /// The session's machine.
    pub machine_id: MachineId,
    /// The session.
    pub session_id: SessionId,
}

/// A kanban task. Sessions linked to the same task may see and message each other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Task {
    /// The task.
    pub id: TaskId,
    /// Title.
    pub title: String,
    /// Column.
    pub status: TaskStatus,
    /// Sort key within the column, ascending (fractional indexing).
    pub order: f64,
    /// Priority.
    pub priority: Priority,
    /// Assigned projects, in assignment order.
    pub project_ids: Vec<ProjectId>,
    /// Free-text notes.
    pub notes: String,
    /// Checklist.
    pub checklist: Vec<ChecklistItem>,
    /// Linked sessions, possibly dangling.
    pub links: Vec<SessionLink>,
    /// When created.
    pub created_at: UnixMillis,
    /// When last changed.
    pub updated_at: UnixMillis,
}

/// Params of `tasks.create`. Absent fields take the defaults noted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct TasksCreateParams {
    /// Title.
    pub title: String,
    /// Column; default `backlog`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    /// Sort key; default: the end of the column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
    /// Priority; default `p2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Assigned projects; default none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_ids: Vec<ProjectId>,
    /// Notes; default empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Checklist entry texts; default none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<String>,
    /// Linked sessions; default none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<SessionLink>,
}

/// Params of `tasks.update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TasksUpdateParams {
    /// The task.
    pub task_id: TaskId,
    /// New title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// New priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// New assigned projects (replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<ProjectId>>,
    /// New notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Params of `tasks.move`: change column and position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct TasksMoveParams {
    /// The task.
    pub task_id: TaskId,
    /// The target column.
    pub status: TaskStatus,
    /// The new sort key.
    pub order: f64,
}

/// Params of `tasks.delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TaskRef {
    /// The task.
    pub task_id: TaskId,
}

/// Params of `tasks.link` and `tasks.unlink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TasksLinkParams {
    /// The task.
    pub task_id: TaskId,
    /// The session link to add or remove.
    #[serde(flatten)]
    pub link: SessionLink,
}

/// Params of `tasks.checklist_add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChecklistAddParams {
    /// The task.
    pub task_id: TaskId,
    /// The new entry's text (starts unticked).
    pub text: String,
    /// Insert position; default: the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

/// Params of `tasks.checklist_update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChecklistUpdateParams {
    /// The task.
    pub task_id: TaskId,
    /// The entry.
    pub item_id: ChecklistItemId,
    /// New text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// New ticked state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

/// Params of `tasks.checklist_remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChecklistRemoveParams {
    /// The task.
    pub task_id: TaskId,
    /// The entry.
    pub item_id: ChecklistItemId,
}
