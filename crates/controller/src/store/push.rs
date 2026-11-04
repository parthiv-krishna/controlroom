//! Web Push subscriptions (one per device) and the per-device delivery dedup log.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::{DeviceId, SessionId};
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreResult};

/// A device's push subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSubscriptionRow {
    /// The device.
    pub device_id: DeviceId,
    /// The push service endpoint (https).
    pub endpoint: String,
    /// Base64url uncompressed P-256 point from the browser subscription.
    pub p256dh: String,
    /// Base64url 16-byte auth secret from the browser subscription.
    pub auth: String,
}

/// One push to claim in the dedup log.
#[derive(Debug, Clone, Copy)]
pub struct Delivery<'a> {
    /// The device.
    pub device_id: &'a DeviceId,
    /// The session.
    pub session_id: SessionId,
    /// The attention event within the session.
    pub event_key: &'a str,
}

/// Owner-scoped push operations.
pub struct PushSubscriptions<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "device_id, endpoint, p256dh, auth";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<PushSubscriptionRow> {
    Ok(PushSubscriptionRow {
        device_id: DeviceId::new(row.get::<_, String>(0)?),
        endpoint: row.get(1)?,
        p256dh: row.get(2)?,
        auth: row.get(3)?,
    })
}

impl<'a> PushSubscriptions<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Attaches (or replaces) a registered device's subscription; returns whether the device
    /// exists.
    pub fn set(&self, sub: &PushSubscriptionRow) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "INSERT INTO push_subscriptions (owner_id, device_id, endpoint, p256dh, auth, created_at)
             SELECT owner_id, device_id, ?3, ?4, ?5, ?6 FROM devices
             WHERE owner_id = ?1 AND device_id = ?2
             ON CONFLICT (owner_id, device_id) DO UPDATE SET
                 endpoint = excluded.endpoint, p256dh = excluded.p256dh, auth = excluded.auth",
            rusqlite::params![
                self.owner.as_str(),
                sub.device_id.as_str(),
                sub.endpoint,
                sub.p256dh,
                sub.auth,
                UnixMillis::now().0
            ],
        )?;
        Ok(n > 0)
    }

    /// A device's subscription.
    pub fn get(&self, device: &DeviceId) -> StoreResult<Option<PushSubscriptionRow>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM push_subscriptions WHERE owner_id = ?1 AND device_id = ?2"
                ),
                (self.owner.as_str(), device.as_str()),
                map,
            )
            .optional()?)
    }

    /// Every subscription.
    pub fn list(&self) -> StoreResult<Vec<PushSubscriptionRow>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM push_subscriptions WHERE owner_id = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drops a device's subscription (unsubscribe, or the push service said it is gone).
    pub fn delete(&self, device: &DeviceId) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "DELETE FROM push_subscriptions WHERE owner_id = ?1 AND device_id = ?2",
            (self.owner.as_str(), device.as_str()),
        )?;
        Ok(n > 0)
    }

    /// Records a successful send.
    pub fn mark_success(&self, device: &DeviceId, at: UnixMillis) -> StoreResult<()> {
        self.store.lock().execute(
            "UPDATE push_subscriptions SET last_success_at = ?3
             WHERE owner_id = ?1 AND device_id = ?2",
            (self.owner.as_str(), device.as_str(), at.0),
        )?;
        Ok(())
    }

    /// Claims the right to push one event to one device. `false` if it was already claimed
    /// (dedup) or the device is unknown.
    pub fn claim(&self, d: Delivery<'_>) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "INSERT OR IGNORE INTO push_deliveries
                 (owner_id, device_id, session_id, event_key, sent_at)
             SELECT owner_id, device_id, ?3, ?4, ?5 FROM devices
             WHERE owner_id = ?1 AND device_id = ?2",
            rusqlite::params![
                self.owner.as_str(),
                d.device_id.as_str(),
                d.session_id.to_string(),
                d.event_key,
                UnixMillis::now().0
            ],
        )?;
        Ok(n > 0)
    }

    /// Releases a claim after a failed (retryable) send.
    pub fn release(&self, d: Delivery<'_>) -> StoreResult<()> {
        self.store.lock().execute(
            "DELETE FROM push_deliveries WHERE owner_id = ?1 AND device_id = ?2
             AND session_id = ?3 AND event_key = ?4",
            rusqlite::params![
                self.owner.as_str(),
                d.device_id.as_str(),
                d.session_id.to_string(),
                d.event_key
            ],
        )?;
        Ok(())
    }

    /// Forgets the deliveries of a deleted session.
    pub fn forget_session(&self, session: SessionId) -> StoreResult<()> {
        self.store.lock().execute(
            "DELETE FROM push_deliveries WHERE owner_id = ?1 AND session_id = ?2",
            (self.owner.as_str(), session.to_string()),
        )?;
        Ok(())
    }

    /// Forgets deliveries older than `before`; returns how many.
    pub fn prune_deliveries(&self, before: UnixMillis) -> StoreResult<usize> {
        Ok(self.store.lock().execute(
            "DELETE FROM push_deliveries WHERE owner_id = ?1 AND sent_at < ?2",
            (self.owner.as_str(), before.0),
        )?)
    }
}
