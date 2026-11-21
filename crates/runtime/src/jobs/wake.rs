//! The text of job wakes. A wake carries only metadata and the log path, never log content: the
//! agent reads what it needs from the file (spec §8). How to read a log is said once, in the
//! `job` tool's description, so a wake is one line per job:
//!
//! ```text
//! [controlroom] Job 01J… "build" exited with code 1 after 3m 05s; 1234 lines in devbox:/…/01J….log; followed lines not reported yet: 81, 115, 186-189.
//! ```
//!
//! A wake for several jobs (merged in one buffer window) lists one job per line under
//! `[controlroom] Jobs:`.

use controlroom_protocol::ids::{JobId, RemotePath};
use controlroom_protocol::session::JobStatus;

use super::model::{JobEnd, Pending};

/// Line ranges listed per job before the rest is summarized.
const MAX_RANGES: usize = 8;

/// What one job contributes to a wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeEntry {
    pub job: JobId,
    pub name: String,
    pub log_path: RemotePath,
    /// The machine's display name (the log is `machine:path` for the agent).
    pub machine: String,
    /// Output lines so far.
    pub lines: u64,
    pub kind: WakeKind,
}

/// Why a job wakes the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WakeKind {
    /// It ended; `pending` are followed lines no follow wake reported yet.
    Exit {
        end: JobEnd,
        duration_ms: u64,
        pending: Option<Pending>,
    },
    /// Followed lines appeared while it runs.
    Follow {
        pending: Pending,
        pattern: Option<String>,
    },
}

/// One wake, possibly merging several jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wake {
    /// The run trigger's label, e.g. `"job: tests (exit 0)"`.
    pub label: String,
    /// The message to the agent.
    pub text: String,
}

/// Composes the wake for `entries` (at least one).
pub(crate) fn compose(entries: &[WakeEntry]) -> Wake {
    let labels: Vec<String> = entries
        .iter()
        .map(|e| format!("{} ({})", e.name, short_outcome(&e.kind)))
        .collect();
    let (label, text) = match (labels.as_slice(), entries) {
        ([one], [entry]) => (
            format!("job: {one}"),
            format!("[controlroom] Job {}", describe(entry)),
        ),
        (many, _) => {
            let lines: Vec<String> = entries
                .iter()
                .map(|e| format!("- job {}", describe(e)))
                .collect();
            (
                format!("jobs: {}", many.join(", ")),
                format!("[controlroom] Jobs:\n{}", lines.join("\n")),
            )
        }
    };
    Wake { label, text }
}

fn short_outcome(kind: &WakeKind) -> String {
    match kind {
        WakeKind::Exit { end, .. } => match (end.status, end.exit, &end.signal) {
            (JobStatus::Lost, _, _) => "lost".into(),
            (JobStatus::TimedOut, _, _) => "timed out".into(),
            (JobStatus::Killed, _, _) => "killed".into(),
            (_, Some(code), _) => format!("exit {code}"),
            (_, None, Some(signal)) => signal.clone(),
            _ => "failed".into(),
        },
        WakeKind::Follow { .. } => "follow".into(),
    }
}

/// `<id> "<name>" <what happened>; <n> lines in <machine>:<path>[; …].`
fn describe(entry: &WakeEntry) -> String {
    let job = format!("{} \"{}\"", entry.job, entry.name);
    let log = format!("{}:{}", entry.machine, entry.log_path);
    match &entry.kind {
        WakeKind::Exit {
            end,
            duration_ms,
            pending,
        } => {
            let how = match (end.status, end.exit, &end.signal, &end.lost) {
                (JobStatus::Lost, .., lost) => format!(
                    "was lost ({}), outcome unknown,",
                    lost.as_deref().unwrap_or("its supervisor is gone")
                ),
                (JobStatus::TimedOut, ..) => "timed out and was stopped".to_owned(),
                (JobStatus::Killed, ..) => "was killed by the user".to_owned(),
                (_, Some(code), ..) => format!("exited with code {code}"),
                (_, None, Some(signal), _) => format!("was killed by {signal}"),
                _ => "ended".to_owned(),
            };
            let unreported = pending
                .as_ref()
                .map(|p| format!("; followed lines not reported yet: {}", ranges(p)))
                .unwrap_or_default();
            format!(
                "{job} {how} after {}; {} in {log}{unreported}.",
                duration(*duration_ms),
                plural(entry.lines, "line")
            )
        }
        WakeKind::Follow { pending, pattern } => {
            let what = match pattern {
                Some(pattern) => format!("{} matching /{pattern}/", plural(pending.count, "line")),
                None => format!("{} new", plural(pending.count, "line")),
            };
            format!(
                "{job} is running; {what} at {}; {} so far in {log}.",
                ranges(pending),
                plural(entry.lines, "line")
            )
        }
    }
}

/// `3-5, 9` (at most [`MAX_RANGES`] ranges).
fn ranges(pending: &Pending) -> String {
    let mut parts: Vec<String> = pending
        .ranges
        .iter()
        .take(MAX_RANGES)
        .map(|(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            }
        })
        .collect();
    if pending.ranges.len() > MAX_RANGES {
        parts.push(format!(
            "and {} more ranges",
            pending.ranges.len() - MAX_RANGES
        ));
    }
    parts.join(", ")
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// `45s`, `3m 05s`, `2h 01m`.
fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::ids::UnixMillis;

    use super::*;

    fn end(status: JobStatus, exit: Option<i32>) -> JobEnd {
        JobEnd {
            status,
            exit,
            signal: None,
            ended_at: UnixMillis(0),
            lines: 0,
            duration_ms: 0,
            lost: None,
        }
    }

    fn entry(name: &str, kind: WakeKind) -> WakeEntry {
        WakeEntry {
            job: "01J00000000000000000000000".parse().unwrap(),
            name: name.into(),
            log_path: RemotePath::new(format!("/home/alice/app/.controlroom/jobs/{name}.log")),
            machine: "devbox".into(),
            lines: 1234,
            kind,
        }
    }

    #[test]
    fn an_exit_wake_is_one_line_with_the_outcome_duration_lines_and_path() {
        let wake = compose(&[entry(
            "build",
            WakeKind::Exit {
                end: end(JobStatus::Succeeded, Some(0)),
                duration_ms: 185_000,
                pending: None,
            },
        )]);
        assert_eq!(wake.label, "job: build (exit 0)");
        assert_eq!(
            wake.text,
            "[controlroom] Job 01J00000000000000000000000 \"build\" exited with code 0 after \
             3m 05s; 1234 lines in devbox:/home/alice/app/.controlroom/jobs/build.log."
        );
    }

    #[test]
    fn an_exit_wake_lists_the_followed_lines_not_reported_yet() {
        let mut pending = Pending::default();
        for line in [81, 115, 186, 188, 189, 216, 218] {
            pending.add(line);
        }
        let wake = compose(&[entry(
            "validate",
            WakeKind::Exit {
                end: end(JobStatus::Failed, Some(1)),
                duration_ms: 101_000,
                pending: Some(pending),
            },
        )]);
        assert_eq!(wake.label, "job: validate (exit 1)");
        assert!(
            wake.text.ends_with(
                "exited with code 1 after 1m 41s; 1234 lines in \
                 devbox:/home/alice/app/.controlroom/jobs/validate.log; followed lines not \
                 reported yet: 81, 115, 186, 188-189, 216, 218."
            ),
            "{}",
            wake.text
        );
        assert!(!wake.text.contains('\n'), "{}", wake.text);
    }

    #[test]
    fn merged_wakes_list_every_job() {
        let pending = Pending {
            count: 13,
            ranges: (1..=10).map(|n| (n * 10, n * 10 + 1)).collect(),
        };
        let wake = compose(&[
            entry(
                "e2e",
                WakeKind::Follow {
                    pending,
                    pattern: Some("FAIL".into()),
                },
            ),
            entry(
                "tests",
                WakeKind::Exit {
                    end: end(JobStatus::TimedOut, None),
                    duration_ms: 7_300_000,
                    pending: None,
                },
            ),
        ]);
        assert_eq!(wake.label, "jobs: e2e (follow), tests (timed out)");
        let lines: Vec<&str> = wake.text.lines().collect();
        assert_eq!(lines[0], "[controlroom] Jobs:");
        assert!(
            lines[1].starts_with(
                "- job 01J00000000000000000000000 \"e2e\" is running; 13 lines matching /FAIL/ \
                 at 10-11, 20-21"
            ),
            "{}",
            lines[1]
        );
        assert!(lines[1].contains("80-81, and 2 more ranges; 1234 lines so far in devbox:"));
        assert!(lines[2].contains("timed out and was stopped after 2h 01m"));
    }
}
