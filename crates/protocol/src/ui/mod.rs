//! The UI API: browser ↔ controller, JSON-RPC over one WebSocket per tab.
//!
//! The API mirrors the daemon's session commands with a `machine_id` added ([`OnMachine`]),
//! aggregates every machine into one state feed (`state.subscribe`), and adds the controller's
//! own data: machines, keys, projects, tasks, contact grants, devices, schedules, sandbox
//! defaults and push.
//!
//! **Connection lifecycle:** `hello` (with the device id), `devices.register`, then
//! `state.subscribe {since}` and `session.subscribe {machine_id, id, since_seq}` per open
//! session. Session subscriptions are shared by the server across tabs and resumed without
//! duplicates. `op_id`s pass through to the daemon unchanged; a pass-through command for an
//! unreachable machine fails with `machine_unavailable` and is never queued by the server.

mod hello;
mod keys;
mod machines;
mod people;
mod projects;
mod sandbox;
mod schedules;
mod state;

pub use hello::*;
pub use keys::*;
pub use machines::*;
pub use people::*;
pub use projects::*;
pub use sandbox::*;
pub use schedules::*;
pub use state::*;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::*;
use crate::ids::MachineId;
use crate::jsonrpc::{Ack, NoParams};
use crate::session::{Job, Todo};

/// Adds the target machine to a daemon command's params: on the wire, `machine_id` sits next to
/// the daemon params' own fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct OnMachine<P> {
    /// The machine whose daemon handles the command.
    pub machine_id: MachineId,
    /// The daemon command's params.
    #[serde(flatten)]
    pub params: P,
}

impl<P> OnMachine<P> {
    /// Wraps daemon params for `machine_id`.
    pub fn new(machine_id: MachineId, params: P) -> Self {
        OnMachine { machine_id, params }
    }
}

/// Marker types for every UI method; see [`crate::jsonrpc::Method`].
pub mod methods {
    use super::*;

    rpc_methods! {
        /// A typed UI request: `{method, params}` for every UI method.
        pub enum UiRequest;

        /// Handshake; must be the first call.
        Hello = "hello" (UiHelloParams) -> UiHelloResult;
        /// Subscribe to the aggregated state feed.
        StateSubscribe = "state.subscribe" (StateSubscribeParams) -> StateSubscribeResult;
        /// Subscribe to one session's timeline and detail.
        SessionSubscribe = "session.subscribe" (OnMachine<SessionSubscribeParams>) -> SessionSubscribeResult;
        /// End a subscription.
        Unsubscribe = "unsubscribe" (UnsubscribeParams) -> Ack;

        /// Step one of adding a machine: fetch its host key.
        MachinesAdd = "machines.add" (NewMachine) -> MachinesAddResult;
        /// Step two: save it with the confirmed host key and bootstrap it.
        MachinesAddConfirm = "machines.add_confirm" (MachinesAddConfirmParams) -> MachinesAddConfirmResult;
        /// Pin a machine's (new) host key and reconnect.
        MachinesConfirmHostKey = "machines.confirm_host_key" (MachinesConfirmHostKeyParams) -> Machine;
        /// Remove a machine record (its sessions keep running).
        MachinesRemove = "machines.remove" (MachineRef) -> Ack;
        /// Reconnect now.
        MachinesReconnect = "machines.reconnect" (MachineRef) -> Ack;
        /// Upgrade the daemon now.
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
        /// Change a project.
        ProjectsUpdate = "projects.update" (ProjectsUpdateParams) -> Project;
        /// Delete a project.
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

        /// Pass-through: create a session.
        SessionCreate = "session.create" (OnMachine<SessionCreateParams>) -> SessionCreateResult;
        /// Pass-through: send or queue a prompt.
        SessionPrompt = "session.prompt" (OnMachine<SessionPromptParams>) -> PromptResult;
        /// Pass-through: cancel the active run.
        SessionCancel = "session.cancel" (OnMachine<SessionOpParams>) -> Ack;
        /// Pass-through: close a session.
        SessionClose = "session.close" (OnMachine<SessionOpParams>) -> Ack;
        /// Pass-through: reopen a closed session.
        SessionReopen = "session.reopen" (OnMachine<SessionOpParams>) -> Ack;
        /// Pass-through: reattach a session's agent.
        SessionResume = "session.resume" (OnMachine<SessionResumeParams>) -> Ack;
        /// Pass-through: start a new agent session.
        SessionStartFresh = "session.start_fresh" (OnMachine<SessionStartFreshParams>) -> StartFreshResult;
        /// Pass-through: delete a session.
        SessionDelete = "session.delete" (OnMachine<SessionOpParams>) -> Ack;
        /// Pass-through: rename a session or change its settings.
        SessionUpdate = "session.update" (OnMachine<SessionUpdateParams>) -> Ack;
        /// Pass-through: move the read cursor forward.
        SessionMarkRead = "session.mark_read" (OnMachine<SessionMarkReadParams>) -> ReadCursor;
        /// Pass-through: mark the last run unread.
        SessionMarkUnread = "session.mark_unread" (OnMachine<SessionOpParams>) -> ReadCursor;
        /// Pass-through: change an agent option.
        SessionSetOption = "session.set_option" (OnMachine<SessionSetOptionParams>) -> Ack;
        /// Pass-through: send a queued item now.
        QueueSendNow = "queue.send_now" (OnMachine<QueueSendNowParams>) -> SendNowResult;
        /// Pass-through: edit a queued item.
        QueueEdit = "queue.edit" (OnMachine<QueueEditParams>) -> Ack;
        /// Pass-through: remove a queued item.
        QueueRemove = "queue.remove" (OnMachine<QueueRemoveParams>) -> Ack;
        /// Pass-through: move a queued item.
        QueueReorder = "queue.reorder" (OnMachine<QueueReorderParams>) -> Ack;
        /// Pass-through: restart a paused queue.
        QueueResume = "queue.resume" (OnMachine<SessionOpParams>) -> Ack;
        /// Pass-through: answer a request.
        RequestRespond = "request.respond" (OnMachine<RequestRespondParams>) -> RequestRespondResult;
        /// Pass-through: add todo entries.
        TodoAdd = "todo.add" (OnMachine<TodoAddParams>) -> TodoAddResult;
        /// Pass-through: edit a todo entry.
        TodoUpdate = "todo.update" (OnMachine<TodoUpdateParams>) -> Todo;
        /// Pass-through: remove a todo entry.
        TodoRemove = "todo.remove" (OnMachine<TodoRemoveParams>) -> Ack;
        /// Pass-through: move a todo entry.
        TodoReorder = "todo.reorder" (OnMachine<TodoReorderParams>) -> Ack;
        /// Pass-through: list a session's jobs.
        JobList = "job.list" (OnMachine<JobListParams>) -> JobListResult;
        /// Pass-through: read a job's log.
        JobLogsMethod = "job.logs" (OnMachine<JobLogsParams>) -> JobLogs;
        /// Pass-through: kill a job.
        JobKill = "job.kill" (OnMachine<JobOpParams>) -> Job;
        /// Pass-through: follow a job's output.
        JobFollowMethod = "job.follow" (OnMachine<JobFollowParams>) -> Job;
        /// Pass-through: stop following a job.
        JobUnfollow = "job.unfollow" (OnMachine<JobOpParams>) -> Job;
        /// Pass-through: remove a session's sandbox grant.
        SandboxRevoke = "sandbox.revoke" (OnMachine<SandboxRevokeParams>) -> Ack;
        /// Pass-through: a machine's agents.
        AgentsList = "agents.list" (OnMachine<NoParams>) -> AgentsListResult;
        /// Pass-through: list a directory on a machine.
        FsList = "fs.list" (OnMachine<FsListParams>) -> FsListResult;
        /// Pass-through: re-run a machine's doctor checks.
        DoctorReportMethod = "doctor.report" (OnMachine<NoParams>) -> DoctorReport;
        /// Pass-through: read a machine's daemon settings.
        SettingsGet = "settings.get" (OnMachine<NoParams>) -> DaemonSettings;
        /// Pass-through: change a machine's daemon settings.
        SettingsSet = "settings.set" (OnMachine<SettingsSetParams>) -> DaemonSettings;
    }
}

/// Marker types for every UI notification; see [`crate::jsonrpc::NotificationMethod`].
pub mod notifications {
    use super::*;

    rpc_notifications! {
        /// A typed controller → UI notification.
        pub enum UiNotification;

        /// Changes for a `state.subscribe` subscription.
        StateEvents = "state.events" (StateEventsParams);
        /// Timeline ops, raw frames and detail for a `session.subscribe` subscription.
        SessionEvents = "session.events" (OnMachine<SessionEventsParams>);
    }
}

pub use methods::UiRequest;
pub use notifications::UiNotification;
