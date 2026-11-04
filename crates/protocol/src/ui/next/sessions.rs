//! Session creation, focus reports and doctor targets of the next UI API.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agentd::SessionCreateParams;
use crate::ids::{DeviceId, MachineId, SessionId, TaskId};

/// Params of the UI `session.create`: the agent daemon's create (workspaces, optional first
/// `prompt`) plus the tasks to link the new session to.
///
/// The controller fills in the sandbox defaults (from the workspaces' machines and the linked
/// tasks' projects) and the prompt's origin (the caller's device, replacing any value sent),
/// creates the session with its first prompt atomically on the agent daemon, then links it to
/// every task in `task_ids`, moving tasks in `backlog` or `todo` to `doing`. Retrying with the
/// same `op_id` is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UiSessionCreateParams {
    /// The agent daemon's create params (boxed: they are by far the largest UI params).
    #[serde(flatten)]
    pub create: Box<SessionCreateParams>,
    /// Tasks to link the new session to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<TaskId>,
}

/// Params of `client.focus`: what this client shows, so pushes for a session the user is
/// looking at are skipped. Send it whenever the open session, visibility or focus changes;
/// closing the connection clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ClientFocusParams {
    /// This browser's device.
    pub device_id: DeviceId,
    /// The open session, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Whether the page is visible and focused.
    pub visible: bool,
}

/// Params of `doctor.report`: whose environment checks to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DoctorReportParams {
    /// A machine's host daemon; default the server's agent daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
}
