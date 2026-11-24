//! Profiles: tenants of one owner. A profile owns machine entries (`machine_profiles`),
//! projects and tasks (their `profile_id`), sandbox defaults (scope `profile`) and the model
//! policy the agent daemon enforces. Every write touches only the columns it changes.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::agentd::{ModelDefaults, ModelPolicy};
use controlroom_protocol::ids::ProfileId;
use controlroom_protocol::ui::Profile;
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreResult, conflict, json_col, json_col_opt, parsed_col, to_json};

/// A profile to create.
#[derive(Debug, Clone)]
pub struct NewProfile {
    /// Display name (unique per owner).
    pub name: String,
    /// A CSS color.
    pub color: String,
    /// Private.
    pub private: bool,
    /// The model accounts its agents may use.
    pub models: ModelPolicy,
}

/// The fields of a profile to change; `None` leaves one unchanged.
#[derive(Debug, Clone, Default)]
pub struct ProfileChanges {
    /// New name.
    pub name: Option<String>,
    /// New color.
    pub color: Option<String>,
    /// Private or not.
    pub private: Option<bool>,
    /// New model accounts.
    pub models: Option<ModelPolicy>,
    /// New model defaults (all `None` clears them).
    pub model_defaults: Option<ModelDefaults>,
    /// New sort key.
    pub position: Option<f64>,
}

/// What a profile still holds (it can be deleted only when empty).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileContents {
    /// Machine entries in it.
    pub machines: u32,
    /// Its projects.
    pub projects: u32,
    /// Its tasks.
    pub tasks: u32,
}

impl ProfileContents {
    /// Whether it holds nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Owner-scoped profile operations.
pub struct Profiles<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "id, name, color, private, models, model_defaults, position, created_at";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: parsed_col(row, 0)?,
        name: row.get(1)?,
        color: row.get(2)?,
        private: row.get(3)?,
        models: json_col(row, 4)?,
        model_defaults: json_col_opt(row, 5)?.unwrap_or_default(),
        position: row.get(6)?,
        created_at: UnixMillis(row.get(7)?),
    })
}

/// Model defaults as stored: `NULL` when nothing is set.
fn defaults_json(defaults: &ModelDefaults) -> StoreResult<Option<String>> {
    if *defaults == ModelDefaults::default() {
        Ok(None)
    } else {
        to_json(defaults).map(Some)
    }
}

impl<'a> Profiles<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Creates a profile after the last one. A taken name is a conflict.
    pub fn insert(&self, profile: NewProfile) -> StoreResult<Profile> {
        let id = ProfileId::generate();
        self.store
            .transaction(|tx| {
                let position: f64 = tx.query_row(
                    "SELECT COALESCE(MAX(position) + 1, 0) FROM profiles WHERE owner_id = ?1",
                    [self.owner.as_str()],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO profiles (id, owner_id, name, color, private, models, position,
                                           created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        id.to_string(),
                        self.owner.as_str(),
                        profile.name,
                        profile.color,
                        profile.private,
                        to_json(&profile.models)?,
                        position,
                        UnixMillis::now().0
                    ],
                )?;
                Ok(())
            })
            .map_err(|e| name_conflict(e, &profile.name))?;
        self.get(id)?
            .ok_or_else(|| super::StoreError::Conflict("the profile vanished".into()))
    }

    /// A profile by id.
    pub fn get(&self, id: ProfileId) -> StoreResult<Option<Profile>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!("SELECT {COLUMNS} FROM profiles WHERE owner_id = ?1 AND id = ?2"),
                (self.owner.as_str(), id.to_string()),
                map,
            )
            .optional()?)
    }

    /// Every profile, in `position` order.
    pub fn list(&self) -> StoreResult<Vec<Profile>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM profiles WHERE owner_id = ?1
             ORDER BY position, created_at, rowid"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Updates the given fields only; returns whether the profile exists. A taken name is a
    /// conflict.
    pub fn update(&self, id: ProfileId, changes: &ProfileChanges) -> StoreResult<bool> {
        let models = changes.models.as_ref().map(to_json).transpose()?;
        let defaults = changes
            .model_defaults
            .as_ref()
            .map(defaults_json)
            .transpose()?;
        let n = self
            .store
            .lock()
            .execute(
                "UPDATE profiles SET name = COALESCE(?3, name), color = COALESCE(?4, color),
                     private = COALESCE(?5, private), models = COALESCE(?6, models),
                     model_defaults = CASE WHEN ?7 THEN ?8 ELSE model_defaults END,
                     position = COALESCE(?9, position)
                 WHERE owner_id = ?1 AND id = ?2",
                rusqlite::params![
                    self.owner.as_str(),
                    id.to_string(),
                    changes.name,
                    changes.color,
                    changes.private,
                    models,
                    defaults.is_some(),
                    defaults.flatten(),
                    changes.position
                ],
            )
            .map_err(|e| {
                conflict(e, || {
                    format!(
                        "a profile named {:?} already exists",
                        changes.name.as_deref().unwrap_or_default()
                    )
                })
            })?;
        Ok(n > 0)
    }

    /// What the profile still holds.
    pub fn contents(&self, id: ProfileId) -> StoreResult<ProfileContents> {
        let conn = self.store.lock();
        let count = |table: &str| -> StoreResult<u32> {
            Ok(conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE owner_id = ?1 AND profile_id = ?2"),
                (self.owner.as_str(), id.to_string()),
                |r| r.get(0),
            )?)
        };
        Ok(ProfileContents {
            machines: count("machine_profiles")?,
            projects: count("projects")?,
            tasks: count("tasks")?,
        })
    }

    /// Deletes a profile with its sandbox defaults; returns whether it existed. One that still
    /// holds machines, projects or tasks is refused by the foreign keys.
    pub fn delete(&self, id: ProfileId) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            tx.execute(
                "DELETE FROM sandbox_defaults WHERE owner_id = ?1 AND scope_kind = 'profile'
                 AND scope_id = ?2",
                (self.owner.as_str(), id.to_string()),
            )?;
            let n = tx.execute(
                "DELETE FROM profiles WHERE owner_id = ?1 AND id = ?2",
                (self.owner.as_str(), id.to_string()),
            )?;
            Ok(n > 0)
        })
    }
}

fn name_conflict(e: super::StoreError, name: &str) -> super::StoreError {
    match e {
        super::StoreError::Sqlite(e) => {
            conflict(e, || format!("a profile named {name:?} already exists"))
        }
        other => other,
    }
}
