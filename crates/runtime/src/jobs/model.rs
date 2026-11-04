//! A job as persisted with its session (table `jobs`), built from its host's job events.

use controlroom_protocol::host::KilledBy;
use controlroom_protocol::ids::{JobId, MachineId, RemotePath, Seq, UnixMillis, WorkspaceId};
use controlroom_protocol::session::{Job, JobFollow, JobStatus};
use serde::{Deserialize, Serialize};

/// Line ranges kept per job before new lines extend the last range.
const MAX_RANGES: usize = 1000;
/// Wake ids remembered per job.
const WAKES_KEPT: usize = 16;

/// The agent's follow subscription, as the host logged it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FollowSpec {
    pub buffer_seconds: u32,
    pub pattern: Option<String>,
}

/// Followed lines no wake has reported yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pending {
    /// How many lines.
    pub count: u64,
    /// Their ranges, ascending and merged.
    pub ranges: Vec<(u64, u64)>,
}

impl Pending {
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Adds a line number (in ascending order; older or repeated lines are ignored).
    pub(crate) fn add(&mut self, line: u64) {
        let full = self.ranges.len() >= MAX_RANGES;
        match self.ranges.last_mut() {
            Some((_, end)) if line <= *end => return,
            Some((_, end)) if line == *end + 1 || full => *end = line,
            _ => self.ranges.push((line, line)),
        }
        self.count += 1;
    }

    pub(crate) fn first(&self) -> Option<u64> {
        self.ranges.first().map(|r| r.0)
    }

    pub(crate) fn last(&self) -> Option<u64> {
        self.ranges.last().map(|r| r.1)
    }
}

/// How a job ended (its `exit` event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobEnd {
    pub status: JobStatus,
    pub exit: Option<i32>,
    pub signal: Option<String>,
    /// Host time.
    pub ended_at: UnixMillis,
    pub lines: u64,
    pub duration_ms: u64,
    /// Why the outcome is unknown (e.g. "host rebooted").
    pub lost: Option<String>,
}

/// A job of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobRecord {
    pub id: JobId,
    pub machine_id: MachineId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub command: String,
    pub cwd: RemotePath,
    pub log_path: RemotePath,
    pub timeout_s: Option<u32>,
    pub started_at: UnixMillis,
    /// The session's transcript head when it appeared: its timeline item sits there.
    pub seq: Seq,
    /// Output lines so far.
    pub lines: u64,
    pub follow: Option<FollowSpec>,
    /// The last line a wake reported: follow wakes report later lines only.
    pub notified_line: u64,
    pub pending: Pending,
    /// The host time of the first pending line: its follow wake is due `buffer_seconds` later.
    pub first_match_at: Option<UnixMillis>,
    pub kill: Option<KilledBy>,
    /// `None` while running.
    pub end: Option<JobEnd>,
    /// The exit wake was delivered (or isn't needed: the agent killed the job).
    pub exit_woken: bool,
    /// Ids of the wakes delivered (`exit:<machine>/<job>`, `follow:<machine>/<job>:<a>-<b>`),
    /// so a replayed event can never wake twice.
    pub wakes: Vec<String>,
}

impl JobRecord {
    pub(crate) fn status(&self) -> JobStatus {
        self.end.as_ref().map_or(JobStatus::Running, |e| e.status)
    }

    pub(crate) fn running(&self) -> bool {
        self.end.is_none()
    }

    /// When the buffered follow wake is due, if lines wait.
    pub(crate) fn follow_due_at(&self) -> Option<UnixMillis> {
        let follow = self.follow.as_ref()?;
        let first = self.first_match_at?;
        Some(UnixMillis(
            first.0 + i64::from(follow.buffer_seconds) * 1000,
        ))
    }

    /// Records a delivered wake.
    pub(crate) fn woke(&mut self, wake: String) {
        self.wakes.push(wake);
        let excess = self.wakes.len().saturating_sub(WAKES_KEPT);
        self.wakes.drain(..excess);
    }

    /// The wire view.
    pub(crate) fn to_job(&self) -> Job {
        let end = self.end.as_ref();
        Job {
            id: self.id,
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            machine_id: Some(self.machine_id),
            workspace_id: Some(self.workspace_id),
            status: self.status(),
            exit: end.and_then(|e| e.exit),
            signal: end.and_then(|e| e.signal.clone()),
            started_at: self.started_at,
            ended_at: end.map(|e| e.ended_at),
            timeout_s: self.timeout_s,
            log_path: self.log_path.clone(),
            lines: end.map_or(self.lines, |e| e.lines),
            follow: self.follow.as_ref().map(|f| JobFollow {
                buffer_seconds: f.buffer_seconds,
                pattern: f.pattern.clone(),
                next_wake_at: self.follow_due_at(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_lines_merge_into_ranges() {
        let mut pending = Pending::default();
        for line in [3, 4, 5, 5, 2, 9, 10] {
            pending.add(line);
        }
        assert_eq!(pending.ranges, [(3, 5), (9, 10)]);
        assert_eq!(pending.count, 5);
        assert_eq!((pending.first(), pending.last()), (Some(3), Some(10)));
    }
}
