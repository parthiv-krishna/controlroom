//! The daemon API: controller ↔ `controlroomd`.
//!
//! **Deprecated** (remote-tools model): pi moves to the server, so this per-machine API splits
//! into the agent daemon API ([`crate::agentd`], sessions) and the host API ([`crate::host`],
//! tools). Its method table, `machine.subscribe` feed ([`MachineSnapshot`], [`MachineEvent`]),
//! cwd-based [`SessionCreateParams`] / [`SessionCreateResult`] and the mailbox ([`mailbox`
//! types](OutboxItem)) are removed once the controller and the daemon have switched. The
//! session-scoped command types, the `hello` types, [`FsListParams`], [`DoctorReport`] and
//! [`DaemonSettings`] are reused by the new APIs and move to them then.
//!
//! Newline-delimited JSON-RPC over an SSH exec channel running `controlroomd connect`. A daemon
//! accepts several concurrent clients, and a client going away never triggers anything.
//!
//! **Connection lifecycle:**
//! 1. The client calls `hello`. The daemon answers `incompatible_version` when the protocol
//!    majors differ, and `hello_required` to any other method sent first.
//! 2. The client subscribes with `machine.subscribe {since_rev}` and, per open session,
//!    `session.subscribe {id, since_seq}`. A subscribe's result is sent before any of its
//!    notifications. After a reconnect the client resubscribes with the last `rev` / `head_seq`
//!    it applied and receives exactly what it missed.
//! 3. Mutating commands carry a client-generated `op_id`; repeating one returns the original
//!    result instead of applying it twice.
//! 4. The controller additionally reads the outbox and delivers to inboxes (see
//!    [`OutboxItem`] and [`InboxDelivery`]).
//!
//! Methods that belong to an optional feature need the matching `hello` capability: `job.*`
//! (`jobs`), `todo.*` (`todos`), `outbox.*` and `inbox.deliver` (`mailbox`).

mod commands;
mod feeds;
mod handshake;
mod host;
mod mailbox;
mod runtime;

pub use commands::*;
pub use feeds::*;
pub use handshake::*;
pub use host::*;
pub use mailbox::*;
pub use runtime::*;

use crate::jsonrpc::{Ack, NoParams};
use crate::session::{Job, Todo};

/// Marker types for every daemon method; see [`crate::jsonrpc::Method`].
pub mod methods {
    use super::*;

    rpc_methods! {
        /// A typed daemon request: `{method, params}` for every daemon method. Servers decode
        /// with [`DaemonRequest::from_request`] and match exhaustively.
        pub enum DaemonRequest;

        /// Handshake; must be the first call.
        Hello = "hello" (HelloParams) -> HelloResult;
        /// Subscribe to every session's summary and the settings.
        MachineSubscribe = "machine.subscribe" (MachineSubscribeParams) -> MachineSubscribeResult;
        /// Subscribe to one session's timeline and detail.
        SessionSubscribe = "session.subscribe" (SessionSubscribeParams) -> SessionSubscribeResult;
        /// End a subscription.
        Unsubscribe = "unsubscribe" (UnsubscribeParams) -> Ack;

        /// Create a session and start its agent.
        SessionCreate = "session.create" (SessionCreateParams) -> SessionCreateResult;
        /// Send a prompt now, or queue it while a run is active.
        SessionPrompt = "session.prompt" (SessionPromptParams) -> PromptResult;
        /// Cancel the active run; pending blocking requests are withdrawn.
        SessionCancel = "session.cancel" (SessionOpParams) -> Ack;
        /// Close ("archive") a session: stop the agent, keep the history.
        SessionClose = "session.close" (SessionOpParams) -> Ack;
        /// Reopen a closed session as `dormant`; `deleted` if retention purged it.
        SessionReopen = "session.reopen" (SessionOpParams) -> Ack;
        /// Reattach a `dormant` or `interrupted` session's agent.
        SessionResume = "session.resume" (SessionResumeParams) -> Ack;
        /// Start a new agent session in the same controlroom session.
        SessionStartFresh = "session.start_fresh" (SessionStartFreshParams) -> StartFreshResult;
        /// Delete a session and its transcript now.
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

        /// Answer any request: permission, question, dialog, sandbox, contact or spawn.
        RequestRespond = "request.respond" (RequestRespondParams) -> RequestRespondResult;

        /// Add todo entries.
        TodoAdd = "todo.add" (TodoAddParams) -> TodoAddResult;
        /// Edit a todo entry.
        TodoUpdate = "todo.update" (TodoUpdateParams) -> Todo;
        /// Remove a todo entry.
        TodoRemove = "todo.remove" (TodoRemoveParams) -> Ack;
        /// Move a todo entry.
        TodoReorder = "todo.reorder" (TodoReorderParams) -> Ack;

        /// List a session's jobs.
        JobList = "job.list" (JobListParams) -> JobListResult;
        /// Read a range of a job's log.
        JobLogsMethod = "job.logs" (JobLogsParams) -> JobLogs;
        /// Kill a job.
        JobKill = "job.kill" (JobOpParams) -> Job;
        /// Wake the agent on a job's new or matching output.
        JobFollowMethod = "job.follow" (JobFollowParams) -> Job;
        /// Stop following a job (the exit wake stays on).
        JobUnfollow = "job.unfollow" (JobOpParams) -> Job;

        /// Remove a session's sandbox grant.
        SandboxRevoke = "sandbox.revoke" (SandboxRevokeParams) -> Ack;

        /// Configured agents with their dependency checks.
        AgentsList = "agents.list" (NoParams) -> AgentsListResult;
        /// List a directory.
        FsList = "fs.list" (FsListParams) -> FsListResult;
        /// Run the environment checks.
        DoctorReportMethod = "doctor.report" (NoParams) -> DoctorReport;
        /// Read the daemon settings.
        SettingsGet = "settings.get" (NoParams) -> DaemonSettings;
        /// Change the daemon settings.
        SettingsSet = "settings.set" (SettingsSetParams) -> DaemonSettings;

        /// Read the outbox (controller only).
        OutboxSubscribe = "outbox.subscribe" (OutboxSubscribeParams) -> OutboxSubscribeResult;
        /// Report an outbox item's outcome (controller only).
        OutboxAck = "outbox.ack" (OutboxAckParams) -> Ack;
        /// Deliver something to a session (controller only).
        InboxDeliver = "inbox.deliver" (InboxDeliverParams) -> Ack;
    }
}

/// Marker types for every daemon notification; see [`crate::jsonrpc::NotificationMethod`].
pub mod notifications {
    use super::*;

    rpc_notifications! {
        /// A typed daemon → client notification.
        pub enum DaemonNotification;

        /// Changes for a `machine.subscribe` subscription.
        MachineEvents = "machine.events" (MachineEventsParams);
        /// Timeline ops, raw frames and detail for a `session.subscribe` subscription.
        SessionEvents = "session.events" (SessionEventsParams);
        /// New items for an `outbox.subscribe` subscription.
        OutboxItems = "outbox.items" (OutboxItemsParams);
    }
}

pub use methods::DaemonRequest;
pub use notifications::DaemonNotification;
