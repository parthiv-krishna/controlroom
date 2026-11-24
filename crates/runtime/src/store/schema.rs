//! The `agent.sqlite` schema. Edited in place until the first release, like the controller's
//! client store: bump [`VERSION`] on every change. An existing database with another version
//! is refused (delete the agent daemon's state to start over); there are no upgrades.
//!
//! Version 2 (profiles: `sessions.profile_id` and `profile_policies`) replaced version 1, the
//! first one, which had a migration list.

use rusqlite::Connection;

/// The schema version, kept in `PRAGMA user_version`.
pub(crate) const VERSION: u32 = 2;

/// Sessions with their workspaces and shells, queue, op dedup, decisions, raised requests,
/// todos, injections, jobs, the materialized timeline, the rev log, the hosts' job cursors and
/// the profiles' policies (as the controller last reported them).
const SCHEMA: &str = r#"
CREATE TABLE kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    driver TEXT NOT NULL,
    name TEXT,
    created_at INTEGER NOT NULL,
    closed_at INTEGER,
    parent_id TEXT,
    -- NULL only for a session created while no profile was reported (a debug client's).
    profile_id TEXT,
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
    timeline_seq INTEGER NOT NULL,
    auto_continue TEXT NOT NULL,
    run_triggers TEXT NOT NULL,
    workspaces TEXT NOT NULL,
    shells TEXT NOT NULL,
    late_calls TEXT NOT NULL,
    outages TEXT NOT NULL
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
CREATE TABLE raised_requests (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    raised TEXT NOT NULL,
    PRIMARY KEY (session_id, request_id)
);
CREATE TABLE todos (
    session_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    todo TEXT NOT NULL,
    PRIMARY KEY (session_id, position)
);
CREATE TABLE injections (
    session_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    injection TEXT NOT NULL,
    PRIMARY KEY (session_id, position)
);
CREATE TABLE jobs (
    session_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    job TEXT NOT NULL,
    PRIMARY KEY (session_id, job_id)
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
CREATE TABLE host_cursors (
    machine_id TEXT PRIMARY KEY,
    rev INTEGER NOT NULL
);
-- The last `profiles.report`: what each profile's sessions may use (JSON columns).
CREATE TABLE profile_policies (
    profile_id TEXT PRIMARY KEY,
    models TEXT NOT NULL,
    model_defaults TEXT NOT NULL,
    private INTEGER NOT NULL,
    reported_at INTEGER NOT NULL
);
"#;

/// Why the database can't be used.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SchemaError {
    /// The database was written with another schema.
    #[error(
        "agent.sqlite has schema version {0}, this build uses {VERSION}; delete the agent \
         daemon's state to start over (agent.sqlite and sessions/; see docs/deployment.md)"
    )]
    Mismatch(u32),
    /// SQLite failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Creates the schema in an empty database, or checks an existing one's version.
pub(crate) fn apply(conn: &Connection) -> Result<(), SchemaError> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found == VERSION {
        return Ok(());
    }
    let empty: bool = conn.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table')",
        [],
        |row| row.get(0),
    )?;
    if found != 0 || !empty {
        return Err(SchemaError::Mismatch(found));
    }
    conn.execute_batch(&format!(
        "BEGIN; {SCHEMA} PRAGMA user_version = {VERSION}; COMMIT;"
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(conn: &Connection) -> u32 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_new_database_gets_the_schema_once() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn).unwrap();
        apply(&conn).unwrap();
        assert_eq!(version(&conn), VERSION);
    }

    /// Older (and newer) databases are refused, never upgraded: the profiles release starts
    /// fresh.
    #[test]
    fn another_version_is_refused_with_how_to_start_over() {
        for other in [1, VERSION + 1] {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch("CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
                .unwrap();
            conn.pragma_update(None, "user_version", other).unwrap();
            let error = apply(&conn).unwrap_err();
            assert!(matches!(error, SchemaError::Mismatch(v) if v == other));
            assert!(
                error
                    .to_string()
                    .contains("delete the agent daemon's state"),
                "{error}"
            );
        }
        // Tables without a version are not ours to fill in either.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        assert!(matches!(apply(&conn), Err(SchemaError::Mismatch(0))));
    }
}
