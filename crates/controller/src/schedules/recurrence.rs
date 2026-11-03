//! When a schedule is due: five-field cron expressions in an IANA time zone (DST handled by
//! croner: a time skipped by spring-forward runs right after the gap, a repeated wall-clock
//! time runs once), and fixed intervals.
//!
//! Interval schedules keep their cadence: the due times are the first due time plus whole
//! intervals, and the first one is an interval after the schedule was created or changed.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::ui::ScheduleSpec;
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};
use jiff::Timestamp;
use jiff::tz::TimeZone;

/// The shortest interval.
pub(crate) const MIN_INTERVAL_S: u32 = 60;

/// How many due times [`Recurrence::due_until`] counts exactly; more are reported as this many.
pub(crate) const MAX_COUNTED: u32 = 10_000;

/// A parsed, valid schedule spec.
#[derive(Debug, Clone)]
pub(crate) enum Recurrence {
    /// A cron expression in a time zone.
    Cron { cron: Box<Cron>, tz: TimeZone },
    /// A fixed interval.
    Interval { every_ms: i64 },
}

/// Why a spec is invalid; the message is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct SpecError(String);

/// Due times in a range: how many (up to [`MAX_COUNTED`]) and the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Due {
    pub count: u32,
    pub last: UnixMillis,
}

impl Recurrence {
    /// Parses a spec and its time zone (only cron expressions use the zone, but it must be
    /// valid either way).
    pub(crate) fn new(spec: &ScheduleSpec, timezone: &str) -> Result<Self, SpecError> {
        let tz = TimeZone::get(timezone)
            .map_err(|_| SpecError(format!("unknown time zone \"{timezone}\"")))?;
        match spec {
            ScheduleSpec::Cron { expr } => {
                if expr.split_whitespace().count() != 5 {
                    return Err(SpecError(
                        "cron expressions have five fields: minute hour day month weekday".into(),
                    ));
                }
                let cron = CronParser::builder()
                    .seconds(Seconds::Disallowed)
                    .year(Year::Disallowed)
                    .build()
                    .parse(expr)
                    .map_err(|e| SpecError(format!("invalid cron expression: {e}")))?;
                Ok(Self::Cron {
                    cron: Box::new(cron),
                    tz,
                })
            }
            ScheduleSpec::Interval { every_s } if *every_s >= MIN_INTERVAL_S => {
                Ok(Self::Interval {
                    every_ms: i64::from(*every_s) * 1000,
                })
            }
            ScheduleSpec::Interval { .. } => Err(SpecError(format!(
                "intervals are at least {MIN_INTERVAL_S} s"
            ))),
            ScheduleSpec::Unknown => Err(SpecError("unknown schedule spec".into())),
        }
    }

    /// The first due time of a schedule created or changed at `now`.
    pub(crate) fn first_after(&self, now: UnixMillis) -> Option<UnixMillis> {
        match self {
            Self::Cron { .. } => self.following(now),
            Self::Interval { every_ms } => now.0.checked_add(*every_ms).map(UnixMillis),
        }
    }

    /// The due time after due time `t`.
    pub(crate) fn following(&self, t: UnixMillis) -> Option<UnixMillis> {
        match self {
            Self::Cron { cron, tz } => {
                let start = zoned(t, tz)?;
                let next = cron.find_next_occurrence(&start, false).ok()?;
                Some(UnixMillis(next.timestamp().as_millisecond()))
            }
            Self::Interval { every_ms } => t.0.checked_add(*every_ms).map(UnixMillis),
        }
    }

    /// The due times from `first` (itself a due time) up to and including `until`; `None` when
    /// `first` is after `until`.
    pub(crate) fn due_until(&self, first: UnixMillis, until: UnixMillis) -> Option<Due> {
        if first > until {
            return None;
        }
        match self {
            Self::Interval { every_ms } => {
                let steps = (until.0 - first.0) / every_ms;
                Some(Due {
                    count: u32::try_from(steps.saturating_add(1))
                        .unwrap_or(u32::MAX)
                        .min(MAX_COUNTED),
                    last: UnixMillis(first.0 + steps * every_ms),
                })
            }
            Self::Cron { cron, tz } => {
                let end = zoned(until, tz)?;
                let last = cron.find_previous_occurrence(&end, true).ok()?;
                let last = UnixMillis(last.timestamp().as_millisecond()).max(first);
                let mut count = 1;
                let mut t = first;
                while t < last && count < MAX_COUNTED {
                    match self.following(t) {
                        Some(next) if next <= last => {
                            count += 1;
                            t = next;
                        }
                        _ => break,
                    }
                }
                Some(Due { count, last })
            }
        }
    }
}

fn zoned(t: UnixMillis, tz: &TimeZone) -> Option<jiff::Zoned> {
    Some(Timestamp::from_millisecond(t.0).ok()?.to_zoned(tz.clone()))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn cron(expr: &str, tz: &str) -> Recurrence {
        Recurrence::new(
            &ScheduleSpec::Cron {
                expr: expr.to_owned(),
            },
            tz,
        )
        .unwrap()
    }

    /// A wall-clock time in `tz` (the earlier instant when it happens twice).
    fn at(tz: &str, y: i16, mo: i8, d: i8, h: i8, mi: i8) -> UnixMillis {
        let zone = TimeZone::get(tz).unwrap();
        let z = zone
            .to_ambiguous_zoned(date(y, mo, d).at(h, mi, 0, 0))
            .earlier()
            .unwrap();
        UnixMillis(z.timestamp().as_millisecond())
    }

    const HOUR: i64 = 3_600_000;

    #[test]
    fn weekdays_at_nine_in_the_schedules_zone() {
        let r = cron("0 9 * * 1-5", "Europe/Berlin");
        // Friday 2025-01-03 10:00 Berlin → Monday 2025-01-06 09:00 Berlin (08:00 UTC).
        let next = r.following(at("Europe/Berlin", 2025, 1, 3, 10, 0)).unwrap();
        assert_eq!(next, at("UTC", 2025, 1, 6, 8, 0));
        // Strictly after: a due time's follower is the next day.
        assert_eq!(r.following(next).unwrap(), at("UTC", 2025, 1, 7, 8, 0));
        // Names and the same time in another zone.
        let ny = cron("0 9 * * MON-FRI", "America/New_York");
        assert_eq!(
            ny.following(at("UTC", 2025, 1, 3, 23, 0)).unwrap(),
            at("UTC", 2025, 1, 6, 14, 0)
        );
    }

    #[test]
    fn daily_runs_keep_the_wall_clock_across_dst() {
        let r = cron("0 9 * * *", "Europe/Berlin");
        // Spring forward on 2025-03-30: 09:00 is 08:00 UTC before and 07:00 UTC after.
        let before = r.following(at("Europe/Berlin", 2025, 3, 29, 0, 0)).unwrap();
        let after = r.following(before).unwrap();
        assert_eq!(before, at("UTC", 2025, 3, 29, 8, 0));
        assert_eq!(after, at("UTC", 2025, 3, 30, 7, 0));
        assert_eq!(after.0 - before.0, 23 * HOUR);
        // Fall back on 2025-10-26: 25 hours apart.
        let before = r
            .following(at("Europe/Berlin", 2025, 10, 25, 0, 0))
            .unwrap();
        let after = r.following(before).unwrap();
        assert_eq!(after.0 - before.0, 25 * HOUR);
    }

    #[test]
    fn a_time_skipped_by_spring_forward_runs_right_after_the_gap() {
        // 02:30 doesn't exist in New York on 2025-03-09 (02:00 → 03:00).
        let r = cron("30 2 * * *", "America/New_York");
        let next = r
            .following(at("America/New_York", 2025, 3, 9, 0, 0))
            .unwrap();
        assert_eq!(next, at("America/New_York", 2025, 3, 9, 3, 0));
        // And the day after, at 02:30 again.
        assert_eq!(
            r.following(next).unwrap(),
            at("America/New_York", 2025, 3, 10, 2, 30)
        );
    }

    #[test]
    fn a_repeated_wall_clock_time_runs_once() {
        // 01:30 happens twice in New York on 2025-11-02 (02:00 EDT → 01:00 EST).
        let r = cron("30 1 * * *", "America/New_York");
        let first = r
            .following(at("America/New_York", 2025, 11, 2, 0, 0))
            .unwrap();
        assert_eq!(first, at("UTC", 2025, 11, 2, 5, 30)); // 01:30 EDT
        assert_eq!(
            r.following(first).unwrap(),
            at("America/New_York", 2025, 11, 3, 1, 30)
        );
    }

    #[test]
    fn intervals_keep_their_cadence_and_count_missed_runs() {
        let r = Recurrence::new(&ScheduleSpec::Interval { every_s: 7200 }, "UTC").unwrap();
        let t0 = UnixMillis(1_000);
        let first = r.first_after(t0).unwrap();
        assert_eq!(first.0, 1_000 + 2 * HOUR);
        assert_eq!(r.following(first).unwrap().0, 1_000 + 4 * HOUR);
        // From the first due time to 5 h after it: due at +0, +2 h, +4 h.
        let due = r.due_until(first, UnixMillis(first.0 + 5 * HOUR)).unwrap();
        assert_eq!(
            due,
            Due {
                count: 3,
                last: UnixMillis(first.0 + 4 * HOUR)
            }
        );
        assert_eq!(r.due_until(first, UnixMillis(first.0 - 1)), None);
        assert_eq!(r.due_until(first, first).unwrap().count, 1);
        // Interval specs are independent of the zone (but it must be valid).
        assert!(Recurrence::new(&ScheduleSpec::Interval { every_s: 60 }, "Asia/Tokyo").is_ok());
    }

    #[test]
    fn cron_due_times_are_counted_across_a_gap() {
        let r = cron("0 * * * *", "Europe/Berlin");
        let first = at("UTC", 2025, 3, 30, 0, 0);
        // 00:00–04:00 UTC on the spring-forward night: five hourly runs (UTC hours are all
        // there), the last at 04:00 UTC.
        let due = r.due_until(first, at("UTC", 2025, 3, 30, 4, 30)).unwrap();
        assert_eq!(due.count, 5);
        assert_eq!(due.last, at("UTC", 2025, 3, 30, 4, 0));
        // A long downtime counts up to the cap.
        let every_minute = cron("* * * * *", "UTC");
        let first = at("UTC", 2025, 1, 1, 0, 0);
        let due = every_minute
            .due_until(first, at("UTC", 2025, 2, 1, 0, 0))
            .unwrap();
        assert_eq!(due.count, MAX_COUNTED);
        assert_eq!(due.last, at("UTC", 2025, 2, 1, 0, 0));
    }

    #[test]
    fn invalid_specs_and_zones_are_refused() {
        let err = |spec: ScheduleSpec, tz: &str| Recurrence::new(&spec, tz).unwrap_err().0;
        let c = |e: &str| ScheduleSpec::Cron { expr: e.into() };
        assert!(err(c("0 9 * *"), "UTC").contains("five fields"));
        assert!(err(c("0 0 9 * * *"), "UTC").contains("five fields"));
        assert!(err(c("61 9 * * *"), "UTC").contains("invalid cron"));
        assert!(err(c("0 9 * * *"), "Mars/Olympus").contains("unknown time zone"));
        assert!(err(ScheduleSpec::Interval { every_s: 59 }, "UTC").contains("at least 60"));
        assert!(err(ScheduleSpec::Unknown, "UTC").contains("unknown"));
    }
}
