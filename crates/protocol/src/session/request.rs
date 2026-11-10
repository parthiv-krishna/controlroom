//! Requests: approvals (blocking), questions (non-blocking) and agent dialogs, rendered by one
//! request card.
//!
//! Every request carries a generic `title`, `detail` and `options`, so a client can show and
//! answer even a kind or body it doesn't know.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{MachineId, RemotePath, RequestId, SessionId, UnixMillis};
use crate::session::{DeviceRef, PeerRef};

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// The agent wants to run a tool (blocking).
    Permission,
    /// A question from `ask_user` (always non-blocking; the answer is injected).
    Question,
    /// One of the agent's own blocking dialogs (select, confirm or input).
    Dialog,
    /// A sandbox approval: a command, a path or an endpoint (blocking).
    Sandbox,
    /// Contact with a session outside the policy (blocking; raised by the controller).
    Contact,
    /// Spawning a subagent on another machine (blocking; raised by the controller).
    Spawn,
    /// An agent command in a shell waits for terminal input: the user types it in the
    /// terminal (blocking; the agent can't type). Withdrawn when the command moves on.
    TerminalInput,
    /// `bash_privileged`: run a command unsandboxed on a host (blocking, every time).
    Privileged,
    /// A request kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The lifecycle of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// Waiting for an answer.
    Pending,
    /// Answered (a dismissed question counts as answered "declined").
    Answered,
    /// Withdrawn by the agent, or made moot (e.g. its run was cancelled).
    Withdrawn,
    /// Timed out unanswered (a timed-out approval is a denial).
    Expired,
    /// A state this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// How a request option is styled on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OptionStyle {
    /// Allows once.
    Allow,
    /// Allows more than this once (always, this session, this pair).
    AllowAlways,
    /// Denies.
    Deny,
    /// Neither (dialog choices).
    Neutral,
    /// A style this build doesn't know; render as neutral.
    #[serde(other)]
    Unknown,
}

/// One answer option of an approval or a select or confirm dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RequestOption {
    /// Sent back in [`Response::Choice`].
    pub id: String,
    /// Button label, e.g. `"Allow once"`.
    pub label: String,
    /// Styling hint.
    pub style: OptionStyle,
}

/// One choice of a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionOption {
    /// The label, also the value sent back in [`QuestionAnswer::selected`].
    pub label: String,
    /// Optional explanation.
    pub description: Option<String>,
}

/// One question of a question request or an input dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Question {
    /// Identifies the question within its request.
    pub id: String,
    /// The question text (Markdown).
    pub text: String,
    /// Suggested choices; empty for a free-text question.
    pub options: Vec<QuestionOption>,
    /// Whether several choices may be selected.
    pub multi: bool,
    /// Whether a free-text answer is allowed.
    pub free_text: bool,
}

/// Access asked for or granted on a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PathAccess {
    /// Read only.
    Read,
    /// Write only.
    Write,
    /// Read and write.
    ReadWrite,
    /// An access mode this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The structured subject of a request, for a richer card. Optional: `title` and `detail` are
/// always enough to decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestBody {
    /// Run a command.
    Command {
        /// The exact command line.
        command: String,
        /// Where it runs, when known.
        cwd: Option<RemotePath>,
    },
    /// Access a path outside the sandbox.
    Path {
        /// The path.
        path: RemotePath,
        /// The access asked for.
        access: PathAccess,
        /// Whether the path is under the workspace root (filled by the host).
        #[serde(default)]
        in_workspace: Option<bool>,
        /// The machine whose sandbox asked (filled by the agent daemon): "Always allow" and
        /// "Never" change this machine's sandbox defaults.
        #[serde(default)]
        machine_id: Option<MachineId>,
        /// That machine's display name.
        #[serde(default)]
        machine: Option<String>,
    },
    /// Call an HTTP endpoint through the sandbox proxy.
    Endpoint {
        /// The HTTP method.
        method: String,
        /// The URL.
        url: String,
    },
    /// Change a file.
    Diff {
        /// The file.
        path: RemotePath,
        /// The old content (empty for a new file).
        old: String,
        /// The new content.
        new: String,
    },
    /// A mediated `git push`, approved each time.
    GitPush {
        /// The full command line.
        command: String,
        /// The remote.
        remote: String,
        /// The branch or refspec.
        branch: String,
        /// Whether it force-pushes.
        force: bool,
    },
    /// Contact with a session outside the policy (the requester is this session).
    Contact {
        /// The session to contact.
        peer: PeerRef,
        /// The agent's stated reason.
        reason: Option<String>,
    },
    /// A subagent on another machine.
    Spawn {
        /// The target machine's display name.
        machine: String,
        /// The agent key.
        agent: String,
        /// The child's working directory.
        cwd: RemotePath,
        /// The child's title.
        title: String,
    },
    /// A command waiting for terminal input.
    TerminalInput {
        /// The machine's display name.
        machine: String,
        /// The shell's name.
        shell: String,
        /// The agent's command.
        command: String,
    },
    /// A command to run unsandboxed (`bash_privileged`).
    Privileged {
        /// The machine's display name.
        machine: String,
        /// The exact command line.
        command: String,
        /// The agent's stated reason.
        reason: String,
        /// Where it runs, when given.
        cwd: Option<RemotePath>,
    },
    /// A body kind this build doesn't know; use the request's title and detail.
    #[serde(other)]
    Unknown,
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuestionAnswer {
    /// The [`Question::id`].
    pub question_id: String,
    /// Labels of the selected options.
    #[serde(default)]
    pub selected: Vec<String>,
    /// The free-text answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// How a request is answered (`request.respond`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Pick one of [`Request::options`] (approvals, select and confirm dialogs).
    Choice {
        /// The [`RequestOption::id`].
        option_id: String,
        /// An optional reason, e.g. passed back to the agent with a denial.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Answer [`Request::questions`] (questions and input dialogs).
    Answers {
        /// One answer per question.
        answers: Vec<QuestionAnswer>,
    },
    /// Dismiss a question: the agent is told "declined".
    Dismiss,
    /// A response kind this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The recorded answer of a resolved request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RequestAnswer {
    /// What was answered.
    pub response: Response,
    /// A short human summary, e.g. `"Allowed once"` or the answer text.
    pub label: String,
    /// The deciding device; `null` when not decided by the user (e.g. expired).
    pub by: Option<DeviceRef>,
    /// When it was answered.
    pub at: UnixMillis,
}

/// A request in a session: pending ones in the summary, all of them as timeline items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Request {
    /// Unique within the session.
    pub id: RequestId,
    /// What it asks for.
    pub kind: RequestKind,
    /// Whether the agent is blocked until it is answered. `false` for questions (the lighter
    /// attention tier), `true` for approvals and dialogs.
    pub blocking: bool,
    /// Lifecycle state.
    pub state: RequestState,
    /// One-line summary, e.g. `"Run git push origin main?"`.
    pub title: String,
    /// Longer explanation (Markdown).
    pub detail: Option<String>,
    /// Structured subject, when there is one.
    pub body: Option<RequestBody>,
    /// Answer options (approvals and dialogs).
    pub options: Vec<RequestOption>,
    /// Questions (question requests and input dialogs).
    pub questions: Vec<Question>,
    /// The answer, once resolved.
    pub answer: Option<RequestAnswer>,
    /// Set when the request belongs to a child session and is rolled up to its parent; answer
    /// it on the child session.
    pub origin_child: Option<SessionId>,
    /// When it was raised.
    pub created_at: UnixMillis,
}
