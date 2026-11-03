//! A schedule's recent runs, newest first and bounded. Consecutive misses collapse into one
//! entry ("missed ×3"), and recording the same due time again (a retry after a restart, whose
//! result the daemon returns from its op log) replaces the entry instead of adding one.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ui::{ScheduleOutcome, ScheduleRun};

/// How many entries a schedule keeps.
pub(crate) const MAX_RUNS: usize = 20;

/// One delivered (or skipped, or failed) due time.
pub(crate) fn run(at: UnixMillis, outcome: ScheduleOutcome, detail: Option<String>) -> ScheduleRun {
    ScheduleRun {
        at,
        until: None,
        outcome,
        detail,
        count: 1,
    }
}

/// `count` missed due times from `first` to `last`.
pub(crate) fn missed(first: UnixMillis, last: UnixMillis, count: u32, why: &str) -> ScheduleRun {
    ScheduleRun {
        at: first,
        until: (count > 1).then_some(last),
        outcome: ScheduleOutcome::Missed,
        detail: Some(why.to_owned()),
        count,
    }
}

/// Adds `new` to `runs` (newest first).
pub(crate) fn push(runs: &mut Vec<ScheduleRun>, new: ScheduleRun) {
    match runs.first_mut() {
        Some(head)
            if head.outcome == ScheduleOutcome::Missed
                && new.outcome == ScheduleOutcome::Missed
                && head.detail == new.detail
                && new.at > head.at =>
        {
            head.until = Some(new.until.unwrap_or(new.at));
            head.count = head.count.saturating_add(new.count);
        }
        Some(head) if head.at == new.at && head.outcome != ScheduleOutcome::Missed => *head = new,
        _ => {
            runs.insert(0, new);
            runs.truncate(MAX_RUNS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHY: &str = "devbox was unreachable";

    #[test]
    fn consecutive_misses_collapse() {
        let mut runs = vec![run(UnixMillis(1), ScheduleOutcome::Fired, None)];
        push(&mut runs, missed(UnixMillis(2), UnixMillis(2), 1, WHY));
        push(&mut runs, missed(UnixMillis(3), UnixMillis(4), 2, WHY));
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].count, 3);
        assert_eq!(
            (runs[0].at, runs[0].until),
            (UnixMillis(2), Some(UnixMillis(4)))
        );
        // A different reason starts a new entry; so does a run in between.
        push(
            &mut runs,
            missed(UnixMillis(5), UnixMillis(5), 1, "the server was down"),
        );
        push(
            &mut runs,
            run(UnixMillis(6), ScheduleOutcome::Skipped, None),
        );
        push(&mut runs, missed(UnixMillis(7), UnixMillis(7), 1, WHY));
        let counts: Vec<_> = runs.iter().map(|r| (r.outcome, r.count)).collect();
        assert_eq!(
            counts,
            [
                (ScheduleOutcome::Missed, 1),
                (ScheduleOutcome::Skipped, 1),
                (ScheduleOutcome::Missed, 1),
                (ScheduleOutcome::Missed, 3),
                (ScheduleOutcome::Fired, 1)
            ]
        );
    }

    #[test]
    fn the_same_due_time_is_recorded_once_and_the_log_is_bounded() {
        let mut runs = Vec::new();
        push(&mut runs, run(UnixMillis(1), ScheduleOutcome::Queued, None));
        push(&mut runs, run(UnixMillis(1), ScheduleOutcome::Fired, None));
        assert_eq!(runs, [run(UnixMillis(1), ScheduleOutcome::Fired, None)]);
        for t in 2..40 {
            push(&mut runs, run(UnixMillis(t), ScheduleOutcome::Fired, None));
        }
        assert_eq!(runs.len(), MAX_RUNS);
        assert_eq!(runs[0].at, UnixMillis(39));
    }
}
