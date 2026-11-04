//! The host API: controller ↔ `controlroomd` in the host role, on every execution machine.
//!
//! Newline-delimited JSON-RPC over an SSH exec channel running `controlroomd connect`, as the
//! daemon API before it. A host keeps no sessions: it knows **workspaces** (one session's
//! directory with its own sandbox profile), **shells** (PTY supervisors in a workspace),
//! **jobs**, and a **journal** of tool calls keyed by [`crate::ids::CallId`]. pi, its
//! extensions and model credentials never run here.
//!
//! The controller only routes: the agent daemon decides every request (it reaches the host
//! through the controller's `hostops` bridge, see [`crate::agentd`]), and the controller
//! forwards every host notification back to it.
//!
//! **Connection lifecycle:**
//! 1. `hello` first (`hello_required` otherwise). Same single-controller rule as the daemon
//!    API ([`crate::daemon::ClientRole`]). The result's `epoch` changes when the daemon restarts.
//! 2. After every `hello` the agent daemon (through the controller) subscribes again:
//!    `jobs.subscribe {since: its cursor}` and `approvals.subscribe`.
//! 3. Every method is idempotent (workspaces and shells by id, tool calls by call id, acks and
//!    answers by their subject), so a request whose connection dropped before its response is
//!    simply sent again on the next connection.
//!
//! Bulk bytes never ride this channel: terminals use `controlroomd pty attach <shell id>`
//! channels and the human file browser uses SFTP; `read_bytes` is capped.
//!
//! Methods that belong to an optional feature need the matching `hello` capability: `shell.*`
//! and `bash` (`shells`), `job_*` ops and `jobs.*` (`jobs`), `approvals.subscribe` and
//! `approval.respond` (`sandbox_approvals`).

mod approvals;
mod handshake;
mod jobs;
mod tool;
mod workspace;

pub use approvals::*;
pub use handshake::*;
pub use jobs::*;
pub use tool::*;
pub use workspace::*;

use crate::daemon::{DoctorReport, FsListParams, FsListResult, HelloParams, UnsubscribeParams};
use crate::jsonrpc::{Ack, NoParams};

/// Marker types for every host method; see [`crate::jsonrpc::Method`].
pub mod methods {
    use super::*;

    rpc_methods! {
        /// A typed host request: `{method, params}` for every host method.
        pub enum HostRequest;

        /// Handshake; must be the first call.
        Hello = "hello" (HelloParams) -> HostHelloResult;
        /// Resolve a workspace's directory and write its sandbox profile.
        WorkspaceOpen = "workspace.open" (WorkspaceOpenParams) -> WorkspaceOpenResult;
        /// Stop a workspace's shells and jobs and forget it.
        WorkspaceClose = "workspace.close" (WorkspaceCloseParams) -> Ack;
        /// Start (or return) a named shell in a workspace.
        ShellOpen = "shell.open" (ShellOpenParams) -> HostShell;
        /// End a shell.
        ShellClose = "shell.close" (ShellCloseParams) -> Ack;
        /// The host's shells.
        ShellList = "shell.list" (ShellListParams) -> ShellListResult;
        /// Run a tool call once (a repeat attaches or returns the journaled result).
        ToolRun = "tool.run" (ToolRunParams) -> HostResult;
        /// Wait for an earlier call's result (`not_found` `call` if it never started).
        ToolAttach = "tool.attach" (ToolCallRef) -> HostResult;
        /// Interrupt a running call.
        ToolCancel = "tool.cancel" (ToolCallRef) -> ToolCancelResult;
        /// Release journaled results the agent daemon has logged.
        JournalAck = "journal.ack" (JournalAckParams) -> Ack;
        /// Read the job event log from a cursor, then follow it.
        JobsSubscribe = "jobs.subscribe" (JobsSubscribeParams) -> JobsSubscribeResult;
        /// Report the consumed revision of the job event log.
        JobsAck = "jobs.ack" (JobsAckParams) -> Ack;
        /// Pending sandbox approvals, then new ones (controller only).
        ApprovalsSubscribe = "approvals.subscribe" (NoParams) -> ApprovalsSubscribeResult;
        /// Answer a sandbox approval.
        ApprovalRespond = "approval.respond" (ApprovalRespondParams) -> Ack;
        /// End a subscription.
        Unsubscribe = "unsubscribe" (UnsubscribeParams) -> Ack;
        /// List a directory with the host user's own access (new-session dialog, file browser).
        FsList = "fs.list" (FsListParams) -> FsListResult;
        /// Run the environment checks (nono, Landlock, persistence, boot start).
        DoctorReportMethod = "doctor.report" (NoParams) -> DoctorReport;
    }
}

/// Marker types for every host notification; see [`crate::jsonrpc::NotificationMethod`].
pub mod notifications {
    use super::*;

    rpc_notifications! {
        /// A typed host → controller notification.
        pub enum HostNotification;

        /// New output of a running `bash` call.
        ToolOutput = "tool.output" (ToolOutputParams);
        /// A `bash` call started or stopped waiting for terminal input.
        ToolWaitingInput = "tool.waiting_input" (ToolWaitingInputParams);
        /// A shell changed (cwd, state, foreground process), opened or closed.
        ShellChanged = "shell.changed" (ShellChangedParams);
        /// New job events for a `jobs.subscribe` subscription.
        JobsEvent = "jobs.event" (JobsEventParams);
        /// A new sandbox approval for an `approvals.subscribe` subscription.
        ApprovalRequest = "approval.request" (ApprovalRequestParams);
        /// A sandbox approval expired or was withdrawn.
        ApprovalClosed = "approval.closed" (ApprovalClosedParams);
    }
}

pub use methods::HostRequest;
pub use notifications::HostNotification;
