//! Login sessions of the web shell. The cookie carries a random token; the table stores only
//! its hash. Each session is tagged with the auth mode and subject (so a dev-token session
//! never survives a switch to OIDC, and the allow-list can be re-checked per request) and with
//! the device it belongs to (so removing a device signs it out).

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::DeviceId;
use controlroom_protocol::ui::AuthMode;
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreResult, enum_col, enum_str, json_col, to_json};

/// A stored login session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSessionRow {
    /// SHA-256 of the cookie token (hex).
    pub token_hash: String,
    /// The owner it signs in as.
    pub owner: OwnerId,
    /// How the user logged in.
    pub mode: AuthMode,
    /// The OIDC subject, or the dev-token marker.
    pub subject: String,
    /// The OIDC groups at login.
    pub groups: Vec<String>,
    /// Display name, if the identity provider supplied one.
    pub name: Option<String>,
    /// Email, if the identity provider supplied one.
    pub email: Option<String>,
    /// The browser it belongs to, once known.
    pub device_id: Option<DeviceId>,
    /// The CSRF token for non-WebSocket POSTs.
    pub csrf_token: String,
    /// When it was created.
    pub created_at: UnixMillis,
    /// When it was last used.
    pub last_seen_at: UnixMillis,
    /// When it expires unless used again.
    pub expires_at: UnixMillis,
}

/// A login session to create.
#[derive(Debug, Clone)]
pub struct NewAuthSession {
    /// SHA-256 of the cookie token (hex).
    pub token_hash: String,
    /// How the user logged in.
    pub mode: AuthMode,
    /// The OIDC subject, or the dev-token marker.
    pub subject: String,
    /// The OIDC groups.
    pub groups: Vec<String>,
    /// Display name.
    pub name: Option<String>,
    /// Email.
    pub email: Option<String>,
    /// The browser, if known at login.
    pub device_id: Option<DeviceId>,
    /// The CSRF token.
    pub csrf_token: String,
    /// When it expires unless used.
    pub expires_at: UnixMillis,
}

const COLUMNS: &str = "token_hash, owner_id, mode, subject, groups, name, email, device_id, \
                       csrf_token, created_at, last_seen_at, expires_at";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuthSessionRow> {
    Ok(AuthSessionRow {
        token_hash: row.get(0)?,
        owner: OwnerId::new(row.get::<_, String>(1)?),
        mode: enum_col(row, 2)?,
        subject: row.get(3)?,
        groups: json_col(row, 4)?,
        name: row.get(5)?,
        email: row.get(6)?,
        device_id: row.get::<_, Option<String>>(7)?.map(DeviceId::new),
        csrf_token: row.get(8)?,
        created_at: UnixMillis(row.get(9)?),
        last_seen_at: UnixMillis(row.get(10)?),
        expires_at: UnixMillis(row.get(11)?),
    })
}

/// Owner-scoped login-session operations.
pub struct AuthSessions<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

impl<'a> AuthSessions<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Creates a login session.
    pub fn create(&self, session: NewAuthSession) -> StoreResult<AuthSessionRow> {
        let now = UnixMillis::now().0;
        self.store.lock().execute(
            "INSERT INTO auth_sessions (token_hash, owner_id, mode, subject, groups, name, email,
                                        device_id, csrf_token, created_at, last_seen_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
            rusqlite::params![
                session.token_hash,
                self.owner.as_str(),
                enum_str(&session.mode)?,
                session.subject,
                to_json(&session.groups)?,
                session.name,
                session.email,
                session.device_id.as_ref().map(DeviceId::as_str),
                session.csrf_token,
                now,
                session.expires_at.0
            ],
        )?;
        Ok(AuthSessionRow {
            token_hash: session.token_hash,
            owner: self.owner.clone(),
            mode: session.mode,
            subject: session.subject,
            groups: session.groups,
            name: session.name,
            email: session.email,
            device_id: session.device_id,
            csrf_token: session.csrf_token,
            created_at: UnixMillis(now),
            last_seen_at: UnixMillis(now),
            expires_at: session.expires_at,
        })
    }

    /// Records the device of a session that has none yet.
    pub fn set_device(&self, token_hash: &str, device: &DeviceId) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "UPDATE auth_sessions SET device_id = ?3
             WHERE owner_id = ?1 AND token_hash = ?2 AND device_id IS NULL",
            (self.owner.as_str(), token_hash, device.as_str()),
        )?;
        Ok(n > 0)
    }

    /// Logout: revokes one session.
    pub fn revoke(&self, token_hash: &str) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "DELETE FROM auth_sessions WHERE owner_id = ?1 AND token_hash = ?2",
            (self.owner.as_str(), token_hash),
        )?;
        Ok(n > 0)
    }

    /// Revokes every session of a device; returns how many.
    pub fn revoke_device(&self, device: &DeviceId) -> StoreResult<usize> {
        Ok(self.store.lock().execute(
            "DELETE FROM auth_sessions WHERE owner_id = ?1 AND device_id = ?2",
            (self.owner.as_str(), device.as_str()),
        )?)
    }

    /// Revokes every session of the owner; returns how many.
    pub fn revoke_all(&self) -> StoreResult<usize> {
        Ok(self.store.lock().execute(
            "DELETE FROM auth_sessions WHERE owner_id = ?1",
            [self.owner.as_str()],
        )?)
    }
}

/// The unscoped half: cookie → session (→ owner), and expiry sweeping.
pub struct AuthSessionLookup<'a> {
    store: &'a Store,
}

impl<'a> AuthSessionLookup<'a> {
    pub(super) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// The live session for `token_hash`, without touching it.
    pub fn get(&self, token_hash: &str, now: UnixMillis) -> StoreResult<Option<AuthSessionRow>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM auth_sessions WHERE token_hash = ?1 AND expires_at > ?2"
                ),
                (token_hash, now.0),
                map,
            )
            .optional()?)
    }

    /// The live session for `token_hash`, with its expiry slid to `expires_at`.
    pub fn touch(
        &self,
        token_hash: &str,
        now: UnixMillis,
        expires_at: UnixMillis,
    ) -> StoreResult<Option<AuthSessionRow>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!(
                    "UPDATE auth_sessions SET last_seen_at = ?2, expires_at = ?3
                     WHERE token_hash = ?1 AND expires_at > ?2 RETURNING {COLUMNS}"
                ),
                (token_hash, now.0, expires_at.0),
                map,
            )
            .optional()?)
    }

    /// Deletes expired sessions; returns how many.
    pub fn purge_expired(&self, now: UnixMillis) -> StoreResult<usize> {
        Ok(self
            .store
            .lock()
            .execute("DELETE FROM auth_sessions WHERE expires_at <= ?1", [now.0])?)
    }
}
