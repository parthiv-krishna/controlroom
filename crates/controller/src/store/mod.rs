//! The client store: SQLite, with every row scoped to an owner.
//!
//! All row access goes through [`OwnerRepo`] ([`Store::owner`]); every query it issues filters
//! on its owner id, so one owner can never read or change another owner's rows. The unscoped
//! operations are owner management, the installation's controller id, and resolving an auth
//! cookie to its session ([`Store::auth_sessions`]).
//!
//! **Writes touch only the columns they change.** Nothing reads a row, awaits, and writes the
//! whole row back: that was the prototype's lost-update bug (spec §16). Edits that must read
//! first (a JSON list column) do so inside one transaction under the connection lock.
//!
//! **Schema.** The schema in [`schema`] is edited in place until the first release; there is no
//! migration code. A database with another schema version is refused with a message telling
//! the user to delete it.

mod auth_sessions;
mod devices;
mod grants;
mod keys;
mod machines;
mod prefs;
mod projects;
mod push;
mod sandbox;
mod schedules;
mod schema;
mod tasks;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::ControllerId;
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use auth_sessions::{AuthSessionLookup, AuthSessionRow, AuthSessions, NewAuthSession};
pub use devices::Devices;
pub use grants::Grants;
pub use keys::{KeyKind, KeyRow, Keys, NewKey};
pub use machines::{MachineRow, Machines, NewMachineRow, SeenUpdate};
pub use prefs::Prefs;
pub use projects::Projects;
pub use push::{Delivery, PushSubscriptionRow, PushSubscriptions};
pub use sandbox::SandboxProfiles;
pub use schedules::{NewSchedule, ScheduleRow, Schedules, Timing};
pub use tasks::{NewTask, TaskLink, TaskRow, Tasks};

/// Errors from the client store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A JSON column didn't encode or decode.
    #[error("json column: {0}")]
    Json(#[from] serde_json::Error),
    /// The database was written with another schema.
    #[error(
        "the client store has schema version {found}, this build uses {expected}; \
         delete the database file to start over (no migrations before the first release)"
    )]
    SchemaMismatch {
        /// The database's version.
        found: u32,
        /// This build's version.
        expected: u32,
    },
    /// A stored value doesn't parse.
    #[error("invalid stored value in {column}: {value}")]
    InvalidValue {
        /// Where.
        column: &'static str,
        /// What.
        value: String,
    },
    /// A write would break a uniqueness rule (e.g. two machines with one name).
    #[error("{0}")]
    Conflict(String),
}

/// A store result.
pub type StoreResult<T> = Result<T, StoreError>;

/// The owner whose rows a repository may touch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnerId(String);

impl OwnerId {
    /// The single owner of a v1 deployment.
    pub const LOCAL: &str = "local";

    /// Wraps an owner id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The single owner every v1 deployment uses.
    pub fn local() -> Self {
        Self(Self::LOCAL.to_owned())
    }

    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OwnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The SQLite client store. Cheap to clone.
///
/// Queries are short and run synchronously under a mutex; nothing holds it across an await.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (creating if needed) the database file.
    pub fn open(path: &Path) -> StoreResult<Self> {
        Self::init(Connection::open(path)?)
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> StoreResult<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        // In-memory databases report "memory" instead.
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        schema::apply(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Runs `f` in one transaction.
    pub(crate) fn transaction<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Repositories scoped to `owner`.
    pub fn owner(&self, owner: &OwnerId) -> OwnerRepo<'_> {
        OwnerRepo {
            store: self,
            owner: owner.clone(),
        }
    }

    /// Creates an owner row if it doesn't exist yet.
    pub fn ensure_owner(&self, owner: &OwnerId) -> StoreResult<()> {
        self.lock().execute(
            "INSERT OR IGNORE INTO owners (id, created_at) VALUES (?1, ?2)",
            (owner.as_str(), UnixMillis::now().0),
        )?;
        Ok(())
    }

    /// Every owner, in id order.
    pub fn owner_ids(&self) -> StoreResult<Vec<OwnerId>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM owners ORDER BY id")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(OwnerId).collect())
    }

    /// This installation's controller id, generated on first use. Daemons serve one controller
    /// at a time and recognize a reconnecting controller by it.
    pub fn controller_id(&self) -> StoreResult<ControllerId> {
        let conn = self.lock();
        let existing: Option<String> = conn
            .query_row("SELECT controller_id FROM installation", [], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            return parse("installation.controller_id", &id);
        }
        let id = ControllerId::generate();
        conn.execute(
            "INSERT INTO installation (controller_id) VALUES (?1)",
            [id.to_string()],
        )?;
        Ok(id)
    }

    /// Resolving an auth cookie to its session (and owner) is necessarily unscoped.
    pub fn auth_sessions(&self) -> AuthSessionLookup<'_> {
        AuthSessionLookup::new(self)
    }
}

/// Entry point to the owner-scoped repositories.
pub struct OwnerRepo<'s> {
    store: &'s Store,
    owner: OwnerId,
}

impl OwnerRepo<'_> {
    /// SSH keys.
    pub fn keys(&self) -> Keys<'_> {
        Keys::new(self.store, &self.owner)
    }

    /// Machine records.
    pub fn machines(&self) -> Machines<'_> {
        Machines::new(self.store, &self.owner)
    }

    /// Projects.
    pub fn projects(&self) -> Projects<'_> {
        Projects::new(self.store, &self.owner)
    }

    /// Tasks, their checklists, project assignments and session links.
    pub fn tasks(&self) -> Tasks<'_> {
        Tasks::new(self.store, &self.owner)
    }

    /// Contact grants.
    pub fn grants(&self) -> Grants<'_> {
        Grants::new(self.store, &self.owner)
    }

    /// Devices.
    pub fn devices(&self) -> Devices<'_> {
        Devices::new(self.store, &self.owner)
    }

    /// Login sessions.
    pub fn auth_sessions(&self) -> AuthSessions<'_> {
        AuthSessions::new(self.store, &self.owner)
    }

    /// Web Push subscriptions and the delivery dedup log.
    pub fn push(&self) -> PushSubscriptions<'_> {
        PushSubscriptions::new(self.store, &self.owner)
    }

    /// Schedules.
    pub fn schedules(&self) -> Schedules<'_> {
        Schedules::new(self.store, &self.owner)
    }

    /// Sandbox default profiles.
    pub fn sandbox_profiles(&self) -> SandboxProfiles<'_> {
        SandboxProfiles::new(self.store, &self.owner)
    }

    /// UI and notification preferences.
    pub fn prefs(&self) -> Prefs<'_> {
        Prefs::new(self.store, &self.owner)
    }
}

pub(crate) fn to_json<T: Serialize + ?Sized>(value: &T) -> StoreResult<String> {
    Ok(serde_json::to_string(value)?)
}

/// Decodes a JSON column inside a row mapper.
pub(crate) fn json_col<T: DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<T> {
    let text: String = row.get(idx)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Decodes a nullable JSON column inside a row mapper.
pub(crate) fn json_col_opt<T: DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<Option<T>> {
    let text: Option<String> = row.get(idx)?;
    text.map(|text| {
        serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

/// Decodes a parsed (e.g. ULID) column inside a row mapper.
pub(crate) fn parsed_col<T>(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: String = row.get(idx)?;
    text.parse().map_err(|e: T::Err| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Decodes a nullable parsed column inside a row mapper.
pub(crate) fn parsed_col_opt<T>(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: Option<String> = row.get(idx)?;
    text.map(|text| {
        text.parse().map_err(|e: T::Err| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

/// Parses a stored value outside a row mapper.
pub(crate) fn parse<T: FromStr>(column: &'static str, value: &str) -> StoreResult<T> {
    value.parse().map_err(|_| StoreError::InvalidValue {
        column,
        value: value.to_owned(),
    })
}

/// Encodes a `snake_case` serde enum as its bare string.
pub(crate) fn enum_str<T: Serialize>(value: &T) -> StoreResult<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(StoreError::InvalidValue {
            column: "enum",
            value: other.to_string(),
        }),
    }
}

/// Decodes a bare-string serde enum column inside a row mapper.
pub(crate) fn enum_col<T: DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<T> {
    let text: String = row.get(idx)?;
    serde_json::from_value(serde_json::Value::String(text)).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Maps a UNIQUE constraint failure to [`StoreError::Conflict`].
pub(crate) fn conflict(e: rusqlite::Error, message: impl FnOnce() -> String) -> StoreError {
    match &e {
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StoreError::Conflict(message())
        }
        _ => StoreError::Sqlite(e),
    }
}
