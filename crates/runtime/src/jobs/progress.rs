//! A job's output as the daemon follows it, in memory: the line count, the lines that match
//! the follow pattern, the exit, and when the buffered follow wake is due. Rebuilt after a
//! restart by reading the job's transcript again.
//!
//! A pipe job's transcript is its `meta` record, then one `out`/`stderr` record per output line,
//! then its `exit` record, so line `n` of the log is the record with seq `n + 1`.

use std::collections::BTreeMap;

use controlroom_protocol::ids::UnixMillis;
use controlroom_supervisor::plain::plain_line;
use controlroom_transcript::{Body, ExitInfo, Record};
use regex::Regex;

/// The seq of output line `line` (from 1) in a job transcript (see the module docs).
pub(crate) fn line_seq(line: u64) -> u64 {
    line + 1
}

/// Sets of line numbers, kept as merged inclusive ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LineRanges {
    /// start → end (inclusive), disjoint and not adjacent.
    ranges: BTreeMap<u64, u64>,
}

impl LineRanges {
    pub(crate) fn insert(&mut self, line: u64) {
        let before = self
            .ranges
            .range(..=line)
            .next_back()
            .map(|(s, e)| (*s, *e));
        if before.is_some_and(|(_, end)| end >= line) {
            return;
        }
        let start = match before {
            Some((start, end)) if end + 1 == line => start,
            _ => line,
        };
        let end = match self.ranges.remove(&(line + 1)) {
            Some(end) => end,
            None => line,
        };
        self.ranges.insert(start, end);
    }

    /// Drops every line up to `line`.
    pub(crate) fn drop_through(&mut self, line: u64) {
        let kept = self.ranges.split_off(&(line + 1));
        let cut = self
            .ranges
            .iter()
            .next_back()
            .filter(|(_, end)| **end > line)
            .map(|(_, end)| (line + 1, *end));
        self.ranges = kept;
        if let Some((start, end)) = cut {
            self.ranges.insert(start, end);
        }
    }

    pub(crate) fn ranges(&self) -> Vec<(u64, u64)> {
        self.ranges.iter().map(|(s, e)| (*s, *e)).collect()
    }
}

/// What following a job wakes on.
#[derive(Debug, Clone)]
pub(crate) enum Matcher {
    /// Every new line.
    Any,
    /// Lines whose plain text matches.
    Pattern(Regex),
}

/// The lines a follow wake reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// How many lines.
    pub count: u64,
    /// Their ranges, in order.
    pub ranges: Vec<(u64, u64)>,
}

/// The exit record, once folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exited {
    pub info: ExitInfo,
    pub at: UnixMillis,
}

/// A running job's progress.
#[derive(Debug, Clone, Default)]
pub(crate) struct Progress {
    /// Output lines folded.
    pub lines: u64,
    /// The last record folded.
    pub seq: u64,
    pub exited: Option<Exited>,
    /// The follow subscription's matcher, while following.
    matcher: Option<Matcher>,
    /// Matching lines after the follow baseline (pattern follows only).
    matches: LineRanges,
    /// When the buffered follow wake fires.
    pub due_at: Option<UnixMillis>,
    /// The line count the session detail shows; refreshed at most once a tick, so a chatty job
    /// doesn't flood the feeds.
    pub shown_lines: u64,
}

/// What a folded record changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Folded {
    /// Nothing the follow cares about (or an old record).
    Nothing,
    /// A new line the follow wakes on.
    Followed,
    /// The job exited.
    Exited,
}

impl Progress {
    /// Folds the next record. Lines after `baseline` count for the follow subscription.
    pub(crate) fn fold(&mut self, record: &Record, baseline: u64) -> Folded {
        if record.seq <= self.seq || self.exited.is_some() {
            return Folded::Nothing;
        }
        self.seq = record.seq;
        match &record.body {
            Body::Out(line) | Body::Stderr(line) => {
                self.lines += 1;
                let line_no = self.lines;
                if line_seq(line_no) != record.seq {
                    tracing::warn!("job transcript: line {line_no} is at seq {}", record.seq);
                }
                if line_no > baseline && self.wakes_on(&line.raw) {
                    if matches!(self.matcher, Some(Matcher::Pattern(_))) {
                        self.matches.insert(line_no);
                    }
                    return Folded::Followed;
                }
                Folded::Nothing
            }
            Body::Exit(info) => {
                self.exited = Some(Exited {
                    info: *info,
                    at: UnixMillis(i64::try_from(record.ts).unwrap_or(i64::MAX)),
                });
                Folded::Exited
            }
            Body::In(_) | Body::Meta(_) | Body::Unknown(_) => Folded::Nothing,
        }
    }

    fn wakes_on(&self, raw: &str) -> bool {
        match &self.matcher {
            None => false,
            Some(Matcher::Any) => true,
            Some(Matcher::Pattern(regex)) => regex.is_match(&plain_line(raw)),
        }
    }

    /// Starts (or changes) following. Lines already folded are matched by
    /// [`Progress::add_matches`] from a rescan.
    pub(crate) fn follow(&mut self, matcher: Matcher) {
        self.matcher = Some(matcher);
        self.matches = LineRanges::default();
        self.due_at = None;
    }

    pub(crate) fn unfollow(&mut self) {
        self.matcher = None;
        self.matches = LineRanges::default();
        self.due_at = None;
    }

    /// Records matches a rescan found among already folded lines.
    pub(crate) fn add_matches(&mut self, lines: &[u64]) {
        for line in lines {
            self.matches.insert(*line);
        }
    }

    /// The followed lines after `baseline` that no wake has reported yet.
    pub(crate) fn pending(&mut self, baseline: u64) -> Option<Pending> {
        let ranges = match &self.matcher {
            None => return None,
            Some(Matcher::Any) => {
                (self.lines > baseline).then(|| vec![(baseline + 1, self.lines)])?
            }
            Some(Matcher::Pattern(_)) => {
                self.matches.drop_through(baseline);
                self.matches.ranges()
            }
        };
        let count = ranges.iter().map(|(s, e)| e - s + 1).sum();
        (count > 0).then_some(Pending { count, ranges })
    }
}

#[cfg(test)]
mod tests {
    use controlroom_transcript::Line;

    use super::*;

    fn out(seq: u64, text: &str) -> Record {
        Record {
            seq,
            ts: 0,
            body: Body::Out(Line::new(text)),
        }
    }

    #[test]
    fn ranges_merge_and_drop() {
        let mut r = LineRanges::default();
        for line in [5, 3, 4, 9, 7, 8, 1, 4] {
            r.insert(line);
        }
        assert_eq!(r.ranges(), [(1, 1), (3, 5), (7, 9)]);
        r.drop_through(4);
        assert_eq!(r.ranges(), [(5, 5), (7, 9)]);
        r.drop_through(10);
        assert_eq!(r.ranges(), []);
    }

    #[test]
    fn a_pattern_follow_reports_matching_lines_after_the_baseline() {
        let mut p = Progress::default();
        p.follow(Matcher::Pattern(Regex::new("FAIL|panicked").unwrap()));
        let lines = [
            "ok a",
            "\u{1b}[31mFAIL\u{1b}[0m b",
            "ok c",
            "FAIL d",
            "FAIL e",
        ];
        for (i, text) in lines.iter().enumerate() {
            let seq = i as u64 + 2;
            let expected = if text.contains("FAIL") {
                Folded::Followed
            } else {
                Folded::Nothing
            };
            assert_eq!(p.fold(&out(seq, text), 0), expected, "{text}");
        }
        assert_eq!(p.lines, 5);
        let pending = p.pending(0).unwrap();
        assert_eq!((pending.count, pending.ranges), (3, vec![(2, 2), (4, 5)]));
        assert_eq!(p.pending(5), None);
        // Old records are ignored.
        assert_eq!(p.fold(&out(3, "FAIL again"), 0), Folded::Nothing);
    }

    #[test]
    fn a_plain_follow_reports_every_new_line_and_exit_ends_folding() {
        let mut p = Progress::default();
        assert_eq!(p.fold(&out(2, "before"), 0), Folded::Nothing);
        p.follow(Matcher::Any);
        assert_eq!(p.fold(&out(3, "x"), 1), Folded::Followed);
        assert_eq!(p.pending(1).unwrap().ranges, [(2, 2)]);
        let exit = Record {
            seq: 4,
            ts: 7,
            body: Body::Exit(ExitInfo {
                code: Some(0),
                signal: None,
                cause: None,
            }),
        };
        assert_eq!(p.fold(&exit, 1), Folded::Exited);
        assert_eq!(p.exited.unwrap().at, UnixMillis(7));
        assert_eq!(p.fold(&out(5, "late"), 1), Folded::Nothing);
        p.unfollow();
        assert_eq!(p.pending(0), None);
    }
}
