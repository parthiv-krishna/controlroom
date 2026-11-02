//! Sessions, agent sessions, queue, decisions and timeline items.

use std::collections::BTreeMap;
use std::str::FromStr;

use controlroom_protocol::ids::{ItemId, RemotePath, Seq, SessionId, UnixMillis};
use controlroom_protocol::session::{AgentSession, Request};
use controlroom_protocol::timeline::{Actor, ItemBody, TimelineItem};
use rusqlite::{OptionalExtension, Row, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{Store, json};
use super::{int, int_usize, uint};
use crate::model::{AgentSessionRow, Decision, SessionData};

const SESSION_COLUMNS: &str = "id, agent, driver, cwd, name, created_at, closed_at, parent_id,
    spawned_by, sandbox_level, sandbox, read_seq, last_turn_end_seq, last_outcome, last_error,
    queue_paused, auto_send, pending_start, history_note, resume_failed, applied_seq,
    timeline_seq";

/// A timeline item as stored. Not the wire form, which doesn't round-trip: the body is
/// flattened into the item, so a `request` body's own `id` and `kind` collide with the item's
/// `id` and the body's `kind` tag (reported as a protocol issue).
#[derive(Serialize, Deserialize)]
struct StoredItem {
    id: ItemId,
    seq_from: Seq,
    seq_to: Seq,
    actor: Actor,
    ts: UnixMillis,
    streaming: bool,
    body: StoredBody,
}

/// [`ItemBody`] with requests nested instead of flattened into the tagged body.
#[derive(Serialize, Deserialize)]
enum StoredBody {
    Request(Request),
    Other(ItemBody),
}

impl From<StoredBody> for ItemBody {
    fn from(body: StoredBody) -> Self {
        match body {
            StoredBody::Request(request) => ItemBody::Request(request),
            StoredBody::Other(body) => body,
        }
    }
}

impl From<StoredItem> for TimelineItem {
    fn from(s: StoredItem) -> Self {
        TimelineItem {
            id: s.id,
            seq_from: s.seq_from,
            seq_to: s.seq_to,
            actor: s.actor,
            ts: s.ts,
            streaming: s.streaming,
            body: s.body.into(),
        }
    }
}

fn stored(item: &TimelineItem) -> StoredItem {
    StoredItem {
        id: item.id.clone(),
        seq_from: item.seq_from,
        seq_to: item.seq_to,
        actor: item.actor,
        ts: item.ts,
        streaming: item.streaming,
        body: match &item.body {
            ItemBody::Request(request) => StoredBody::Request(request.clone()),
            other => StoredBody::Other(other.clone()),
        },
    }
}

fn decode<T: DeserializeOwned>(raw: &str) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn decode_opt<T: DeserializeOwned>(raw: Option<String>) -> rusqlite::Result<Option<T>> {
    raw.as_deref().map(decode).transpose()
}

fn session_id(raw: &str) -> rusqlite::Result<SessionId> {
    SessionId::from_str(raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn session_row(row: &Row<'_>) -> rusqlite::Result<SessionData> {
    let millis = |i: usize| row.get::<_, Option<i64>>(i).map(|v| v.map(UnixMillis));
    Ok(SessionData {
        id: session_id(&row.get::<_, String>(0)?)?,
        agent: row.get(1)?,
        driver: decode(&row.get::<_, String>(2)?)?,
        cwd: RemotePath::new(row.get::<_, String>(3)?),
        name: row.get(4)?,
        created_at: UnixMillis(row.get(5)?),
        closed_at: millis(6)?,
        parent_id: row
            .get::<_, Option<String>>(7)?
            .as_deref()
            .map(session_id)
            .transpose()?,
        spawned_by: decode_opt(row.get(8)?)?,
        sandbox_level: decode_opt(row.get(9)?)?,
        sandbox: decode(&row.get::<_, String>(10)?)?,
        read_seq: Seq(uint(row.get(11)?)),
        last_turn_end_seq: Seq(uint(row.get(12)?)),
        last_outcome: decode_opt(row.get(13)?)?,
        last_error: row.get(14)?,
        queue_paused: row.get(15)?,
        auto_send: row.get(16)?,
        pending_start: decode_opt(row.get(17)?)?,
        history_note: row.get::<_, Option<String>>(18)?.map(RemotePath::new),
        resume_failed: row.get(19)?,
        applied_seq: Seq(uint(row.get(20)?)),
        timeline_seq: Seq(uint(row.get(21)?)),
        agent_sessions: Vec::new(),
        queue: Vec::new(),
        decisions: BTreeMap::new(),
    })
}

impl Store {
    /// Every session with its agent sessions, queue and decisions.
    pub(crate) fn load_sessions(&self) -> rusqlite::Result<Vec<SessionData>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY id"
        ))?;
        let rows = stmt.query_map([], session_row)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(self.complete(row?)?);
        }
        Ok(sessions)
    }

    /// One session, as committed.
    pub(crate) fn load_session(&self, id: SessionId) -> rusqlite::Result<Option<SessionData>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
                [id.to_string()],
                session_row,
            )
            .optional()?;
        row.map(|data| self.complete(data)).transpose()
    }

    fn complete(&self, mut data: SessionData) -> rusqlite::Result<SessionData> {
        let id = data.id.to_string();
        let mut stmt = self.conn.prepare(
            "SELECT driver, native_id, native_path, started_at, ended_at, started_by
             FROM agent_sessions WHERE session_id = ?1 ORDER BY idx",
        )?;
        for row in stmt.query_map([&id], |r| {
            Ok(AgentSessionRow {
                session: AgentSession {
                    driver: decode(&r.get::<_, String>(0)?)?,
                    native_id: r.get(1)?,
                    started_at: UnixMillis(r.get(3)?),
                    ended_at: r.get::<_, Option<i64>>(4)?.map(UnixMillis),
                    started_by: decode(&r.get::<_, String>(5)?)?,
                },
                native_path: r.get(2)?,
            })
        })? {
            data.agent_sessions.push(row?);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT entry FROM queue WHERE session_id = ?1 ORDER BY position")?;
        for raw in stmt.query_map([&id], |r| r.get::<_, String>(0))? {
            data.queue.push(decode(&raw?)?);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT decision FROM decisions WHERE session_id = ?1")?;
        for raw in stmt.query_map([&id], |r| r.get::<_, String>(0))? {
            let decision: Decision = decode(&raw?)?;
            data.decisions.insert(decision.request.id.clone(), decision);
        }
        Ok(data)
    }

    /// Saves what changed between `before` (the committed state, if any) and `after`.
    pub(crate) fn save_session(
        &self,
        before: Option<&SessionData>,
        after: &SessionData,
    ) -> rusqlite::Result<()> {
        let id = after.id.to_string();
        let row_changed = before.is_none_or(|b| {
            let mut b = b.clone();
            b.agent_sessions.clone_from(&after.agent_sessions);
            b.queue.clone_from(&after.queue);
            b.decisions.clone_from(&after.decisions);
            b != *after
        });
        if row_changed {
            self.save_row(after)?;
        }
        if before.is_none_or(|b| b.agent_sessions != after.agent_sessions) {
            self.conn
                .execute("DELETE FROM agent_sessions WHERE session_id = ?1", [&id])?;
            for (idx, row) in after.agent_sessions.iter().enumerate() {
                let s = &row.session;
                self.conn.execute(
                    "INSERT INTO agent_sessions (session_id, idx, driver, native_id, native_path,
                     started_at, ended_at, started_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        int_usize(idx),
                        json(&s.driver),
                        s.native_id,
                        row.native_path,
                        s.started_at.0,
                        s.ended_at.map(|t| t.0),
                        json(&s.started_by)
                    ],
                )?;
            }
        }
        if before.is_none_or(|b| b.queue != after.queue) {
            self.conn
                .execute("DELETE FROM queue WHERE session_id = ?1", [&id])?;
            for (position, entry) in after.queue.iter().enumerate() {
                self.conn.execute(
                    "INSERT INTO queue (id, session_id, position, entry) VALUES (?1, ?2, ?3, ?4)",
                    params![entry.id.to_string(), id, int_usize(position), json(entry)],
                )?;
            }
        }
        for (request_id, decision) in &after.decisions {
            if before.is_some_and(|b| b.decisions.get(request_id) == Some(decision)) {
                continue;
            }
            self.conn.execute(
                "INSERT OR REPLACE INTO decisions (session_id, request_id, decision)
                 VALUES (?1, ?2, ?3)",
                params![id, request_id.as_str(), json(decision)],
            )?;
        }
        Ok(())
    }

    fn save_row(&self, d: &SessionData) -> rusqlite::Result<()> {
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO sessions ({SESSION_COLUMNS}) VALUES
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                  ?18, ?19, ?20, ?21, ?22)"
            ),
            params![
                d.id.to_string(),
                d.agent,
                json(&d.driver),
                d.cwd.as_str(),
                d.name,
                d.created_at.0,
                d.closed_at.map(|t| t.0),
                d.parent_id.map(|p| p.to_string()),
                d.spawned_by.as_ref().map(json),
                d.sandbox_level.as_ref().map(json),
                json(&d.sandbox),
                int(d.read_seq.0),
                int(d.last_turn_end_seq.0),
                d.last_outcome.as_ref().map(json),
                d.last_error,
                d.queue_paused,
                d.auto_send,
                d.pending_start.as_ref().map(json),
                d.history_note.as_ref().map(RemotePath::as_str),
                d.resume_failed,
                int(d.applied_seq.0),
                int(d.timeline_seq.0),
            ],
        )?;
        Ok(())
    }

    /// Deletes a session and everything stored for it.
    pub(crate) fn delete_session(&self, id: SessionId) -> rusqlite::Result<()> {
        let id = id.to_string();
        for table in [
            "sessions",
            "agent_sessions",
            "queue",
            "decisions",
            "timeline_items",
        ] {
            let column = if table == "sessions" {
                "id"
            } else {
                "session_id"
            };
            self.conn
                .execute(&format!("DELETE FROM {table} WHERE {column} = ?1"), [&id])?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // The materialized timeline
    // ---------------------------------------------------------------------------------------

    pub(crate) fn timeline_item(
        &self,
        id: SessionId,
        item: &ItemId,
    ) -> rusqlite::Result<Option<(TimelineItem, Seq)>> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT item, changed_seq FROM timeline_items
                 WHERE session_id = ?1 AND item_id = ?2",
                params![id.to_string(), item.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(raw, seq)| Ok((decode::<StoredItem>(&raw)?.into(), Seq(uint(seq)))))
            .transpose()
    }

    pub(crate) fn put_timeline_item(
        &self,
        id: SessionId,
        item: &TimelineItem,
        changed: Seq,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO timeline_items (session_id, item_id, seq_from, changed_seq, item)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                item.id.as_str(),
                int(item.seq_from.0),
                int(changed.0),
                json(&stored(item))
            ],
        )?;
        Ok(())
    }

    /// Items changed after `since`, ordered by `seq_from`.
    pub(crate) fn timeline_since(
        &self,
        id: SessionId,
        since: Seq,
    ) -> rusqlite::Result<Vec<TimelineItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT item FROM timeline_items WHERE session_id = ?1 AND changed_seq > ?2
             ORDER BY seq_from, item_id",
        )?;
        let rows = stmt.query_map(params![id.to_string(), int(since.0)], |r| {
            r.get::<_, String>(0)
        })?;
        let mut items = Vec::new();
        for raw in rows {
            items.push(decode::<StoredItem>(&raw?)?.into());
        }
        Ok(items)
    }
}
