//! Tool calls on a host: [`HostOp`] and [`HostResult`], run with `tool.run`, recovered with
//! `tool.attach`, interrupted with `tool.cancel`, and released with `journal.ack`.
//!
//! **Exactly once.** Every call carries a [`CallId`] and goes through the host's journal:
//!
//! - `read`, `read_bytes`, `stat`, `ls`, `find`, `grep` are idempotent: a resend within the
//!   journal's lifetime returns the journaled result, after a daemon restart they rerun;
//! - `write` is journaled and atomic (temp file, fsync, rename; the result is journaled before
//!   the reply). A resend returns the journaled result. If the daemon died mid-write, the file's
//!   hash tells: it holds the new content (the result) or not (`lost`, `may_have_run: false`);
//! - `bash` is at-most-once: the journal row is fsynced before the command reaches the shell. A
//!   resend attaches to the running command or returns its result, never reruns. If the daemon
//!   restarted, the shell's PTY supervisor still has the output (the result is recovered from
//!   its log); without it the call is `lost` (`may_have_run: true`);
//! - `job_*` are at-most-once by call id: a resend returns the same job, never a second one.
//!
//! The same [`HostOp`] travels the agent API (`host.call`, where the agent names the machine and
//! the shell by name) and the host API (`tool.run`, where the agent daemon has resolved the
//! machine to a workspace). Paths are [`RemotePath`]s: absolute, `~`-relative, or relative to the
//! workspace root.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::FollowFrom;
use crate::host::{FsEntry, FsEntryKind};
use crate::ids::{CallId, JobId, RemotePath, UnixMillis, WorkspaceId};
use crate::session::Job;

/// How a `write`'s `content` is encoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContentEncoding {
    /// UTF-8 text (the default).
    #[default]
    Utf8,
    /// Base64 bytes.
    Base64,
}

/// Who killed a job. A kill by the agent sends no exit wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum KilledBy {
    /// The agent (`job kill`).
    Agent,
    /// The user (`job.kill` from the UI).
    User,
    /// A value this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// A follow subscription given with `job_run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JobFollowSpec {
    /// Only lines matching this regex count; `null` for every line.
    pub pattern: Option<String>,
    /// The batching window, in seconds.
    pub buffer_seconds: u32,
}

/// One operation of a tool call, tagged by `op`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostOp {
    /// A window of a text file, with pi's limits (at most 2000 lines and 50 KB); for an image
    /// (png, jpeg, gif, webp, bmp) only its type and size.
    Read {
        /// The file.
        path: RemotePath,
        /// The first line, from 1; default 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u64>,
        /// At most this many lines.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// Raw bytes of a file, base64-encoded (at most 20 MiB per call). Used for images and as
    /// the read step of an edit.
    ReadBytes {
        /// The file.
        path: RemotePath,
        /// The first byte; default 0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u64>,
        /// At most this many bytes; default to the end (within the cap).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<u64>,
    },
    /// Whether a path exists, and what it is.
    Stat {
        /// The path.
        path: RemotePath,
    },
    /// Create or replace a file atomically, creating parent directories.
    Write {
        /// The file.
        path: RemotePath,
        /// The new content.
        content: String,
        /// How `content` is encoded; default `utf8`.
        #[serde(default)]
        #[ts(as = "Option<ContentEncoding>", optional)]
        encoding: ContentEncoding,
        /// Compare-and-swap: write only if the file's current SHA-256 (hex) is this, else
        /// `conflict`. Edits always set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_sha256: Option<String>,
    },
    /// A directory's entries, sorted by name (dot files included).
    Ls {
        /// The directory; default the workspace root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<RemotePath>,
        /// At most this many entries; default 500.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// Files whose path matches a glob, honoring ignore files.
    Find {
        /// The glob, e.g. `"*.rs"` or `"*.test.ts"`, matched against paths relative to
        /// `path`.
        pattern: String,
        /// Where to search; default the workspace root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<RemotePath>,
        /// At most this many paths; default 1000.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// Lines matching a regex (or a literal), honoring ignore files.
    Grep {
        /// The regex, or the literal text with `literal`.
        pattern: String,
        /// Where to search (a directory or a file); default the workspace root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<RemotePath>,
        /// Only files matching this glob.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob: Option<String>,
        /// Match case-insensitively. Default `false`.
        #[serde(default)]
        #[ts(as = "Option<bool>", optional)]
        ignore_case: bool,
        /// `pattern` is literal text, not a regex. Default `false`.
        #[serde(default)]
        #[ts(as = "Option<bool>", optional)]
        literal: bool,
        /// Lines of context around each match; default 0.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<u32>,
        /// At most this many matches; default 100.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// Run a command in a named shell (at most once). The shell keeps its state; the result is
    /// the command's output between its OSC 133 markers.
    Bash {
        /// The shell's name (in the call's session).
        shell: String,
        /// The command line.
        command: String,
        /// Interrupt it after this many seconds (enforced on the host, so it holds while the
        /// link is down); default 600.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_s: Option<u32>,
    },
    /// Start a background job in a detached job supervisor under the workspace's sandbox. Its
    /// `started` event is logged before the process exists.
    JobRun {
        /// The command line, run by the login shell.
        command: String,
        /// The working directory, absolute or relative to the workspace root; default the root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<RemotePath>,
        /// A short display name; default the command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Stop it after this many seconds (it ends `timed_out`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_s: Option<u32>,
        /// Follow it from its first line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow: Option<JobFollowSpec>,
    },
    /// Start or change a job's follow subscription; the host logs a `follow` event with the
    /// matches so far (for `from: unseen`).
    JobFollow {
        /// The job.
        job_id: JobId,
        /// Only lines matching this regex count; default every line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        /// The batching window; default 15.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        buffer_seconds: Option<u32>,
        /// Which output to cover; default `unseen`.
        #[serde(default)]
        #[ts(as = "Option<FollowFrom>", optional)]
        from: FollowFrom,
    },
    /// End a job's follow subscription (the exit wake stays).
    JobUnfollow {
        /// The job.
        job_id: JobId,
    },
    /// SIGTERM the job's process group, SIGKILL after 5 s.
    JobKill {
        /// The job.
        job_id: JobId,
        /// Who asked.
        by: KilledBy,
    },
    /// The jobs of the call's workspace, or one.
    JobStatus {
        /// One job; default every job of the workspace.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job_id: Option<JobId>,
    },
    /// An op this build doesn't know; answered with `unsupported`.
    #[serde(other)]
    Unknown,
}

/// Why a text window stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReadCut {
    /// At the line limit (2000, or the call's `limit`).
    Lines,
    /// At the byte limit (50 KB).
    Bytes,
    /// A reason this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// What a `read` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadContent {
    /// A window of a text file.
    Text {
        /// The lines, without newlines.
        lines: Vec<String>,
        /// The number of the first line, from 1.
        start_line: u64,
        /// Lines in the file, counted like pi (a trailing newline adds an empty last line).
        total_lines: u64,
        /// Why the window stopped before `limit` or the end of the file, if it did.
        cut: Option<ReadCut>,
        /// The first line alone exceeds the byte limit (`lines` is empty).
        first_line_too_long: bool,
    },
    /// An image; fetch it with `read_bytes`.
    Image {
        /// Its MIME type, e.g. `"image/png"`.
        mime_type: String,
    },
    /// A kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// One line of `grep` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct GrepLine {
    /// The file, relative to the searched path.
    pub path: String,
    /// The line number, from 1.
    pub line: u64,
    /// The line, cut at 500 characters.
    pub text: String,
    /// A context line rather than a match.
    pub context: bool,
}

/// The result of a tool call, tagged by `op` like its [`HostOp`]. Every resolved path is
/// absolute, so the extension can show it as `machine:path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostResult {
    /// See [`HostOp::Read`].
    Read {
        /// The file, resolved.
        path: RemotePath,
        /// Its size in bytes.
        size: u64,
        /// What was read.
        content: ReadContent,
    },
    /// See [`HostOp::ReadBytes`].
    ReadBytes {
        /// The file, resolved.
        path: RemotePath,
        /// The bytes, base64-encoded.
        data: String,
        /// How many bytes `data` holds.
        length: u64,
        /// The file's size in bytes.
        size: u64,
        /// SHA-256 (hex) of the whole file, when the call read all of it (an edit's
        /// `expect_sha256`); `null` for a partial read.
        sha256: Option<String>,
    },
    /// See [`HostOp::Stat`].
    Stat {
        /// The path, resolved.
        path: RemotePath,
        /// Whether it exists.
        exists: bool,
        /// What it is, when it exists.
        kind: Option<FsEntryKind>,
        /// Size in bytes, for files.
        size: Option<u64>,
        /// Last modification time.
        mtime: Option<UnixMillis>,
    },
    /// See [`HostOp::Write`].
    Write {
        /// The file, resolved.
        path: RemotePath,
        /// Bytes written.
        bytes: u64,
        /// SHA-256 (hex) of the new content.
        sha256: String,
    },
    /// See [`HostOp::Ls`].
    Ls {
        /// The directory, resolved.
        path: RemotePath,
        /// Its entries, sorted by name (at most `limit`).
        entries: Vec<FsEntry>,
        /// Entries in the directory.
        total: u64,
    },
    /// See [`HostOp::Find`].
    Find {
        /// The searched directory, resolved.
        path: RemotePath,
        /// Matching paths relative to it, sorted.
        paths: Vec<String>,
        /// More paths matched than `limit`.
        limit_reached: bool,
    },
    /// See [`HostOp::Grep`].
    Grep {
        /// The searched path, resolved.
        path: RemotePath,
        /// Matches and context lines, in file order.
        lines: Vec<GrepLine>,
        /// Matches returned.
        matches: u64,
        /// More lines matched than `limit`.
        limit_reached: bool,
        /// Some lines were cut at 500 characters.
        lines_truncated: bool,
    },
    /// See [`HostOp::Bash`]. A non-zero exit is a result, not an error.
    Bash {
        /// The exit status; `null` when it died from a signal.
        exit_code: Option<i32>,
        /// The signal name, if it died from one.
        signal: Option<String>,
        /// The shell's directory after the command (OSC 7).
        cwd: RemotePath,
        /// The output (stdout and stderr as the PTY saw them), ANSI-stripped, carriage returns
        /// collapsed; the last 2000 lines or 50 KB.
        output: String,
        /// `output` was cut; the whole output is in `log_path`.
        truncated: bool,
        /// The command's full output on the host.
        log_path: RemotePath,
        /// It ran into `timeout_s` and was interrupted.
        timed_out: bool,
        /// It was cancelled (`tool.cancel`).
        cancelled: bool,
        /// It waited for terminal input at some point.
        waited_for_input: bool,
        /// Why the shell was reopened before this command (its variables are lost), if it was.
        shell_reset: Option<String>,
        /// How long it ran.
        duration_ms: u64,
    },
    /// See [`HostOp::JobRun`].
    JobRun {
        /// The new job.
        job: Box<Job>,
    },
    /// See [`HostOp::JobFollow`].
    JobFollow {
        /// The job after the change.
        job: Box<Job>,
    },
    /// See [`HostOp::JobUnfollow`].
    JobUnfollow {
        /// The job after the change.
        job: Box<Job>,
    },
    /// See [`HostOp::JobKill`].
    JobKill {
        /// The job after the change (an ended job returns unchanged).
        job: Box<Job>,
    },
    /// See [`HostOp::JobStatus`].
    JobStatus {
        /// The jobs, newest first.
        jobs: Vec<Job>,
    },
    /// A result this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// Params of `tool.run`: run a call once. A repeat with the same `call_id` attaches to the
/// running call or returns its journaled result; it never runs again. `tool.output` and
/// `tool.waiting_input` for the call go to every connection waiting on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolRunParams {
    /// The call.
    pub call_id: CallId,
    /// The workspace it runs in, under that workspace's sandbox.
    pub workspace_id: WorkspaceId,
    /// What to do.
    pub op: HostOp,
}

/// Params of `tool.attach` and `tool.cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolCallRef {
    /// The call.
    pub call_id: CallId,
}

/// Result of `tool.cancel`: a running `bash` call gets ETX in its PTY (SIGINT to the foreground
/// group) and SIGKILL of that group after 5 s; its result follows with `cancelled: true` and the
/// partial output. Other ops are short and are not interrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolCancelResult {
    /// Something was interrupted; `false` when the call had ended or never ran.
    pub cancelled: bool,
}

/// Params of `journal.ack`: the agent daemon has logged these results, so the host may drop
/// them (otherwise they expire 24 h after the call ended). Unknown ids are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct JournalAckParams {
    /// The calls.
    pub call_ids: Vec<CallId>,
}

/// Params of the `tool.output` notification: new output of a running `bash` call,
/// ANSI-stripped, in order, coalesced (at most a few per second).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolOutputParams {
    /// The call.
    pub call_id: CallId,
    /// The new text.
    pub text: String,
}

/// Params of the `tool.waiting_input` notification: a `bash` call's foreground process started
/// (or stopped) waiting for terminal input, after 2 s without output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolWaitingInputParams {
    /// The call.
    pub call_id: CallId,
    /// Whether it is waiting now.
    pub waiting: bool,
}
