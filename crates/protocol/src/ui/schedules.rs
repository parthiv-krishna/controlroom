//! Schedules: owned by the controller, each wakes one session with a canned message.
//!
//! The scheduler fires `session.prompt` with op id `<schedule id>:<scheduled unix ms>`, so a
//! restart or retry can never double-fire. The run's trigger shows as "schedule: <name>".
//! Closing the session disables its schedules. Not exposed to agents.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::IfBusy;
use crate::ids::{MachineId, ScheduleId, SessionId, UnixMillis};

/// When a schedule fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// A five-field cron expression, evaluated in the schedule's time zone.
    Cron {
        /// e.g. `"0 9 * * 1-5"` (weekdays at 09:00).
        expr: String,
    },
    /// A fixed interval.
    Interval {
        /// Seconds between runs.
        every_s: u32,
    },
    /// A spec this build doesn't know; shown read-only and never fired.
    #[serde(other)]
    Unknown,
}

/// What happens to runs missed while the controller was down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    /// Skip them (the default).
    #[default]
    Skip,
    /// Run once for all of them.
    RunOnce,
    /// A policy this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What happened when a schedule was due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleOutcome {
    /// Sent, starting a run.
    Fired,
    /// Queued behind an active run.
    Queued,
    /// Skipped because the session was busy (`if_busy: skip`).
    Skipped,
    /// Missed: the machine stayed unreachable until the next scheduled time, or the controller
    /// was down.
    Missed,
    /// The prompt was rejected (e.g. the session is closed).
    Failed,
    /// An outcome this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One due time of a schedule. Consecutive misses collapse into one entry ("missed ×3").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ScheduleRun {
    /// The scheduled time (the first one, for collapsed misses).
    pub at: UnixMillis,
    /// What happened.
    pub outcome: ScheduleOutcome,
    /// Details, e.g. the error.
    pub detail: Option<String>,
    /// How many due times this entry covers (more than 1 only for collapsed misses).
    pub count: u32,
}

/// A schedule attached to one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Schedule {
    /// The schedule.
    pub id: ScheduleId,
    /// The session's machine.
    pub machine_id: MachineId,
    /// The session it wakes.
    pub session_id: SessionId,
    /// Display name.
    pub name: String,
    /// When it fires.
    pub spec: ScheduleSpec,
    /// IANA time zone, e.g. `"Europe/Berlin"`.
    pub timezone: String,
    /// The prompt it sends.
    pub message: String,
    /// Whether it fires.
    pub enabled: bool,
    /// What to do while the session is busy.
    pub if_busy: IfBusy,
    /// What to do with runs missed during controller downtime.
    pub missed: MissedPolicy,
    /// The next due time, while enabled.
    pub next_run: Option<UnixMillis>,
    /// Recent due times, newest first (bounded).
    pub recent_runs: Vec<ScheduleRun>,
    /// When it was created.
    pub created_at: UnixMillis,
}

/// Params of `schedules.create`. Absent fields take the defaults noted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SchedulesCreateParams {
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
    /// The prompt it sends.
    pub message: String,
    /// Default `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Default `queue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_busy: Option<IfBusy>,
    /// Default `skip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed: Option<MissedPolicy>,
}

/// Params of `schedules.update`. Absent fields are left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SchedulesUpdateParams {
    /// The schedule.
    pub schedule_id: ScheduleId,
    /// New name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<ScheduleSpec>,
    /// New time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// New message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Enable or disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// New busy policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_busy: Option<IfBusy>,
    /// New missed policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed: Option<MissedPolicy>,
}

/// Params of `schedules.delete` and `schedules.run_now`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ScheduleRef {
    /// The schedule.
    pub schedule_id: ScheduleId,
}
