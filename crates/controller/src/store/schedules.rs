//! Schedules (spec §8.1): data only. The scheduler that fires them arrives in P5 and will own
//! `next_run` and `recent_runs`.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::daemon::IfBusy;
use controlroom_protocol::ids::{MachineId, ScheduleId, SessionId};
use controlroom_protocol::ui::{MissedPolicy, Schedule, ScheduleSpec, SchedulesUpdateParams};
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreResult, enum_col, enum_str, json_col, parsed_col, to_json};

/// The fields of a new schedule.
#[derive(Debug, Clone)]
pub struct NewSchedule {
    /// The session's machine.
    pub machine_id: MachineId,
    /// The session.
    pub session_id: SessionId,
    /// Display name.
    pub name: String,
    /// When it fires.
    pub spec: ScheduleSpec,
    /// IANA time zone.
    pub timezone: String,
    /// The prompt.
    pub message: String,
    /// Whether it fires.
    pub enabled: bool,
    /// Busy policy.
    pub if_busy: IfBusy,
    /// Missed-run policy.
    pub missed: MissedPolicy,
}

/// Owner-scoped schedule operations.
pub struct Schedules<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "id, machine_id, session_id, name, spec, timezone, message, enabled, \
                       if_busy, missed, next_run, recent_runs, created_at";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: parsed_col(row, 0)?,
        machine_id: parsed_col(row, 1)?,
        session_id: parsed_col(row, 2)?,
        name: row.get(3)?,
        spec: json_col(row, 4)?,
        timezone: row.get(5)?,
        message: row.get(6)?,
        enabled: row.get(7)?,
        if_busy: enum_col(row, 8)?,
        missed: enum_col(row, 9)?,
        next_run: row.get::<_, Option<i64>>(10)?.map(UnixMillis),
        recent_runs: json_col(row, 11)?,
        created_at: UnixMillis(row.get(12)?),
    })
}

impl<'a> Schedules<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Creates a schedule.
    pub fn insert(&self, s: NewSchedule) -> StoreResult<Schedule> {
        let id = ScheduleId::generate();
        self.store.lock().execute(
            "INSERT INTO schedules (id, owner_id, machine_id, session_id, name, spec, timezone,
                                    message, enabled, if_busy, missed, recent_runs, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, '[]', ?12)",
            rusqlite::params![
                id.to_string(),
                self.owner.as_str(),
                s.machine_id.to_string(),
                s.session_id.to_string(),
                s.name,
                to_json(&s.spec)?,
                s.timezone,
                s.message,
                s.enabled,
                enum_str(&s.if_busy)?,
                enum_str(&s.missed)?,
                UnixMillis::now().0
            ],
        )?;
        self.get(id)?
            .ok_or_else(|| super::StoreError::Conflict("the schedule vanished".into()))
    }

    /// A schedule by id.
    pub fn get(&self, id: ScheduleId) -> StoreResult<Option<Schedule>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!("SELECT {COLUMNS} FROM schedules WHERE owner_id = ?1 AND id = ?2"),
                (self.owner.as_str(), id.to_string()),
                map,
            )
            .optional()?)
    }

    /// Every schedule, oldest first.
    pub fn list(&self) -> StoreResult<Vec<Schedule>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE owner_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Updates the given fields only; returns whether the schedule exists.
    pub fn update(&self, p: &SchedulesUpdateParams) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "UPDATE schedules SET name = COALESCE(?3, name), spec = COALESCE(?4, spec),
                 timezone = COALESCE(?5, timezone), message = COALESCE(?6, message),
                 enabled = COALESCE(?7, enabled), if_busy = COALESCE(?8, if_busy),
                 missed = COALESCE(?9, missed)
             WHERE owner_id = ?1 AND id = ?2",
            rusqlite::params![
                self.owner.as_str(),
                p.schedule_id.to_string(),
                p.name,
                p.spec.as_ref().map(to_json).transpose()?,
                p.timezone,
                p.message,
                p.enabled,
                p.if_busy.as_ref().map(enum_str).transpose()?,
                p.missed.as_ref().map(enum_str).transpose()?,
            ],
        )?;
        Ok(n > 0)
    }

    /// Deletes a schedule; returns whether it existed.
    pub fn delete(&self, id: ScheduleId) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "DELETE FROM schedules WHERE owner_id = ?1 AND id = ?2",
            (self.owner.as_str(), id.to_string()),
        )?;
        Ok(n > 0)
    }
}
