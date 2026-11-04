//! The next UI API: browser ↔ controller with **global sessions**.
//!
//! It replaces [`crate::ui`]'s own method table once the UI switches over, and is then promoted
//! to `ui` (its TypeScript lives in `web/src/lib/protocol/next/` until then). The differences:
//!
//! - session commands pass through to the agent daemon unchanged (no `machine_id`);
//!   `session.create` takes workspaces; `session.workspace_add|remove` are new;
//! - the state feed carries the agent daemon's sessions (with workspaces, per-workspace
//!   availability and shells; never stale) with their project membership, plus `overlaps[]`;
//! - projects are named, colored groups of tasks (no location prefixes); a session belongs to
//!   the projects of its linked tasks, else "Unassigned";
//! - tasks, contact grants, schedules and focus reports name sessions by id alone;
//! - `fs.list` goes to a machine's host daemon; `doctor.report` to a host or the server;
//!   `agents.list` and `settings.*` to the agent daemon.
//!
//! Everything else (machines, keys, devices, push, sandbox defaults) is unchanged.

mod projects;
mod schedules;
mod sessions;
mod state;

pub use projects::*;
pub use schedules::*;
pub use sessions::*;
pub use state::*;

use crate::agentd::{SessionCreateResult, WorkspaceAddParams, WorkspaceRemoveParams};
use crate::daemon::*;
use crate::jsonrpc::{Ack, NoParams};
use crate::session::{Job, Todo, Workspace};
use crate::ui::{
    AdditionalKey, ChecklistAddParams, ChecklistRemoveParams, ChecklistUpdateParams, Device,
    DeviceRefParams, DevicesRegisterParams, DevicesRenameParams, GrantsRevokeParams,
    KeysDeleteParams, KeysGenerateParams, KeysImportParams, KeysListResult, KeysRotateResult,
    Machine, MachineRef, MachinesAddConfirmParams, MachinesAddConfirmResult, MachinesAddResult,
    MachinesConfirmHostKeyParams, MachinesSetSettingsParams, NewMachine, NotificationPreferences,
    NotificationsSetPreferencesParams, NotificationsSubscribeParams, OnMachine, ProjectRef,
    SandboxProfilesResult, SandboxSetProfileParams, ScheduleRef, SchedulesUpdateParams,
    StateSubscribeParams, TaskRef, TasksMoveParams, TasksUpdateParams, UiHelloParams,
    UiHelloResult,
};

/// Marker types for every next UI method; see [`crate::jsonrpc::Method`].
pub mod methods {
    use super::*;

    rpc_methods! {
        /// A typed request of the next UI API.
        pub enum UiRequest;

        /// Handshake; must be the first call.
        Hello = "hello" (UiHelloParams) -> UiHelloResult;
        /// Heartbeat: answers at once, so a client can tell a live connection from a dead one.
        Ping = "ping" (NoParams) -> Ack;
        /// Subscribe to the aggregated state feed.
        StateSubscribe = "state.subscribe" (StateSubscribeParams) -> StateSubscribeResult;
        /// Subscribe to one session's timeline and detail.
        SessionSubscribe = "session.subscribe" (SessionSubscribeParams) -> SessionSubscribeResult;
        /// End a subscription.
        Unsubscribe = "unsubscribe" (UnsubscribeParams) -> Ack;

        /// Step one of adding a machine: fetch its host key.
        MachinesAdd = "machines.add" (NewMachine) -> MachinesAddResult;
        /// Step two: save it with the confirmed host key and bootstrap it.
        MachinesAddConfirm = "machines.add_confirm" (MachinesAddConfirmParams) -> MachinesAddConfirmResult;
        /// Pin a machine's (new) host key and reconnect.
        MachinesConfirmHostKey = "machines.confirm_host_key" (MachinesConfirmHostKeyParams) -> Machine;
        /// Remove a machine record (sessions keep their other workspaces).
        MachinesRemove = "machines.remove" (MachineRef) -> Ack;
        /// Reconnect now.
        MachinesReconnect = "machines.reconnect" (MachineRef) -> Ack;
        /// Upgrade the host daemon now.
        MachinesUpgrade = "machines.upgrade" (MachineRef) -> Ack;
        /// Change the controller's machine settings.
        MachinesSetSettings = "machines.set_settings" (MachinesSetSettingsParams) -> Machine;

        /// The controlroom key and additional keys.
        KeysList = "keys.list" (NoParams) -> KeysListResult;
        /// Rotate the controlroom key on every machine.
        KeysRotate = "keys.rotate" (NoParams) -> KeysRotateResult;
        /// Generate an additional key.
        KeysGenerate = "keys.generate" (KeysGenerateParams) -> AdditionalKey;
        /// Import an additional key.
        KeysImport = "keys.import" (KeysImportParams) -> AdditionalKey;
        /// Delete an additional key.
        KeysDelete = "keys.delete" (KeysDeleteParams) -> Ack;

        /// Create a project.
        ProjectsCreate = "projects.create" (ProjectsCreateParams) -> Project;
        /// Rename or recolor a project.
        ProjectsUpdate = "projects.update" (ProjectsUpdateParams) -> Project;
        /// Delete a project (its tasks stay, unassigned from it).
        ProjectsDelete = "projects.delete" (ProjectRef) -> Ack;

        /// Create a task.
        TasksCreate = "tasks.create" (TasksCreateParams) -> Task;
        /// Change a task's fields.
        TasksUpdate = "tasks.update" (TasksUpdateParams) -> Task;
        /// Move a task to a column and position.
        TasksMove = "tasks.move" (TasksMoveParams) -> Task;
        /// Delete a task.
        TasksDelete = "tasks.delete" (TaskRef) -> Ack;
        /// Link a session to a task.
        TasksLink = "tasks.link" (TasksLinkParams) -> Task;
        /// Unlink a session from a task.
        TasksUnlink = "tasks.unlink" (TasksLinkParams) -> Task;
        /// Add a checklist entry.
        TasksChecklistAdd = "tasks.checklist_add" (ChecklistAddParams) -> Task;
        /// Edit or tick a checklist entry.
        TasksChecklistUpdate = "tasks.checklist_update" (ChecklistUpdateParams) -> Task;
        /// Remove a checklist entry.
        TasksChecklistRemove = "tasks.checklist_remove" (ChecklistRemoveParams) -> Task;

        /// Revoke a contact grant.
        GrantsRevoke = "grants.revoke" (GrantsRevokeParams) -> Ack;

        /// Register this browser (idempotent; the server's label wins).
        DevicesRegister = "devices.register" (DevicesRegisterParams) -> Device;
        /// Rename a device.
        DevicesRename = "devices.rename" (DevicesRenameParams) -> Device;
        /// Remove a device and sign it out everywhere.
        DevicesRemove = "devices.remove" (DeviceRefParams) -> Ack;

        /// Which events push.
        NotificationsPreferences = "notifications.preferences" (NoParams) -> NotificationPreferences;
        /// Change which events push.
        NotificationsSetPreferences = "notifications.set_preferences" (NotificationsSetPreferencesParams) -> NotificationPreferences;
        /// Attach a Web Push subscription to a device.
        NotificationsSubscribe = "notifications.subscribe" (NotificationsSubscribeParams) -> Device;
        /// Drop a device's push subscription.
        NotificationsUnsubscribe = "notifications.unsubscribe" (DeviceRefParams) -> Device;

        /// Create a schedule.
        SchedulesCreate = "schedules.create" (SchedulesCreateParams) -> Schedule;
        /// Change a schedule.
        SchedulesUpdate = "schedules.update" (SchedulesUpdateParams) -> Schedule;
        /// Delete a schedule.
        SchedulesDelete = "schedules.delete" (ScheduleRef) -> Ack;
        /// Fire a schedule now.
        SchedulesRunNow = "schedules.run_now" (ScheduleRef) -> Schedule;

        /// The sandbox defaults of every machine and project.
        SandboxProfiles = "sandbox.profiles" (NoParams) -> SandboxProfilesResult;
        /// Edit a machine's or project's sandbox defaults.
        SandboxSetProfile = "sandbox.set_profile" (SandboxSetProfileParams) -> SandboxProfilesResult;

        /// Report what this client shows, for push deduplication.
        ClientFocus = "client.focus" (ClientFocusParams) -> Ack;

        /// Create a session on its workspaces with its first prompt, and link it to tasks.
        SessionCreate = "session.create" (UiSessionCreateParams) -> SessionCreateResult;
        /// Pass-through: give a session another workspace.
        SessionWorkspaceAdd = "session.workspace_add" (WorkspaceAddParams) -> Workspace;
        /// Pass-through: drop a session's workspace.
        SessionWorkspaceRemove = "session.workspace_remove" (WorkspaceRemoveParams) -> Ack;
        /// Pass-through: send or queue a prompt.
        SessionPrompt = "session.prompt" (SessionPromptParams) -> PromptResult;
        /// Pass-through: cancel the active run.
        SessionCancel = "session.cancel" (SessionOpParams) -> Ack;
        /// Pass-through: close a session.
        SessionClose = "session.close" (SessionOpParams) -> Ack;
        /// Pass-through: reopen a closed session.
        SessionReopen = "session.reopen" (SessionOpParams) -> Ack;
        /// Pass-through: reattach a session's agent.
        SessionResume = "session.resume" (SessionResumeParams) -> Ack;
        /// Pass-through: start a new agent session.
        SessionStartFresh = "session.start_fresh" (SessionStartFreshParams) -> StartFreshResult;
        /// Pass-through: delete a session.
        SessionDelete = "session.delete" (SessionOpParams) -> Ack;
        /// Pass-through: rename a session or change its settings.
        SessionUpdate = "session.update" (SessionUpdateParams) -> Ack;
        /// Pass-through: move the read cursor forward.
        SessionMarkRead = "session.mark_read" (SessionMarkReadParams) -> ReadCursor;
        /// Pass-through: mark the last run unread.
        SessionMarkUnread = "session.mark_unread" (SessionOpParams) -> ReadCursor;
        /// Pass-through: change an agent option.
        SessionSetOption = "session.set_option" (SessionSetOptionParams) -> Ack;
        /// Pass-through: send a queued item now.
        QueueSendNow = "queue.send_now" (QueueSendNowParams) -> SendNowResult;
        /// Pass-through: edit a queued item.
        QueueEdit = "queue.edit" (QueueEditParams) -> Ack;
        /// Pass-through: remove a queued item.
        QueueRemove = "queue.remove" (QueueRemoveParams) -> Ack;
        /// Pass-through: move a queued item.
        QueueReorder = "queue.reorder" (QueueReorderParams) -> Ack;
        /// Pass-through: restart a paused queue.
        QueueResume = "queue.resume" (SessionOpParams) -> Ack;
        /// Pass-through: answer a request.
        RequestRespond = "request.respond" (RequestRespondParams) -> RequestRespondResult;
        /// Pass-through: add todo entries.
        TodoAdd = "todo.add" (TodoAddParams) -> TodoAddResult;
        /// Pass-through: edit a todo entry.
        TodoUpdate = "todo.update" (TodoUpdateParams) -> Todo;
        /// Pass-through: remove a todo entry.
        TodoRemove = "todo.remove" (TodoRemoveParams) -> Ack;
        /// Pass-through: move a todo entry.
        TodoReorder = "todo.reorder" (TodoReorderParams) -> Ack;
        /// Pass-through: list a session's jobs.
        JobList = "job.list" (JobListParams) -> JobListResult;
        /// Pass-through: read a job's log.
        JobLogsMethod = "job.logs" (JobLogsParams) -> JobLogs;
        /// Pass-through: kill a job.
        JobKill = "job.kill" (JobOpParams) -> Job;
        /// Pass-through: follow a job's output.
        JobFollowMethod = "job.follow" (JobFollowParams) -> Job;
        /// Pass-through: stop following a job.
        JobUnfollow = "job.unfollow" (JobOpParams) -> Job;
        /// Pass-through: remove a session's sandbox grant.
        SandboxRevoke = "sandbox.revoke" (SandboxRevokeParams) -> Ack;
        /// Pass-through: the agents the server can run.
        AgentsList = "agents.list" (NoParams) -> AgentsListResult;
        /// Pass-through: the agent daemon's settings.
        SettingsGet = "settings.get" (NoParams) -> DaemonSettings;
        /// Pass-through: change the agent daemon's settings.
        SettingsSet = "settings.set" (SettingsSetParams) -> DaemonSettings;
        /// List a directory on a machine (its host daemon, with the user's own access).
        FsList = "fs.list" (OnMachine<FsListParams>) -> FsListResult;
        /// Re-run a machine's or the server's doctor checks.
        DoctorReportMethod = "doctor.report" (DoctorReportParams) -> DoctorReport;
    }
}

/// Marker types for every next UI notification; see [`crate::jsonrpc::NotificationMethod`].
pub mod notifications {
    use super::*;

    rpc_notifications! {
        /// A typed controller → UI notification of the next UI API.
        pub enum UiNotification;

        /// Changes for a `state.subscribe` subscription.
        StateEvents = "state.events" (StateEventsParams);
        /// Timeline ops, raw frames and detail for a `session.subscribe` subscription.
        SessionEvents = "session.events" (SessionEventsParams);
    }
}

pub use methods::UiRequest;
pub use notifications::UiNotification;
