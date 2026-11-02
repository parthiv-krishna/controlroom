//! Session commands the controller does more for than pass them through.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::daemon::SessionCreateParams;
use crate::ids::{MachineId, TaskId};

/// Params of the UI `session.create`: the daemon's create (including its optional first
/// `prompt`) plus the tasks to link the new session to.
///
/// The controller fills in the sandbox defaults and the prompt's origin (the caller's device,
/// replacing any value sent), creates the session with its first prompt atomically on the
/// daemon, then links it to every task in `task_ids`, moving tasks in `backlog` or `todo` to
/// `doing`. Retrying with the same `op_id` is safe: the daemon returns the original session
/// (the prompt is sent once), and links and moves are idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UiSessionCreateParams {
    /// The machine to create the session on.
    pub machine_id: MachineId,
    /// The daemon's create params (boxed: they are by far the largest UI params).
    #[serde(flatten)]
    pub create: Box<SessionCreateParams>,
    /// Tasks to link the new session to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<TaskId>,
}
