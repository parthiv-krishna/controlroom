//! `host.sqlite`: the host daemon's durable state, with versioned migrations (`user_version`).
//!
//! The modules that own a table write their own SQL through [`Store::run`], which runs on a
//! blocking thread (every commit is fsynced: `synchronous = FULL`). Tables:
//!
//! - `meta`: the daemon id and the job event log's acked revision;
//! - `workspaces`: open workspaces and the sandbox they were opened with;
//! - `shells`: named shells (the agent's and the user's), their current supervisor and last cwd;
//! - `journal`: tool calls by call id (see [`super::tools`]);
//! - `jobs`, `job_events`: jobs and the job event log (see [`super::jobs`]).

use std::sync::{Arc, Mutex, PoisonError};

use controlroom_protocol::{ErrorData, RpcError};
use rusqlite::{Connection, OptionalExtension};

/// Migrations, applied in order; the index plus one is the schema version.
const MIGRATIONS: &[&str] = &[
    r"
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE workspaces (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        path TEXT NOT NULL,
        root TEXT NOT NULL,
        sandbox TEXT NOT NULL
    );
    CREATE TABLE shells (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        name TEXT NOT NULL,
        workspace_id TEXT NOT NULL,
        supervisor TEXT,
        shell_pid INTEGER,
        cwd TEXT NOT NULL,
        reset_reason TEXT
    );
    CREATE TABLE journal (
        call_id TEXT PRIMARY KEY,
        workspace_id TEXT NOT NULL,
        op TEXT NOT NULL,
        args_hash TEXT NOT NULL,
        state TEXT NOT NULL,
        outcome TEXT,
        shell_id TEXT,
        start_seq INTEGER,
        intent TEXT,
        started_at INTEGER NOT NULL,
        ended_at INTEGER
    );
    CREATE TABLE jobs (
        id TEXT PRIMARY KEY,
        call_id TEXT,
        workspace_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        record TEXT NOT NULL,
        scanned_seq INTEGER NOT NULL DEFAULT 0,
        offset INTEGER NOT NULL DEFAULT 0,
        killed INTEGER NOT NULL DEFAULT 0,
        exit_rev INTEGER
    );
    CREATE TABLE job_events (
        rev INTEGER PRIMARY KEY,
        at INTEGER NOT NULL,
        job_id TEXT NOT NULL,
        event TEXT NOT NULL
    );
",
    // 2: the user's own terminals.
    r"
    ALTER TABLE shells ADD COLUMN owner TEXT NOT NULL DEFAULT 'agent';
    ALTER TABLE shells ADD COLUMN sandbox TEXT NOT NULL DEFAULT 'workspace';
",
];

/// The database, shared by the host's modules.
#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

impl Store {
    /// Opens (creating and migrating) the database at `path`.
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;",
        )?;
        migrate(&conn)?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// Runs `f` on a blocking thread with the connection.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, RpcError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.run_blocking(f))
            .await
            .map_err(internal)?
    }

    /// Runs `f` with the connection on the calling thread (startup, blocking contexts).
    pub fn run_blocking<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, RpcError> {
        let mut conn = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut conn).map_err(internal)
    }
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let version = usize::try_from(version).unwrap_or(0);
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        conn.execute_batch(&format!(
            "BEGIN; {sql}; PRAGMA user_version = {}; COMMIT;",
            i + 1
        ))?;
    }
    Ok(())
}

/// A `meta` value.
pub fn meta_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

/// Sets a `meta` value.
pub fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// An `internal_error`.
pub fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorData::InternalError, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_keeps_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sqlite");
        let store = Store::open(&path).unwrap();
        store
            .run_blocking(|c| meta_set(c, "daemon_id", "x"))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let (version, id) = store
            .run_blocking(|c| {
                let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                Ok((v, meta_get(c, "daemon_id")?))
            })
            .unwrap();
        assert_eq!(usize::try_from(version).unwrap(), MIGRATIONS.len());
        assert_eq!(id.as_deref(), Some("x"));
    }
}
