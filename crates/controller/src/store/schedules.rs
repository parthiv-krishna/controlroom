//! Schedules (spec §8.1). The RPC handlers write the user's fields; the scheduler writes the
//! timing (`next_run`, `pending_run`) and the recent runs. Both write under the hub lock, and
//! a read-modify-write of the runs happens under one connection lock.
//!
//! A schedule wakes a global session (the agent daemon's); until the old UI API goes away
//! (R2) it may instead wake a legacy daemon's session, recorded with its machine.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::daemon::IfBusy;
use controlroom_protocol::ids::{MachineId, ScheduleId, SessionId};
use controlroom_protocol::ui::{
    self, MissedPolicy, ScheduleRun, ScheduleSpec, SchedulesUpdateParams,
};
use rusqlite::{Connection, OptionalExtension};

use super::{
    OwnerId, Store, StoreResult, enum_col, enum_str, json_col, parsed_col, parsed_col_opt, to_json,
};

/// A schedule as stored; [`Self::legacy`] and [`Self::next`] are its views in the two UI APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRow {
    /// The schedule.
    pub id: ScheduleId,
    /// A legacy daemon's machine (old UI API); `None` for a global session.
    pub machine_id: Option<MachineId>,
    /// The session it wakes.
    pub session_id: SessionId,
    /// Display name.
    pub name: String,
    /// When it fires.
    pub spec: ScheduleSpec,
    /// IANA time zone.
    pub timezone: String,
    /// The prompt it sends.
    pub message: String,
    /// Whether it fires.
    pub enabled: bool,
    /// What to do while the session is busy.
    pub if_busy: IfBusy,
    /// What to do with missed runs.
    pub missed: MissedPolicy,
    /// The next due time, while enabled.
    pub next_run: Option<UnixMillis>,
    /// A due time being delivered.
    pub pending_run: Option<UnixMillis>,
    /// Recent due times, newest first.
    pub recent_runs: Vec<ScheduleRun>,
    /// When it was created.
    pub created_at: UnixMillis,
}

impl ScheduleRow {
    /// The old UI API's view, for a legacy daemon's session.
    pub fn legacy(&self) -> Option<ui::Schedule> {
        Some(ui::Schedule {
            id: self.id,
            machine_id: self.machine_id?,
            session_id: self.session_id,
            name: self.name.clone(),
            spec: self.spec.clone(),
            timezone: self.timezone.clone(),
            message: self.message.clone(),
            enabled: self.enabled,
            if_busy: self.if_busy,
            missed: self.missed,
            next_run: self.next_run,
            pending_run: self.pending_run,
            recent_runs: self.recent_runs.clone(),
            created_at: self.created_at,
        })
    }

    /// The next UI API's view, for a global session.
    pub fn next(&self) -> Option<ui::next::Schedule> {
        if self.machine_id.is_some() {
            return None;
        }
        Some(ui::next::Schedule {
            id: self.id,
            session_id: self.session_id,
            name: self.name.clone(),
            spec: self.spec.clone(),
            timezone: self.timezone.clone(),
            message: self.message.clone(),
            enabled: self.enabled,
            if_busy: self.if_busy,
            missed: self.missed,
            next_run: self.next_run,
            pending_run: self.pending_run,
            recent_runs: self.recent_runs.clone(),
            created_at: self.created_at,
        })
    }
}

/// The fields of a new schedule.
#[derive(Debug, Clone)]
pub struct NewSchedule {
    /// A legacy daemon's machine; `None` for a global session.
    pub machine_id: Option<MachineId>,
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
    /// The first due time (`None` while disabled).
    pub next_run: Option<UnixMillis>,
}

/// A schedule's timing, written by the scheduler (and reset when the spec or `enabled`
/// changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// The next due time.
    pub next_run: Option<UnixMillis>,
    /// A due time being delivered.
    pub pending_run: Option<UnixMillis>,
}

/// Owner-scoped schedule operations.
pub struct Schedules<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "id, machine_id, session_id, name, spec, timezone, message, enabled, \
                       if_busy, missed, next_run, pending_run, recent_runs, created_at";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRow> {
    let millis =
        |idx| -> rusqlite::Result<_> { Ok(row.get::<_, Option<i64>>(idx)?.map(UnixMillis)) };
    Ok(ScheduleRow {
        id: parsed_col(row, 0)?,
        machine_id: parsed_col_opt(row, 1)?,
        session_id: parsed_col(row, 2)?,
        name: row.get(3)?,
        spec: json_col(row, 4)?,
        timezone: row.get(5)?,
        message: row.get(6)?,
        enabled: row.get(7)?,
        if_busy: enum_col(row, 8)?,
        missed: enum_col(row, 9)?,
        next_run: millis(10)?,
        pending_run: millis(11)?,
        recent_runs: json_col(row, 12)?,
        created_at: UnixMillis(row.get(13)?),
    })
}

impl<'a> Schedules<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Creates a schedule.
    pub fn insert(&self, s: NewSchedule) -> StoreResult<ScheduleRow> {
        let id = ScheduleId::generate();
        self.store.lock().execute(
            "INSERT INTO schedules (id, owner_id, machine_id, session_id, name, spec, timezone,
                                    message, enabled, if_busy, missed, next_run, recent_runs,
                                    created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, '[]', ?13)",
            rusqlite::params![
                id.to_string(),
                self.owner.as_str(),
                s.machine_id.map(|m| m.to_string()),
                s.session_id.to_string(),
                s.name,
                to_json(&s.spec)?,
                s.timezone,
                s.message,
                s.enabled,
                enum_str(&s.if_busy)?,
                enum_str(&s.missed)?,
                s.next_run.map(|t| t.0),
                UnixMillis::now().0
            ],
        )?;
        self.get(id)?
            .ok_or_else(|| super::StoreError::Conflict("the schedule vanished".into()))
    }

    /// A schedule by id.
    pub fn get(&self, id: ScheduleId) -> StoreResult<Option<ScheduleRow>> {
        self.get_in(&self.store.lock(), id)
    }

    fn get_in(&self, conn: &Connection, id: ScheduleId) -> StoreResult<Option<ScheduleRow>> {
        Ok(conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM schedules WHERE owner_id = ?1 AND id = ?2"),
                (self.owner.as_str(), id.to_string()),
                map,
            )
            .optional()?)
    }

    /// Every schedule, oldest first.
    pub fn list(&self) -> StoreResult<Vec<ScheduleRow>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE owner_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The enabled schedules of one session.
    pub fn enabled_for_session(&self, session: SessionId) -> StoreResult<Vec<ScheduleId>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT id FROM schedules WHERE owner_id = ?1 AND session_id = ?2 AND enabled = 1",
        )?;
        let rows = stmt
            .query_map((self.owner.as_str(), session.to_string()), |row| {
                parsed_col(row, 0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Updates the given fields only, and the timing when given; returns whether the schedule
    /// exists.
    pub fn update(&self, p: &SchedulesUpdateParams, timing: Option<Timing>) -> StoreResult<bool> {
        let conn = self.store.lock();
        let n = conn.execute(
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
        if n > 0
            && let Some(timing) = timing
        {
            self.set_timing_in(&conn, p.schedule_id, timing)?;
        }
        Ok(n > 0)
    }

    fn set_timing_in(&self, conn: &Connection, id: ScheduleId, t: Timing) -> StoreResult<()> {
        conn.execute(
            "UPDATE schedules SET next_run = ?3, pending_run = ?4 WHERE owner_id = ?1 AND id = ?2",
            rusqlite::params![
                self.owner.as_str(),
                id.to_string(),
                t.next_run.map(|t| t.0),
                t.pending_run.map(|t| t.0)
            ],
        )?;
        Ok(())
    }

    /// Records runs (oldest first) and sets the timing; returns the schedule, if it exists.
    pub fn advance(
        &self,
        id: ScheduleId,
        runs: &[ScheduleRun],
        timing: Timing,
    ) -> StoreResult<Option<ScheduleRow>> {
        let conn = self.store.lock();
        if !self.push_runs_in(&conn, id, runs)? {
            return Ok(None);
        }
        self.set_timing_in(&conn, id, timing)?;
        self.get_in(&conn, id)
    }

    /// Records a run; when `resolves` is the pending due time, it is no longer pending.
    /// Returns the schedule, if it exists.
    pub fn record(
        &self,
        id: ScheduleId,
        run: ScheduleRun,
        resolves: Option<UnixMillis>,
    ) -> StoreResult<Option<ScheduleRow>> {
        let conn = self.store.lock();
        if !self.push_runs_in(&conn, id, &[run])? {
            return Ok(None);
        }
        if let Some(at) = resolves {
            conn.execute(
                "UPDATE schedules SET pending_run = NULL
                 WHERE owner_id = ?1 AND id = ?2 AND pending_run = ?3",
                rusqlite::params![self.owner.as_str(), id.to_string(), at.0],
            )?;
        }
        self.get_in(&conn, id)
    }

    fn push_runs_in(
        &self,
        conn: &Connection,
        id: ScheduleId,
        new: &[ScheduleRun],
    ) -> StoreResult<bool> {
        let runs: Option<String> = conn
            .query_row(
                "SELECT recent_runs FROM schedules WHERE owner_id = ?1 AND id = ?2",
                (self.owner.as_str(), id.to_string()),
                |row| row.get(0),
            )
            .optional()?;
        let Some(runs) = runs else {
            return Ok(false);
        };
        if new.is_empty() {
            return Ok(true);
        }
        let mut runs: Vec<ScheduleRun> = serde_json::from_str(&runs)?;
        for run in new {
            crate::schedules::push_run(&mut runs, run.clone());
        }
        conn.execute(
            "UPDATE schedules SET recent_runs = ?3 WHERE owner_id = ?1 AND id = ?2",
            rusqlite::params![self.owner.as_str(), id.to_string(), to_json(&runs)?],
        )?;
        Ok(true)
    }

    /// Disables a schedule (nothing is due or pending anymore); returns it if it exists.
    pub fn disable(&self, id: ScheduleId) -> StoreResult<Option<ScheduleRow>> {
        let conn = self.store.lock();
        conn.execute(
            "UPDATE schedules SET enabled = 0, next_run = NULL, pending_run = NULL
             WHERE owner_id = ?1 AND id = ?2",
            (self.owner.as_str(), id.to_string()),
        )?;
        self.get_in(&conn, id)
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
