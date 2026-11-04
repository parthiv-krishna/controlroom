//! Schedules (spec §8.1): the controller wakes a session's agent with a canned message on a
//! cron or interval spec.
//!
//! - [`recurrence`]: next-run math (cron in an IANA zone with DST handling, intervals);
//! - [`plan`]: what is due, claimed or missed at a given time (pure);
//! - [`deliver`]: one due time as a `session.prompt` with a deterministic op id;
//! - [`runs`]: the bounded recent-run log with collapsed misses;
//! - [`scheduler`]: the per-owner task that fires due times, retries an unreachable agent daemon
//!   until the next due time, and disables the schedules of closed or deleted sessions;
//! - [`Clock`]: the system clock, or a manual one for tests.
//!
//! Schedules are controller data only: the daemon sees an ordinary prompt with
//! `origin: schedule`, and agents have no way to create them.

mod clock;
mod deliver;
mod plan;
mod recurrence;
mod runs;
mod scheduler;

pub use clock::{Clock, ManualClock};
pub(crate) use recurrence::Recurrence;
pub(crate) use runs::push as push_run;
pub(crate) use scheduler::{Scheduler, run_now};
