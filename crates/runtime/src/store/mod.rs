//! `agent.sqlite`: sessions, queue, op dedup, decisions, the materialized timeline, the rev log,
//! job cursors and settings. One connection, used under the runtime's lock; every entry point runs as one
//! transaction ([`Store::begin`], [`Store::commit`], [`Store::rollback`]).
//!
//! The database runs in WAL mode with `synchronous=FULL`: a committed decision or queue change
//! survives a power loss, so the transcript and the store never disagree about what was decided.

mod migrations;
mod rows;

use std::path::Path;

use controlroom_protocol::ids::{OpId, Rev, SessionId, UnixMillis};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) use migrations::MigrateError;

/// The store.
pub(crate) struct Store {
    conn: Connection,
    /// Fault injection for tests: the next commit fails (and rolls back).
    #[cfg(test)]
    pub fail_next_commit: bool,
}

/// What [`Store::op`] found for an op id.
pub(crate) enum StoredOp {
    /// Never used.
    New,
    /// The same command ran: its result.
    Done(String),
    /// Used by a different command.
    Conflict,
}

impl Store {
    /// Opens (creating if needed) and migrates the database.
    pub(crate) fn open(path: &Path) -> Result<Store, MigrateError> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        migrations::migrate(&mut conn)?;
        Ok(Store {
            conn,
            #[cfg(test)]
            fail_next_commit: false,
        })
    }

    pub(crate) fn begin(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("BEGIN")
    }

    pub(crate) fn commit(&mut self) -> rusqlite::Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_commit) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some("injected commit failure".into()),
            ));
        }
        self.conn.execute_batch("COMMIT")
    }

    pub(crate) fn rollback(&self) {
        if self.conn.is_autocommit() {
            return;
        }
        if let Err(e) = self.conn.execute_batch("ROLLBACK") {
            tracing::error!("rolling back: {e}");
        }
    }

    // ---------------------------------------------------------------------------------------
    // Key-value settings
    // ---------------------------------------------------------------------------------------

    pub(crate) fn kv_get<T: DeserializeOwned>(&self, key: &str) -> rusqlite::Result<Option<T>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
    }

    pub(crate) fn kv_set<T: Serialize>(&self, key: &str, value: &T) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, json(value)],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Op dedup
    // ---------------------------------------------------------------------------------------

    pub(crate) fn op(&self, op_id: &OpId, method: &str, hash: &str) -> rusqlite::Result<StoredOp> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT method, params_hash, result FROM ops WHERE op_id = ?1",
                [op_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            None => StoredOp::New,
            Some((m, h, result)) if m == method && h == hash => StoredOp::Done(result),
            Some(_) => StoredOp::Conflict,
        })
    }

    pub(crate) fn put_op(
        &self,
        op_id: &OpId,
        method: &str,
        hash: &str,
        result: &str,
        at: UnixMillis,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO ops (op_id, method, params_hash, result, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![op_id.as_str(), method, hash, result, at.0],
        )?;
        Ok(())
    }

    /// Forgets op ids older than `before`.
    pub(crate) fn prune_ops(&self, before: UnixMillis) -> rusqlite::Result<usize> {
        self.conn
            .execute("DELETE FROM ops WHERE at < ?1", [before.0])
    }

    // ---------------------------------------------------------------------------------------
    // The rev log
    // ---------------------------------------------------------------------------------------

    pub(crate) fn last_rev(&self) -> rusqlite::Result<Rev> {
        let rev: Option<i64> = self
            .conn
            .query_row("SELECT MAX(rev) FROM revlog", [], |r| r.get(0))?;
        Ok(Rev(rev.map_or(0, uint)))
    }

    pub(crate) fn append_rev<T: Serialize>(&self, rev: Rev, change: &T) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO revlog (rev, change) VALUES (?1, ?2)",
            params![int(rev.0), json(change)],
        )?;
        Ok(())
    }

    /// The oldest retained rev, if any.
    pub(crate) fn first_rev(&self) -> rusqlite::Result<Option<Rev>> {
        let rev: Option<i64> = self
            .conn
            .query_row("SELECT MIN(rev) FROM revlog", [], |r| r.get(0))?;
        Ok(rev.map(|r| Rev(uint(r))))
    }

    pub(crate) fn revs_after<T: DeserializeOwned>(&self, rev: Rev) -> rusqlite::Result<Vec<T>> {
        let mut stmt = self
            .conn
            .prepare("SELECT change FROM revlog WHERE rev > ?1 ORDER BY rev")?;
        let rows = stmt.query_map([int(rev.0)], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for raw in rows {
            if let Ok(change) = serde_json::from_str(&raw?) {
                out.push(change);
            }
        }
        Ok(out)
    }

    /// Keeps the newest `keep` changes.
    pub(crate) fn prune_revs(&self, keep: u64) -> rusqlite::Result<()> {
        let last = self.last_rev()?;
        self.conn.execute(
            "DELETE FROM revlog WHERE rev <= ?1",
            [int(last.0.saturating_sub(keep))],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Tombstones (sessions purged by retention)
    // ---------------------------------------------------------------------------------------

    pub(crate) fn add_tombstone(&self, id: SessionId, at: UnixMillis) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tombstones (session_id, purged_at) VALUES (?1, ?2)",
            params![id.to_string(), at.0],
        )?;
        Ok(())
    }

    pub(crate) fn is_tombstoned(&self, id: SessionId) -> rusqlite::Result<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM tombstones WHERE session_id = ?1",
                [id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
    }
}

/// JSON text for a value that always serializes.
pub(crate) fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// SQLite integers are `i64`; seqs and revs are `u64` far below `i64::MAX`.
pub(crate) fn int(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(crate) fn int_usize(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(crate) fn uint(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}
