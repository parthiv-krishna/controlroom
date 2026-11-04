//! The session's shared todo list, auto-continue, and background jobs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{JobId, MachineId, RemotePath, TodoId, UnixMillis, WorkspaceId};
use crate::session::Author;

/// The status of a todo entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Finished.
    Done,
    /// Can't proceed; see [`Todo::reason`].
    Blocked,
    /// Won't be done; excluded from the roll-up total.
    Skipped,
    /// A status this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One entry of a session's todo list, edited by both the agent and the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Todo {
    /// The entry.
    pub id: TodoId,
    /// What to do.
    pub text: String,
    /// Its status.
    pub status: TodoStatus,
    /// Why it is blocked (or skipped).
    pub reason: Option<String>,
    /// Who made the last change.
    pub updated_by: Author,
    /// When.
    pub updated_at: UnixMillis,
}

/// Where auto-continue stands. It restarts the agent after a turn while open todo entries
/// remain, unless something else will wake it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AutoContinueStatus {
    /// Enabled and able to continue.
    On,
    /// Disabled for the session.
    Off,
    /// Paused with attention: only blocked entries remain, or the user cancelled a run.
    Paused,
    /// Paused with attention: three runs made no progress.
    Stalled,
    /// Paused with attention: the run budget is used up.
    Budget,
    /// A status this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Auto-continue details for the todo panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AutoContinueInfo {
    /// The status.
    pub status: AutoContinueStatus,
    /// Why it is paused, or why an `on` auto-continue isn't starting a run right now (e.g.
    /// "waiting on job tests"); `null` when nothing is in the way.
    pub reason: Option<String>,
    /// Auto-continue runs used so far.
    pub runs_used: u32,
    /// Auto-continue runs allowed before it pauses with `budget`.
    pub budget: u32,
}

/// The status of a background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Still running.
    Running,
    /// Exited with code 0.
    Succeeded,
    /// Exited with a non-zero code, or died with a signal.
    Failed,
    /// Killed by its timeout.
    TimedOut,
    /// Killed by the agent or the user.
    Killed,
    /// Its supervisor died with its host (a reboot) before it exited: the outcome is unknown.
    Lost,
    /// A status this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// An agent's follow subscription on a job's output. Matching lines are batched for
/// `buffer_seconds`, then wake the agent with their line ranges and the log path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobFollow {
    /// The batching window.
    pub buffer_seconds: u32,
    /// Only lines matching this regex wake the agent; `null` for every line.
    pub pattern: Option<String>,
    /// When the next buffered wake fires; `null` when nothing is buffered.
    pub next_wake_at: Option<UnixMillis>,
}

/// A background job: a supervised, sandboxed process whose output goes to a log file the agent
/// reads. The exit wake is always on; following is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Job {
    /// The job.
    pub id: JobId,
    /// A short display name.
    pub name: String,
    /// The command line.
    pub command: String,
    /// The working directory (on the job's machine).
    pub cwd: RemotePath,
    /// The machine it runs on; `null` from a host daemon (which doesn't know its machine id)
    /// and from the old per-machine daemon. The agent daemon always fills it.
    #[serde(default)]
    pub machine_id: Option<MachineId>,
    /// The workspace it runs in (under that workspace's sandbox); `null` from the old
    /// per-machine daemon.
    #[serde(default)]
    pub workspace_id: Option<WorkspaceId>,
    /// Its status.
    pub status: JobStatus,
    /// The exit code, once exited normally.
    pub exit: Option<i32>,
    /// The terminating signal name (e.g. `"SIGKILL"`), if it died from one.
    pub signal: Option<String>,
    /// When it started.
    pub started_at: UnixMillis,
    /// When it ended.
    pub ended_at: Option<UnixMillis>,
    /// Its timeout, if any.
    pub timeout_s: Option<u32>,
    /// The plain-text log, on the job's machine: `<workspace root>/.controlroom/jobs/<id>.log`
    /// (the old daemon: `<session cwd>/.controlroom/jobs/<id>.log`).
    pub log_path: RemotePath,
    /// Lines in the log so far.
    pub lines: u64,
    /// The agent's follow subscription, if any.
    pub follow: Option<JobFollow>,
}
