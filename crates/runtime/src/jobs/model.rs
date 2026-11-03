//! A job as persisted with its session (table `jobs`).

use controlroom_protocol::ids::{JobId, RemotePath, Seq, UnixMillis};
use controlroom_protocol::session::{Job, JobFollow, JobStatus};
use controlroom_transcript::{ExitInfo, StopCause};
use serde::{Deserialize, Serialize};

use super::progress::Progress;

/// Who asked to kill a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KilledBy {
    /// The agent (`job kill`): it knows, so no exit wake.
    Agent,
    /// The user (`job.kill` from the UI): the exit wake tells the agent.
    User,
}

/// The agent's follow subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FollowSpec {
    pub buffer_seconds: u32,
    pub pattern: Option<String>,
    /// Lines up to here are not reported (`from: now` at follow time).
    pub since_line: u64,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobEnd {
    pub status: JobStatus,
    pub exit: Option<i32>,
    pub signal: Option<String>,
    pub ended_at: UnixMillis,
    /// Output lines in total.
    pub lines: u64,
    /// Why it ended without an exit status (it never started, or its supervisor vanished).
    pub error: Option<String>,
}

impl JobEnd {
    /// The end an exit record describes.
    pub(crate) fn from_exit(info: &ExitInfo, at: UnixMillis, lines: u64) -> JobEnd {
        let status = match (info.cause, info.code) {
            (Some(StopCause::Timeout), _) => JobStatus::TimedOut,
            (Some(StopCause::Terminate), _) => JobStatus::Killed,
            (_, Some(0)) => JobStatus::Succeeded,
            _ => JobStatus::Failed,
        };
        JobEnd {
            status,
            exit: info.code,
            signal: info.signal.map(signal_name),
            ended_at: at,
            lines,
            error: None,
        }
    }
}

/// A job of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobRecord {
    pub id: JobId,
    pub name: String,
    pub command: String,
    pub cwd: RemotePath,
    pub log_path: RemotePath,
    pub timeout_s: Option<u32>,
    pub started_at: UnixMillis,
    /// The session's transcript head when it started: its timeline item sits there.
    pub seq: Seq,
    pub follow: Option<FollowSpec>,
    /// Output lines a wake has covered: a follow with `from: unseen` starts after them.
    pub notified_line: u64,
    pub kill: Option<KilledBy>,
    /// `None` while running.
    pub end: Option<JobEnd>,
    /// The exit wake was delivered (or isn't needed).
    pub exit_woken: bool,
}

impl JobRecord {
    pub(crate) fn running(&self) -> bool {
        self.end.is_none()
    }

    /// The first line a follow wake may report is after this one.
    pub(crate) fn follow_baseline(&self) -> u64 {
        let since = self.follow.as_ref().map_or(0, |f| f.since_line);
        self.notified_line.max(since)
    }

    /// The wire view; `progress` is the live progress of a running job.
    pub(crate) fn to_job(&self, progress: Option<&Progress>) -> Job {
        let end = self.end.as_ref();
        Job {
            id: self.id,
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            status: end.map_or(JobStatus::Running, |e| e.status),
            exit: end.and_then(|e| e.exit),
            signal: end.and_then(|e| e.signal.clone()),
            started_at: self.started_at,
            ended_at: end.map(|e| e.ended_at),
            timeout_s: self.timeout_s,
            log_path: self.log_path.clone(),
            lines: end.map_or_else(|| progress.map_or(0, |p| p.shown_lines), |e| e.lines),
            follow: self.follow.as_ref().map(|f| JobFollow {
                buffer_seconds: f.buffer_seconds,
                pattern: f.pattern.clone(),
                next_wake_at: progress.and_then(|p| p.due_at),
            }),
        }
    }
}

/// `SIGKILL` for 9, and so on; `signal N` for the rest.
pub(crate) fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return format!("signal {signal}"),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_records_map_to_statuses() {
        let end = |code, signal, cause| {
            JobEnd::from_exit(
                &ExitInfo {
                    code,
                    signal,
                    cause,
                },
                UnixMillis(1),
                3,
            )
        };
        assert_eq!(end(Some(0), None, None).status, JobStatus::Succeeded);
        assert_eq!(end(Some(2), None, None).status, JobStatus::Failed);
        let killed = end(None, Some(15), Some(StopCause::Terminate));
        assert_eq!(
            (killed.status, killed.signal.as_deref()),
            (JobStatus::Killed, Some("SIGTERM"))
        );
        let timed_out = end(None, Some(9), Some(StopCause::Timeout));
        assert_eq!(timed_out.status, JobStatus::TimedOut);
        assert_eq!(end(None, Some(11), None).status, JobStatus::Failed);
        assert_eq!(signal_name(64), "signal 64");
    }
}
