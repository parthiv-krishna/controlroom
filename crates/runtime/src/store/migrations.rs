//! Versioned schema migrations, tracked in SQLite's `user_version`.
//!
//! Daemons upgrade themselves in the field, so the schema only ever moves forward: each entry
//! of [`MIGRATIONS`] upgrades version `i` to `i + 1` and runs in its own transaction. Never edit
//! a released migration; append a new one. Later features (todos, jobs, children, outbox) add
//! their tables as new migrations.

use rusqlite::Connection;

/// Schema version `i + 1` is `MIGRATIONS[..=i]`.
const MIGRATIONS: &[&str] = &[
    // 1: sessions, queue, op dedup, decisions, the materialized timeline, the rev log.
    r#"
    CREATE TABLE kv (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE sessions (
        id TEXT PRIMARY KEY,
        agent TEXT NOT NULL,
        driver TEXT NOT NULL,
        cwd TEXT NOT NULL,
        name TEXT,
        created_at INTEGER NOT NULL,
        closed_at INTEGER,
        parent_id TEXT,
        spawned_by TEXT,
        sandbox_level TEXT,
        sandbox TEXT NOT NULL,
        read_seq INTEGER NOT NULL,
        last_turn_end_seq INTEGER NOT NULL,
        last_outcome TEXT,
        last_error TEXT,
        queue_paused INTEGER NOT NULL,
        auto_send INTEGER NOT NULL,
        pending_start TEXT,
        history_note TEXT,
        resume_failed INTEGER NOT NULL,
        applied_seq INTEGER NOT NULL,
        timeline_seq INTEGER NOT NULL
    );
    CREATE TABLE agent_sessions (
        session_id TEXT NOT NULL,
        idx INTEGER NOT NULL,
        driver TEXT NOT NULL,
        native_id TEXT NOT NULL,
        native_path TEXT,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        started_by TEXT NOT NULL,
        PRIMARY KEY (session_id, idx)
    );
    CREATE TABLE queue (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        position INTEGER NOT NULL,
        entry TEXT NOT NULL
    );
    CREATE INDEX queue_session ON queue (session_id, position);
    CREATE TABLE ops (
        op_id TEXT PRIMARY KEY,
        method TEXT NOT NULL,
        params_hash TEXT NOT NULL,
        result TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE INDEX ops_at ON ops (at);
    CREATE TABLE decisions (
        session_id TEXT NOT NULL,
        request_id TEXT NOT NULL,
        decision TEXT NOT NULL,
        PRIMARY KEY (session_id, request_id)
    );
    CREATE TABLE timeline_items (
        session_id TEXT NOT NULL,
        item_id TEXT NOT NULL,
        seq_from INTEGER NOT NULL,
        changed_seq INTEGER NOT NULL,
        item TEXT NOT NULL,
        PRIMARY KEY (session_id, item_id)
    );
    CREATE INDEX timeline_changed ON timeline_items (session_id, changed_seq);
    CREATE TABLE revlog (
        rev INTEGER PRIMARY KEY,
        change TEXT NOT NULL
    );
    CREATE TABLE tombstones (
        session_id TEXT PRIMARY KEY,
        purged_at INTEGER NOT NULL
    );
    "#,
    // 2: requests the daemon raised itself (sandbox approvals).
    r#"
    CREATE TABLE raised_requests (
        session_id TEXT NOT NULL,
        request_id TEXT NOT NULL,
        raised TEXT NOT NULL,
        PRIMARY KEY (session_id, request_id)
    );
    "#,
];

/// The schema version this build writes.
pub(crate) const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Errors from migrating.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MigrateError {
    /// The database was written by a newer daemon (daemons never downgrade).
    #[error("database schema version {0} is newer than this daemon's ({SCHEMA_VERSION})")]
    TooNew(u32),
    /// SQLite failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Brings the schema to [`SCHEMA_VERSION`].
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), MigrateError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(MigrateError::TooNew(current));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(
            None,
            "user_version",
            u32::try_from(index + 1).unwrap_or(u32::MAX),
        )?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_refuses_newer_schemas() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(MigrateError::TooNew(_))));
    }
}
