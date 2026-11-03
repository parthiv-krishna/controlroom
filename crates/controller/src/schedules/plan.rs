//! The scheduler's decisions, as a pure function of a schedule's timing and the time.
//!
//! A schedule's timing is `next_run` (the next due time; while a run is pending, also its
//! deadline) and `pending_run` (a claimed due time not delivered yet). When `next_run` passes:
//!
//! - a pending run is missed (its machine stayed unreachable until the next scheduled time);
//! - due times that passed without the scheduler claiming them (only possible while the
//!   controller was down or stalled) are missed, except the latest, which is claimed and
//!   delivered like any due time;
//! - right after a controller start, every due time that passed during the downtime follows
//!   the schedule's `missed` policy instead: `skip` records them all as missed and claims
//!   none, `run_once` claims only the latest.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ui::{MissedPolicy, ScheduleRun};

use super::recurrence::Recurrence;
use super::runs;

/// Why a pending run was missed.
pub(crate) const UNREACHABLE: &str = "the machine was unreachable";
/// Why unclaimed due times were missed.
pub(crate) const DOWN: &str = "the server was down";

/// What to change about a schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    /// Misses to record, oldest first.
    pub missed: Vec<ScheduleRun>,
    /// The due time to deliver now, and a note for its run entry.
    pub claim: Option<(UnixMillis, Option<String>)>,
    /// The new `next_run`.
    pub next_run: Option<UnixMillis>,
    /// The new `pending_run`: the claimed due time, if any.
    pub pending_run: Option<UnixMillis>,
}

/// What happens to a schedule at `now`; `None` when nothing changes. `downtime` is the
/// schedule's `missed` policy on the first pass after a controller start.
pub(crate) fn step(
    r: &Recurrence,
    next_run: Option<UnixMillis>,
    pending_run: Option<UnixMillis>,
    now: UnixMillis,
    downtime: Option<MissedPolicy>,
) -> Option<Step> {
    let Some(next) = next_run else {
        // An enabled schedule without a next run (e.g. the spec was repaired): start over.
        return Some(Step {
            missed: Vec::new(),
            claim: None,
            next_run: r.first_after(now),
            pending_run,
        });
    };
    let due = r.due_until(next, now)?;
    let mut missed = Vec::new();
    if let Some(p) = pending_run {
        missed.push(runs::missed(p, p, 1, UNREACHABLE));
    }
    let claim = match downtime {
        None => {
            if due.count > 1 {
                let before_last = r.due_until(next, UnixMillis(due.last.0 - 1))?;
                missed.push(runs::missed(next, before_last.last, due.count - 1, DOWN));
            }
            Some((due.last, None))
        }
        Some(MissedPolicy::RunOnce) => {
            let note = if due.count > 1 {
                format!(
                    "run once for {} runs missed while the server was down",
                    due.count
                )
            } else {
                "run late: the server was down".to_owned()
            };
            Some((due.last, Some(note)))
        }
        Some(MissedPolicy::Skip | MissedPolicy::Unknown) => {
            missed.push(runs::missed(next, due.last, due.count, DOWN));
            None
        }
    };
    Some(Step {
        missed,
        pending_run: claim.as_ref().map(|(at, _)| *at),
        claim,
        next_run: r.following(due.last),
    })
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::ui::{ScheduleOutcome, ScheduleSpec};

    use super::*;

    const MIN: i64 = 60_000;

    fn every_10_min() -> Recurrence {
        Recurrence::new(&ScheduleSpec::Interval { every_s: 600 }, "UTC").unwrap()
    }

    fn t(minutes: i64) -> UnixMillis {
        UnixMillis(minutes * MIN)
    }

    #[test]
    fn nothing_happens_before_the_next_run() {
        let r = every_10_min();
        assert_eq!(step(&r, Some(t(10)), None, t(9), None), None);
        // A pending run is retried until its deadline.
        assert_eq!(step(&r, Some(t(20)), Some(t(10)), t(19), None), None);
        assert_eq!(
            step(
                &r,
                Some(t(20)),
                Some(t(10)),
                t(19),
                Some(MissedPolicy::Skip)
            ),
            None
        );
    }

    #[test]
    fn a_due_time_is_claimed_and_the_next_one_scheduled() {
        let s = step(&every_10_min(), Some(t(10)), None, t(10), None).unwrap();
        assert_eq!(
            s,
            Step {
                missed: vec![],
                claim: Some((t(10), None)),
                next_run: Some(t(20)),
                pending_run: Some(t(10)),
            }
        );
    }

    #[test]
    fn a_pending_run_is_missed_at_the_next_due_time() {
        let s = step(&every_10_min(), Some(t(20)), Some(t(10)), t(20), None).unwrap();
        assert_eq!(s.missed, [runs::missed(t(10), t(10), 1, UNREACHABLE)]);
        assert_eq!(s.claim, Some((t(20), None)));
        assert_eq!(s.next_run, Some(t(30)));
    }

    #[test]
    fn a_stalled_scheduler_claims_only_the_latest_due_time() {
        let s = step(&every_10_min(), Some(t(10)), None, t(45), None).unwrap();
        assert_eq!(s.missed, [runs::missed(t(10), t(30), 3, DOWN)]);
        assert_eq!(s.claim, Some((t(40), None)));
        assert_eq!(s.next_run, Some(t(50)));
    }

    #[test]
    fn downtime_with_skip_records_every_due_time_as_missed() {
        let s = step(
            &every_10_min(),
            Some(t(20)),
            Some(t(10)),
            t(45),
            Some(MissedPolicy::Skip),
        )
        .unwrap();
        assert_eq!(
            s.missed,
            [
                runs::missed(t(10), t(10), 1, UNREACHABLE),
                runs::missed(t(20), t(40), 3, DOWN)
            ]
        );
        assert_eq!((s.claim, s.pending_run), (None, None));
        assert_eq!(s.next_run, Some(t(50)));
        assert_eq!(s.missed[1].outcome, ScheduleOutcome::Missed);
    }

    #[test]
    fn downtime_with_run_once_claims_the_latest_due_time_once() {
        let r = every_10_min();
        let s = step(&r, Some(t(10)), None, t(45), Some(MissedPolicy::RunOnce)).unwrap();
        assert!(s.missed.is_empty());
        let (at, note) = s.claim.unwrap();
        assert_eq!(at, t(40));
        assert!(note.unwrap().contains("4 runs missed"));
        assert_eq!(s.next_run, Some(t(50)));
        // A single late due time is run late.
        let s = step(&r, Some(t(10)), None, t(12), Some(MissedPolicy::RunOnce)).unwrap();
        assert_eq!(s.claim.unwrap().0, t(10));
    }

    #[test]
    fn a_missing_next_run_starts_over() {
        let s = step(&every_10_min(), None, None, t(3), None).unwrap();
        assert_eq!(s.next_run, Some(t(13)));
        assert_eq!(s.claim, None);
    }
}
