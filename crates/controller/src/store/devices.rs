//! Devices: the browsers a user signed in from, with their labels.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::DeviceId;
use controlroom_protocol::ui::Device;
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreResult};

/// Owner-scoped device operations.
pub struct Devices<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const SELECT: &str = "SELECT d.device_id, d.label, d.created_at, d.last_seen,
        EXISTS (SELECT 1 FROM push_subscriptions p
                WHERE p.owner_id = d.owner_id AND p.device_id = d.device_id)
    FROM devices d";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        device_id: DeviceId::new(row.get::<_, String>(0)?),
        label: row.get(1)?,
        created_at: UnixMillis(row.get(2)?),
        last_seen: UnixMillis(row.get(3)?),
        push: row.get(4)?,
    })
}

impl<'a> Devices<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Registers a device, or marks a known one as seen. A known device keeps its label.
    pub fn register(&self, id: &DeviceId, label: &str) -> StoreResult<Device> {
        let now = UnixMillis::now().0;
        self.store.lock().execute(
            "INSERT INTO devices (owner_id, device_id, label, created_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (owner_id, device_id) DO UPDATE SET last_seen = excluded.last_seen",
            (self.owner.as_str(), id.as_str(), label, now),
        )?;
        self.get(id)?
            .ok_or_else(|| super::StoreError::Conflict("the device vanished".into()))
    }

    /// A device by id.
    pub fn get(&self, id: &DeviceId) -> StoreResult<Option<Device>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!("{SELECT} WHERE d.owner_id = ?1 AND d.device_id = ?2"),
                (self.owner.as_str(), id.as_str()),
                map,
            )
            .optional()?)
    }

    /// Every device, oldest first.
    pub fn list(&self) -> StoreResult<Vec<Device>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "{SELECT} WHERE d.owner_id = ?1 ORDER BY d.created_at, d.device_id"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Renames a device; returns whether it exists.
    pub fn rename(&self, id: &DeviceId, label: &str) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "UPDATE devices SET label = ?3 WHERE owner_id = ?1 AND device_id = ?2",
            (self.owner.as_str(), id.as_str(), label),
        )?;
        Ok(n > 0)
    }

    /// Removes a device with its login sessions, push subscription and delivery log, in one
    /// transaction; returns whether it existed.
    pub fn remove(&self, id: &DeviceId) -> StoreResult<bool> {
        self.store.transaction(|tx| {
            tx.execute(
                "DELETE FROM auth_sessions WHERE owner_id = ?1 AND device_id = ?2",
                (self.owner.as_str(), id.as_str()),
            )?;
            let n = tx.execute(
                "DELETE FROM devices WHERE owner_id = ?1 AND device_id = ?2",
                (self.owner.as_str(), id.as_str()),
            )?;
            Ok(n > 0)
        })
    }
}
