//! Tasks (kanban items), their checklists, project assignments and session links.
//!
//! Links name a session without foreign keys: deleting a session never deletes them. Deleting
//! the task removes its links, checklist and assignments. Checklist entries are rows of their
//! own, so editing one entry never rewrites the others.

use std::collections::HashMap;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::{ChecklistItemId, ProjectId, SessionId, TaskId};
use controlroom_protocol::ui::{ChecklistItem, Priority, Task, TaskStatus};
use rusqlite::{Connection, OptionalExtension};

use super::{OwnerId, Store, StoreResult, enum_col, enum_str, parsed_col};

/// The fields of a new task.
#[derive(Debug, Clone)]
pub struct NewTask {
    /// Title.
    pub title: String,
    /// Column.
    pub status: TaskStatus,
    /// Sort key; `None` puts it at the end of its column.
    pub order: Option<f64>,
    /// Priority.
    pub priority: Priority,
    /// Assigned projects, in order.
    pub project_ids: Vec<ProjectId>,
    /// Notes.
    pub notes: String,
    /// Checklist texts.
    pub checklist: Vec<String>,
    /// Linked sessions.
    pub links: Vec<SessionId>,
}

/// Owner-scoped task operations.
pub struct Tasks<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "id, title, status, position, priority, notes, created_at, updated_at";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: parsed_col(row, 0)?,
        title: row.get(1)?,
        status: enum_col(row, 2)?,
        order: row.get(3)?,
        priority: enum_col(row, 4)?,
        project_ids: Vec::new(),
        notes: row.get(5)?,
        checklist: Vec::new(),
        links: Vec::new(),
        created_at: UnixMillis(row.get(6)?),
        updated_at: UnixMillis(row.get(7)?),
    })
}

/// Fills in the child rows (assignments, checklist, links) of `tasks`.
fn attach_children(conn: &Connection, owner: &OwnerId, tasks: &mut [Task]) -> StoreResult<()> {
    let index: HashMap<TaskId, usize> = tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let mut stmt = conn.prepare(
        "SELECT task_id, project_id FROM task_projects WHERE owner_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map([owner.as_str()], |r| {
        Ok((parsed_col::<TaskId>(r, 0)?, parsed_col::<ProjectId>(r, 1)?))
    })?;
    for row in rows {
        let (task, project) = row?;
        if let Some(&i) = index.get(&task) {
            tasks[i].project_ids.push(project);
        }
    }
    let mut stmt = conn.prepare(
        "SELECT task_id, item_id, text, done FROM task_checklist WHERE owner_id = ?1
         ORDER BY position, item_id",
    )?;
    let rows = stmt.query_map([owner.as_str()], |r| {
        Ok((
            parsed_col::<TaskId>(r, 0)?,
            ChecklistItem {
                id: parsed_col(r, 1)?,
                text: r.get(2)?,
                done: r.get(3)?,
            },
        ))
    })?;
    for row in rows {
        let (task, item) = row?;
        if let Some(&i) = index.get(&task) {
            tasks[i].checklist.push(item);
        }
    }
    let mut stmt = conn.prepare(
        "SELECT task_id, session_id FROM task_links WHERE owner_id = ?1
         ORDER BY created_at, rowid",
    )?;
    let rows = stmt.query_map([owner.as_str()], |r| {
        Ok((parsed_col::<TaskId>(r, 0)?, parsed_col::<SessionId>(r, 1)?))
    })?;
    for row in rows {
        let (task, link) = row?;
        if let Some(&i) = index.get(&task) {
            tasks[i].links.push(link);
        }
    }
    Ok(())
}

fn touch(conn: &Connection, owner: &OwnerId, id: TaskId) -> StoreResult<bool> {
    let n = conn.execute(
        "UPDATE tasks SET updated_at = ?3 WHERE owner_id = ?1 AND id = ?2",
        (owner.as_str(), id.to_string(), UnixMillis::now().0),
    )?;
    Ok(n > 0)
}

fn set_projects(
    conn: &Connection,
    owner: &OwnerId,
    task: TaskId,
    projects: &[ProjectId],
) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM task_projects WHERE owner_id = ?1 AND task_id = ?2",
        (owner.as_str(), task.to_string()),
    )?;
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO task_projects (owner_id, task_id, project_id, position)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (position, project) in projects.iter().enumerate() {
        stmt.execute(rusqlite::params![
            owner.as_str(),
            task.to_string(),
            project.to_string(),
            position as i64
        ])?;
    }
    Ok(())
}

fn add_link(
    conn: &Connection,
    owner: &OwnerId,
    task: TaskId,
    session: SessionId,
) -> StoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO task_links (owner_id, task_id, session_id, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            owner.as_str(),
            task.to_string(),
            session.to_string(),
            UnixMillis::now().0
        ],
    )?;
    Ok(())
}

fn add_item(
    conn: &Connection,
    owner: &OwnerId,
    task: TaskId,
    position: f64,
    text: &str,
) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO task_checklist (item_id, owner_id, task_id, position, text, done)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        rusqlite::params![
            ChecklistItemId::generate().to_string(),
            owner.as_str(),
            task.to_string(),
            position,
            text
        ],
    )?;
    Ok(())
}

impl<'a> Tasks<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Creates a task with its checklist, assignments and links. Projects must exist.
    pub fn insert(&self, task: NewTask) -> StoreResult<Task> {
        let id = TaskId::generate();
        let now = UnixMillis::now().0;
        let status = enum_str(&task.status)?;
        self.store.transaction(|tx| {
            let order: f64 = match task.order {
                Some(order) => order,
                None => tx.query_row(
                    "SELECT COALESCE(MAX(position) + 1, 0) FROM tasks
                     WHERE owner_id = ?1 AND status = ?2",
                    (self.owner.as_str(), &status),
                    |r| r.get(0),
                )?,
            };
            tx.execute(
                "INSERT INTO tasks (id, owner_id, title, status, position, priority, notes,
                                    created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                rusqlite::params![
                    id.to_string(),
                    self.owner.as_str(),
                    task.title,
                    status,
                    order,
                    enum_str(&task.priority)?,
                    task.notes,
                    now
                ],
            )?;
            set_projects(tx, self.owner, id, &task.project_ids)?;
            for (i, text) in task.checklist.iter().enumerate() {
                add_item(tx, self.owner, id, i as f64, text)?;
            }
            for session in &task.links {
                add_link(tx, self.owner, id, *session)?;
            }
            Ok(())
        })?;
        self.get(id)?
            .ok_or_else(|| super::StoreError::Conflict("the task vanished".into()))
    }

    /// A task by id, with its children.
    pub fn get(&self, id: TaskId) -> StoreResult<Option<Task>> {
        let conn = self.store.lock();
        let Some(task) = conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM tasks WHERE owner_id = ?1 AND id = ?2"),
                (self.owner.as_str(), id.to_string()),
                map,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut tasks = [task];
        attach_children(&conn, self.owner, &mut tasks)?;
        let [task] = tasks;
        Ok(Some(task))
    }

    /// Every task with its children.
    pub fn list(&self) -> StoreResult<Vec<Task>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM tasks WHERE owner_id = ?1 ORDER BY status, position, id"
        ))?;
        let mut tasks = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        attach_children(&conn, self.owner, &mut tasks)?;
        Ok(tasks)
    }

    /// Updates the given fields only; returns whether the task exists.
    pub fn update(
        &self,
        id: TaskId,
        title: Option<&str>,
        priority: Option<Priority>,
        notes: Option<&str>,
        project_ids: Option<&[ProjectId]>,
    ) -> StoreResult<bool> {
        let priority = priority.as_ref().map(enum_str).transpose()?;
        self.store.transaction(|tx| {
            let n = tx.execute(
                "UPDATE tasks SET title = COALESCE(?3, title), priority = COALESCE(?4, priority),
                     notes = COALESCE(?5, notes), updated_at = ?6
                 WHERE owner_id = ?1 AND id = ?2",
                rusqlite::params![
                    self.owner.as_str(),
                    id.to_string(),
                    title,
                    priority,
                    notes,
                    UnixMillis::now().0
                ],
            )?;
            if n > 0
                && let Some(projects) = project_ids
            {
                set_projects(tx, self.owner, id, projects)?;
            }
            Ok(n > 0)
        })
    }

    /// Moves a task to a column and position.
    pub fn move_to(&self, id: TaskId, status: TaskStatus, order: f64) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "UPDATE tasks SET status = ?3, position = ?4, updated_at = ?5
             WHERE owner_id = ?1 AND id = ?2",
            rusqlite::params![
                self.owner.as_str(),
                id.to_string(),
                enum_str(&status)?,
                order,
                UnixMillis::now().0
            ],
        )?;
        Ok(n > 0)
    }

    /// Moves a task in `from` to the end of `to`; returns whether it moved.
    pub fn move_if_in(&self, id: TaskId, from: &[TaskStatus], to: TaskStatus) -> StoreResult<bool> {
        let Some(task) = self.get(id)? else {
            return Ok(false);
        };
        if !from.contains(&task.status) {
            return Ok(false);
        }
        let to = enum_str(&to)?;
        let from: Vec<String> = from.iter().map(enum_str).collect::<StoreResult<_>>()?;
        self.store.transaction(|tx| {
            let order: f64 = tx.query_row(
                "SELECT COALESCE(MAX(position) + 1, 0) FROM tasks WHERE owner_id = ?1 AND status = ?2",
                (self.owner.as_str(), &to),
                |r| r.get(0),
            )?;
            // Only if it is still in one of `from` (a concurrent move wins).
            let n = tx.execute(
                &format!(
                    "UPDATE tasks SET status = ?3, position = ?4, updated_at = ?5
                     WHERE owner_id = ?1 AND id = ?2 AND status IN ({})",
                    from.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")
                ),
                rusqlite::params![
                    self.owner.as_str(),
                    id.to_string(),
                    to,
                    order,
                    UnixMillis::now().0
                ],
            )?;
            Ok(n > 0)
        })
    }

    /// Deletes a task with its links, checklist and assignments.
    pub fn delete(&self, id: TaskId) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "DELETE FROM tasks WHERE owner_id = ?1 AND id = ?2",
            (self.owner.as_str(), id.to_string()),
        )?;
        Ok(n > 0)
    }

    /// Links a session (which need not exist); returns whether the task exists. Linking it
    /// again keeps the first link.
    pub fn link(&self, id: TaskId, session: SessionId) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            if !touch(tx, self.owner, id)? {
                return Ok(false);
            }
            add_link(tx, self.owner, id, session)?;
            Ok(true)
        })
    }

    /// Unlinks a session; returns whether the task exists.
    pub fn unlink(&self, id: TaskId, session: SessionId) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            if !touch(tx, self.owner, id)? {
                return Ok(false);
            }
            tx.execute(
                "DELETE FROM task_links WHERE owner_id = ?1 AND task_id = ?2 AND session_id = ?3",
                rusqlite::params![self.owner.as_str(), id.to_string(), session.to_string()],
            )?;
            Ok(true)
        })
    }

    /// The tasks a session is linked to, oldest link first.
    pub fn linked_to(&self, session: SessionId) -> StoreResult<Vec<TaskId>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT task_id FROM task_links WHERE owner_id = ?1 AND session_id = ?2
             ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map((self.owner.as_str(), session.to_string()), |r| {
                parsed_col(r, 0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The tasks assigned to a project.
    pub fn in_project(&self, project: ProjectId) -> StoreResult<Vec<TaskId>> {
        let conn = self.store.lock();
        let mut stmt = conn
            .prepare("SELECT task_id FROM task_projects WHERE owner_id = ?1 AND project_id = ?2")?;
        let rows = stmt
            .query_map((self.owner.as_str(), project.to_string()), |r| {
                parsed_col(r, 0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Adds a checklist entry at `index` (default: the end); returns whether the task exists.
    pub fn checklist_add(&self, id: TaskId, text: &str, index: Option<u32>) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            if !touch(tx, self.owner, id)? {
                return Ok(false);
            }
            let mut stmt = tx.prepare(
                "SELECT position FROM task_checklist WHERE owner_id = ?1 AND task_id = ?2
                 ORDER BY position, item_id",
            )?;
            let positions = stmt
                .query_map((self.owner.as_str(), id.to_string()), |r| {
                    r.get::<_, f64>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let at = index.map_or(positions.len(), |i| (i as usize).min(positions.len()));
            let position = match (at.checked_sub(1).map(|i| positions[i]), positions.get(at)) {
                (None, None) => 0.0,
                (Some(before), None) => before + 1.0,
                (None, Some(after)) => after - 1.0,
                (Some(before), Some(after)) => (before + after) / 2.0,
            };
            add_item(tx, self.owner, id, position, text)?;
            Ok(true)
        })
    }

    /// Edits one checklist entry's given fields; returns whether it exists.
    pub fn checklist_update(
        &self,
        id: TaskId,
        item: ChecklistItemId,
        text: Option<&str>,
        done: Option<bool>,
    ) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            let n = tx.execute(
                "UPDATE task_checklist SET text = COALESCE(?4, text), done = COALESCE(?5, done)
                 WHERE owner_id = ?1 AND task_id = ?2 AND item_id = ?3",
                rusqlite::params![
                    self.owner.as_str(),
                    id.to_string(),
                    item.to_string(),
                    text,
                    done
                ],
            )?;
            if n > 0 {
                touch(tx, self.owner, id)?;
            }
            Ok(n > 0)
        })
    }

    /// Removes one checklist entry; returns whether it existed.
    pub fn checklist_remove(&self, id: TaskId, item: ChecklistItemId) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            let n = tx.execute(
                "DELETE FROM task_checklist WHERE owner_id = ?1 AND task_id = ?2 AND item_id = ?3",
                (self.owner.as_str(), id.to_string(), item.to_string()),
            )?;
            if n > 0 {
                touch(tx, self.owner, id)?;
            }
            Ok(n > 0)
        })
    }
}
