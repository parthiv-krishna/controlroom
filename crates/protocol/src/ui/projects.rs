//! Projects and tasks (the kanban board).
//!
//! A project has no location: it is a named, colored group of tasks. A session belongs to the
//! projects of the tasks that link it (possibly several); a session no task links is
//! **Unassigned**. The controller computes the membership ([`super::FeedSession::project_ids`]);
//! clients derive nothing.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ChecklistItemId, ProjectId, SessionId, TaskId, UnixMillis};

/// A project: a named, colored group of tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Project {
    /// The project.
    pub id: ProjectId,
    /// Display name.
    pub name: String,
    /// A CSS color, e.g. `"#a78bfa"`.
    pub color: String,
}

/// Params of `projects.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectsCreateParams {
    /// Display name.
    pub name: String,
    /// A CSS color.
    pub color: String,
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

/// A kanban task. Sessions linked to the same task may see and message each other, and belong
/// to the task's projects.
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
    /// Linked sessions, possibly dangling (a deleted session renders as "missing session").
    pub links: Vec<SessionId>,
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
    pub links: Vec<SessionId>,
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
    /// The session to link or unlink.
    pub session_id: SessionId,
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
