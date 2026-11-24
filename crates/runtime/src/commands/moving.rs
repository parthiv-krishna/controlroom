//! `session.move`: a session moves to another profile (design `docs/design/profiles-tenants.md`
//! §6). The controller checks membership first; here the session takes the target profile, so
//! its next launch gets that profile's model policy. A live agent restarts and resumes for it,
//! through the same relaunch as a sandbox-level change; a stopped one gets it when it next
//! starts. Refused (`busy`) while a run is active or the agent is starting, so a run never
//! straddles two policies. Its schedules follow it (they are the controller's, by session).

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::SessionMoveParams;

use super::start_mode_of;
use crate::core::Core;
use crate::errors::busy;

impl Core {
    /// `session.move` (see the module docs).
    pub(crate) fn move_session(&mut self, params: &SessionMoveParams) -> Result<(), RpcError> {
        let id = params.id;
        let session = self.session(id)?;
        if session.data.profile_id == Some(params.profile_id) {
            return Ok(());
        }
        if session.busy() {
            return Err(busy("a run is active; move the session once it ends"));
        }
        let restart = if session.data.closed() || !session.has_process() {
            None
        } else if session.agent_live() && !session.live.relaunch {
            Some(start_mode_of(session)?)
        } else {
            return Err(busy("the agent is starting; try again"));
        };
        self.session_mut(id)?.data.profile_id = Some(params.profile_id);
        if let Some(by) = restart {
            self.restart_agent(id, by)?;
        }
        Ok(())
    }
}
