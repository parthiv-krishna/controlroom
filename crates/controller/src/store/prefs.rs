//! Per-owner preferences as JSON values by key (notification preferences, UI settings).

use rusqlite::OptionalExtension;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{OwnerId, Store, StoreResult, to_json};

/// Owner-scoped preferences.
pub struct Prefs<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

impl<'a> Prefs<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// The value of `key`, if set.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> StoreResult<Option<T>> {
        let text: Option<String> = self
            .store
            .lock()
            .query_row(
                "SELECT value FROM prefs WHERE owner_id = ?1 AND key = ?2",
                (self.owner.as_str(), key),
                |row| row.get(0),
            )
            .optional()?;
        Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
    }

    /// Sets `key` (only that row).
    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> StoreResult<()> {
        self.store.lock().execute(
            "INSERT INTO prefs (owner_id, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (owner_id, key) DO UPDATE SET value = excluded.value",
            (self.owner.as_str(), key, to_json(value)?),
        )?;
        Ok(())
    }
}
