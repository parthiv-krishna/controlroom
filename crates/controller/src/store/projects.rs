//! Projects: named, colored groups of tasks. A session belongs to the projects of the tasks
//! that link it.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::ProjectId;
use controlroom_protocol::ui::Project;
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreResult, parsed_col};

/// Owner-scoped project operations.
pub struct Projects<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "id, name, color";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: parsed_col(row, 0)?,
        name: row.get(1)?,
        color: row.get(2)?,
    })
}

impl<'a> Projects<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Creates a project.
    pub fn insert(&self, name: &str, color: &str) -> StoreResult<Project> {
        let id = ProjectId::generate();
        self.store.lock().execute(
            "INSERT INTO projects (id, owner_id, name, color, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                id.to_string(),
                self.owner.as_str(),
                name,
                color,
                UnixMillis::now().0
            ],
        )?;
        Ok(Project {
            id,
            name: name.to_owned(),
            color: color.to_owned(),
        })
    }

    /// A project by id.
    pub fn get(&self, id: ProjectId) -> StoreResult<Option<Project>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!("SELECT {COLUMNS} FROM projects WHERE owner_id = ?1 AND id = ?2"),
                (self.owner.as_str(), id.to_string()),
                map,
            )
            .optional()?)
    }

    /// Every project, oldest first.
    pub fn list(&self) -> StoreResult<Vec<Project>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM projects WHERE owner_id = ?1 ORDER BY created_at, rowid"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Updates the given fields only; returns whether the project exists.
    pub fn update(
        &self,
        id: ProjectId,
        name: Option<&str>,
        color: Option<&str>,
    ) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "UPDATE projects SET name = COALESCE(?3, name), color = COALESCE(?4, color)
             WHERE owner_id = ?1 AND id = ?2",
            rusqlite::params![self.owner.as_str(), id.to_string(), name, color],
        )?;
        Ok(n > 0)
    }

    /// Deletes a project (and its task assignments and sandbox defaults).
    pub fn delete(&self, id: ProjectId) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            tx.execute(
                "DELETE FROM sandbox_profiles WHERE owner_id = ?1 AND scope_kind = 'project'
                 AND scope_id = ?2",
                (self.owner.as_str(), id.to_string()),
            )?;
            let n = tx.execute(
                "DELETE FROM projects WHERE owner_id = ?1 AND id = ?2",
                (self.owner.as_str(), id.to_string()),
            )?;
            Ok(n > 0)
        })
    }
}
