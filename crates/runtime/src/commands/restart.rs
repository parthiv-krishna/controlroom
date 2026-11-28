//! `session.restart`: relaunch the session's agent and resume its conversation. Some changes
//! only take effect at launch (the credential routes and accounts of Settings › Models, the
//! profile's model policy and defaults, `agents.toml`), and agents outlive restarts of the agent
//! daemon, so this is how a running agent gets them.
//!
//! A live agent restarts through the same relaunch as a sandbox level change: its running run
//! stops (the UI confirms first), its host calls are cancelled, and nothing new is written to
//! the old process. A stopped agent starts. Refused (`busy`) while the agent is starting or
//! already restarting, and for closed sessions (`invalid_state`).

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::SessionRestartParams;
use controlroom_protocol::session::SessionState;

use super::start_mode_of;
use crate::core::{Core, Task};
use crate::errors::{busy, invalid_state};

impl Core {
    /// `session.restart` (see the module docs).
    pub(crate) fn restart(&mut self, params: &SessionRestartParams) -> Result<(), RpcError> {
        let id = params.id;
        let session = self.session(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "session.restart"));
        }
        let live = session.has_process();
        if live && !(session.agent_live() && !session.live.relaunch) {
            return Err(busy("the agent is starting; try again"));
        }
        let by = start_mode_of(session)?;
        if let Some(spec) = &params.sandbox {
            self.session_mut(id)?.data.sandbox = spec.clone();
            self.task(Task::Refresh(id));
        }
        if live {
            self.restart_agent(id, by)
        } else {
            self.request_start(id, by)
        }
    }
}
