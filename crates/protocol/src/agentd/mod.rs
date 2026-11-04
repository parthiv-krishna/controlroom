//! The agent daemon API: controller ↔ `controlroom-agentd`, over the local unix socket
//! `/run/controlroom-agent/agentd.sock`.
//!
//! The agent daemon runs every pi session on the server. It is the old daemon API generalized:
//! sessions are global (they name workspaces instead of a cwd, and nothing session-scoped
//! carries a machine), the mailbox is local (no outbox or inbox), and hosts are reached through
//! the controller's `hostops` bridge (see [`hostops`](self::HostOpsRequestParams)), so the agent
//! daemon never calls the controller.
//!
//! **Connection lifecycle:**
//! 1. `hello` first (`hello_required` otherwise), with the daemon API's params and result and
//!    its single-controller rule ([`crate::daemon::ClientRole`]; `controlroom-agentd ctl` uses
//!    the debug role).
//! 2. The controller subscribes with `sessions.subscribe {since_rev}`, `hostops.subscribe`,
//!    and `session.subscribe {id, since_seq}` per open session, and reports its machines with
//!    `hostops.machines`. After a reconnect it resubscribes with the last `rev` / `head_seq`.
//! 3. Mutating commands carry an `op_id`; repeating one returns the original result.
//!
//! The session-scoped command types are the daemon API's ([`crate::daemon`]): they move here
//! when the daemon API is deleted.

mod create;
mod hostops;
mod sessions;

pub use create::*;
pub use hostops::*;
pub use sessions::*;

use crate::daemon::{
    AgentsListResult, DaemonSettings, DoctorReport, HelloParams, HelloResult, JobFollowParams,
    JobListParams, JobListResult, JobLogs, JobLogsParams, JobOpParams, PromptResult,
    QueueEditParams, QueueRemoveParams, QueueReorderParams, QueueSendNowParams, ReadCursor,
    RequestRespondParams, RequestRespondResult, SandboxRevokeParams, SendNowResult,
    SessionEventsParams, SessionMarkReadParams, SessionOpParams, SessionPromptParams,
    SessionResumeParams, SessionSetOptionParams, SessionStartFreshParams, SessionSubscribeParams,
    SessionSubscribeResult, SessionUpdateParams, SettingsSetParams, StartFreshResult,
    TodoAddParams, TodoAddResult, TodoRemoveParams, TodoReorderParams, TodoUpdateParams,
    UnsubscribeParams,
};
use crate::jsonrpc::{Ack, NoParams};
use crate::session::{Job, Todo, Workspace};

/// Marker types for every agent daemon method; see [`crate::jsonrpc::Method`].
pub mod methods {
    use super::*;

    rpc_methods! {
        /// A typed agent daemon request: `{method, params}` for every method.
        pub enum AgentdRequest;

        /// Handshake; must be the first call.
        Hello = "hello" (HelloParams) -> HelloResult;
        /// Subscribe to every session's summary and the settings.
        SessionsSubscribe = "sessions.subscribe" (SessionsSubscribeParams) -> SessionsSubscribeResult;
        /// Subscribe to one session's timeline and detail.
        SessionSubscribe = "session.subscribe" (SessionSubscribeParams) -> SessionSubscribeResult;
        /// End a subscription.
        Unsubscribe = "unsubscribe" (UnsubscribeParams) -> Ack;

        /// Create a session on its workspaces and start its agent.
        SessionCreate = "session.create" (SessionCreateParams) -> SessionCreateResult;
        /// Give a session another workspace.
        SessionWorkspaceAdd = "session.workspace_add" (WorkspaceAddParams) -> Workspace;
        /// Drop a session's workspace.
        SessionWorkspaceRemove = "session.workspace_remove" (WorkspaceRemoveParams) -> Ack;
        /// Send a prompt now, or queue it while a run is active.
        SessionPrompt = "session.prompt" (SessionPromptParams) -> PromptResult;
        /// Cancel the active run; pending blocking requests are withdrawn, running tool calls
        /// interrupted.
        SessionCancel = "session.cancel" (SessionOpParams) -> Ack;
        /// Close ("archive") a session: stop the agent and its shells, keep the history.
        SessionClose = "session.close" (SessionOpParams) -> Ack;
        /// Reopen a closed session as `dormant`; `deleted` if retention purged it.
        SessionReopen = "session.reopen" (SessionOpParams) -> Ack;
        /// Reattach a `dormant` or `interrupted` session's agent.
        SessionResume = "session.resume" (SessionResumeParams) -> Ack;
        /// Start a new agent session in the same controlroom session.
        SessionStartFresh = "session.start_fresh" (SessionStartFreshParams) -> StartFreshResult;
        /// Delete a session, its transcript and its workspaces' state on the hosts.
        SessionDelete = "session.delete" (SessionOpParams) -> Ack;
        /// Rename a session or change its queue and auto-continue settings.
        SessionUpdate = "session.update" (SessionUpdateParams) -> Ack;
        /// Move the read cursor forward.
        SessionMarkRead = "session.mark_read" (SessionMarkReadParams) -> ReadCursor;
        /// Move the read cursor back to just before the last run's end.
        SessionMarkUnread = "session.mark_unread" (SessionOpParams) -> ReadCursor;
        /// Change an agent option (model, thinking level, ...).
        SessionSetOption = "session.set_option" (SessionSetOptionParams) -> Ack;

        /// Deliver a queued item ahead of everything else.
        QueueSendNow = "queue.send_now" (QueueSendNowParams) -> SendNowResult;
        /// Replace a queued item's content.
        QueueEdit = "queue.edit" (QueueEditParams) -> Ack;
        /// Remove a queued item.
        QueueRemove = "queue.remove" (QueueRemoveParams) -> Ack;
        /// Move a queued item.
        QueueReorder = "queue.reorder" (QueueReorderParams) -> Ack;
        /// Restart a paused queue.
        QueueResume = "queue.resume" (SessionOpParams) -> Ack;

        /// Answer any request, including host sandbox approvals and waits for terminal input.
        RequestRespond = "request.respond" (RequestRespondParams) -> RequestRespondResult;

        /// Add todo entries.
        TodoAdd = "todo.add" (TodoAddParams) -> TodoAddResult;
        /// Edit a todo entry.
        TodoUpdate = "todo.update" (TodoUpdateParams) -> Todo;
        /// Remove a todo entry.
        TodoRemove = "todo.remove" (TodoRemoveParams) -> Ack;
        /// Move a todo entry.
        TodoReorder = "todo.reorder" (TodoReorderParams) -> Ack;

        /// List a session's jobs (on every workspace's host).
        JobList = "job.list" (JobListParams) -> JobListResult;
        /// Read a range of a job's log (from its host).
        JobLogsMethod = "job.logs" (JobLogsParams) -> JobLogs;
        /// Kill a job.
        JobKill = "job.kill" (JobOpParams) -> Job;
        /// Wake the agent on a job's new or matching output.
        JobFollowMethod = "job.follow" (JobFollowParams) -> Job;
        /// Stop following a job (the exit wake stays on).
        JobUnfollow = "job.unfollow" (JobOpParams) -> Job;

        /// Remove a session's sandbox grant.
        SandboxRevoke = "sandbox.revoke" (SandboxRevokeParams) -> Ack;

        /// The agents the server can run, with their dependency checks.
        AgentsList = "agents.list" (NoParams) -> AgentsListResult;
        /// Run the server's environment checks (nono, Landlock, pi).
        DoctorReportMethod = "doctor.report" (NoParams) -> DoctorReport;
        /// Read the settings.
        SettingsGet = "settings.get" (NoParams) -> DaemonSettings;
        /// Change the settings.
        SettingsSet = "settings.set" (SettingsSetParams) -> DaemonSettings;

        /// Start carrying host requests (controller only).
        HostOpsSubscribe = "hostops.subscribe" (NoParams) -> HostOpsSubscribeResult;
        /// Answer a host request (controller only).
        HostOpsRespond = "hostops.respond" (HostOpsRespondParams) -> Ack;
        /// Forward a host daemon's notification (controller only).
        HostOpsNotify = "hostops.notify" (HostOpsNotifyParams) -> Ack;
        /// Report the controller's machines (controller only).
        HostOpsMachines = "hostops.machines" (HostOpsMachinesParams) -> Ack;
    }
}

/// Marker types for every agent daemon notification; see
/// [`crate::jsonrpc::NotificationMethod`].
pub mod notifications {
    use super::*;

    rpc_notifications! {
        /// A typed agent daemon → controller notification.
        pub enum AgentdNotification;

        /// Changes for a `sessions.subscribe` subscription.
        SessionsEvents = "sessions.events" (SessionsEventsParams);
        /// Timeline ops, raw frames and detail for a `session.subscribe` subscription.
        SessionEvents = "session.events" (SessionEventsParams);
        /// New host requests for a `hostops.subscribe` subscription.
        HostOpsRequest = "hostops.request" (HostOpsRequestParams);
        /// Host requests no longer needed.
        HostOpsWithdraw = "hostops.withdraw" (HostOpsWithdrawParams);
    }
}

pub use methods::AgentdRequest;
pub use notifications::AgentdNotification;
