//! Schedules on global sessions: as [`crate::ui::Schedule`], without a machine.
//!
//! The scheduler fires `session.prompt` at the agent daemon with op id
//! `<schedule id>:<scheduled unix ms>`. A due time while the agent daemon can't be reached stays
//! `pending_run` until `next_run`, then is missed. Closing or deleting the session disables its
//! schedules.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::IfBusy;
use crate::ids::{ScheduleId, SessionId, UnixMillis};
use crate::ui::{MissedPolicy, ScheduleRun, ScheduleSpec};

/// A schedule attached to one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Schedule {
    /// The schedule.
    pub id: ScheduleId,
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
    /// The next due time, while enabled. While a run is pending, it is also that run's
    /// deadline.
    pub next_run: Option<UnixMillis>,
    /// A due time that hasn't been delivered yet because the agent daemon can't be reached. It
    /// is retried until `next_run`, then recorded as missed.
    pub pending_run: Option<UnixMillis>,
    /// Recent due times, newest first (bounded).
    pub recent_runs: Vec<ScheduleRun>,
    /// When it was created.
    pub created_at: UnixMillis,
}

/// Params of `schedules.create`. Absent fields take the defaults noted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SchedulesCreateParams {
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
