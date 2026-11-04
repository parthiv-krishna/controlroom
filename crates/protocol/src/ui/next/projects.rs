//! Projects as groups of tasks, and tasks that link global sessions.
//!
//! A project has no location: it groups tasks. A session belongs to the projects of the tasks
//! that link it (possibly several); a session no task links is **Unassigned**. The controller
//! computes the membership ([`super::FeedSession::project_ids`]); clients derive nothing.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ProjectId, SessionId, TaskId, UnixMillis};
use crate::ui::{ChecklistItem, Priority, TaskStatus};

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

/// Params of `tasks.link` and `tasks.unlink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TasksLinkParams {
    /// The task.
    pub task_id: TaskId,
    /// The session to link or unlink.
    pub session_id: SessionId,
}
