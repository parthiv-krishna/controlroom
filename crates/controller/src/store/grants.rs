//! Contact grants: user-approved permissions for two sessions to contact each other outside the
//! default policy. Created by the mailbox (R3); listed in the state feed and revocable.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::{DeviceId, GrantId, SessionId};
use controlroom_protocol::session::DeviceRef;
use controlroom_protocol::ui::ContactScope;
use controlroom_protocol::ui::next::ContactGrant;

use super::{OwnerId, Store, StoreResult, enum_col, enum_str, parsed_col};

/// Owner-scoped grant operations.
pub struct Grants<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactGrant> {
    let device: Option<String> = row.get(5)?;
    let label: Option<String> = row.get(6)?;
    Ok(ContactGrant {
        id: parsed_col(row, 0)?,
        from: parsed_col(row, 1)?,
        to: parsed_col(row, 2)?,
        scope: enum_col(row, 3)?,
        granted_at: UnixMillis(row.get(4)?),
        by: device.map(|device_id| DeviceRef {
            device_id: DeviceId::new(device_id),
            label: label.unwrap_or_default(),
        }),
    })
}

impl<'a> Grants<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Records a grant.
    pub fn insert(
        &self,
        from: SessionId,
        to: SessionId,
        scope: ContactScope,
        by: Option<&DeviceRef>,
    ) -> StoreResult<ContactGrant> {
        let grant = ContactGrant {
            id: GrantId::generate(),
            from,
            to,
            scope,
            granted_at: UnixMillis::now(),
            by: by.cloned(),
        };
        self.store.lock().execute(
            "INSERT INTO grants (id, owner_id, from_session, to_session, scope, granted_at,
                                 by_device, by_label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                grant.id.to_string(),
                self.owner.as_str(),
                from.to_string(),
                to.to_string(),
                enum_str(&scope)?,
                grant.granted_at.0,
                by.map(|d| d.device_id.as_str().to_owned()),
                by.map(|d| d.label.clone()),
            ],
        )?;
        Ok(grant)
    }

    /// Every grant, oldest first.
    pub fn list(&self) -> StoreResult<Vec<ContactGrant>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT id, from_session, to_session, scope, granted_at, by_device, by_label
             FROM grants WHERE owner_id = ?1 ORDER BY granted_at, rowid",
        )?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Revokes a grant; returns whether it existed.
    pub fn delete(&self, id: GrantId) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "DELETE FROM grants WHERE owner_id = ?1 AND id = ?2",
            (self.owner.as_str(), id.to_string()),
        )?;
        Ok(n > 0)
    }
}
