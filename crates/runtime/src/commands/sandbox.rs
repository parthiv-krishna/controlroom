//! `session.set_sandbox_level`: changing the level a session runs at after creation (spec §11).
//!
//! The new level applies everywhere the level matters:
//!
//! - **the agent** (its server sandbox): a live agent restarts and resumes its conversation,
//!   through the same relaunch path as a restarting start fresh; a running run stops with it.
//!   A stopped agent gets the level when it next starts;
//! - **the workspaces**: they are reopened with the new level (`workspace.open`); each host
//!   rewrites the workspace's profiles and restarts the agent's shells there. A host that is
//!   offline gets the level when it reconnects (every reconnect reopens the workspaces);
//! - **the timeline**: a `session_info` divider with the new level;
//! - **the agent's context**: a non-waking note in its next turn. While the restart is pending
//!   nothing is written to the old process (see `reconcile`), so the note reaches the new one.

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::SessionSetSandboxLevelParams;
use controlroom_protocol::ids::{ItemId, SessionId};
use controlroom_protocol::session::{SandboxLevel, SessionState, StartedBy};
use controlroom_protocol::timeline::{InjectOrigin, ItemBody};

use super::session::STOP_GRACE;
use super::start_mode_of;
use crate::core::{Core, Task};
use crate::errors::{busy, invalid_params, invalid_state};
use crate::model::Injection;

const METHOD: &str = "session.set_sandbox_level";
/// The label of the note about the level (and its run trigger, should it start one).
const NOTE_LABEL: &str = "sandbox level";

impl Core {
    /// `session.set_sandbox_level` (see the module docs).
    pub(crate) fn set_sandbox_level(
        &mut self,
        params: &SessionSetSandboxLevelParams,
    ) -> Result<(), RpcError> {
        let id = params.id;
        if params.level == Some(SandboxLevel::Unknown)
            || params
                .sandbox
                .as_ref()
                .is_some_and(|s| s.level == SandboxLevel::Unknown)
        {
            return Err(invalid_params("unknown sandbox level"));
        }
        let session = self.session(id)?;
        let data = &session.data;
        if data.closed() {
            return Err(invalid_state(SessionState::Closed, METHOD));
        }
        let defaults = params.sandbox.as_ref().unwrap_or(&data.sandbox).level;
        let level = params.level.unwrap_or(defaults);
        let changed = level != data.sandbox_level();
        // A live, ready agent restarts; one on its way up can't be restarted safely yet.
        let restart = if !changed || !session.has_process() {
            None
        } else if session.agent_live() && !session.live.relaunch {
            Some(start_mode_of(session)?)
        } else {
            return Err(busy("the agent is starting; try again"));
        };
        let profile = data.effective_sandbox();

        let data = &mut self.session_mut(id)?.data;
        data.sandbox_level = params.level;
        if let Some(spec) = &params.sandbox {
            data.sandbox = spec.clone();
        }
        if data.effective_sandbox() != profile {
            self.task(Task::Refresh(id));
        }
        if !changed {
            return Ok(());
        }
        if let Some(by) = restart {
            self.restart_agent(id, by)?;
        }
        let divider = ItemBody::SessionInfo {
            title: None,
            model: None,
            thinking: None,
            sandbox: Some(level),
        };
        self.put_item(
            id,
            ItemId::new(format!("sandbox-{}", params.op_id)),
            divider,
        )?;
        self.sandbox_note(id, level)
    }

    /// Stops the live agent and starts it again (`by`, normally a resume) once it is gone.
    /// Its running host calls are cancelled, as for `session.cancel`.
    fn restart_agent(&mut self, id: SessionId, by: StartedBy) -> Result<(), RpcError> {
        self.cancel_calls(id, None);
        let session = self.session_mut(id)?;
        session.data.pending_start = Some(by);
        session.live.relaunch = true;
        if let Some(attachment) = &session.live.attachment {
            let conn = attachment.conn.clone();
            self.task(Task::Terminate(conn, STOP_GRACE));
        }
        Ok(())
    }

    /// Tells the agent the new level in its next turn (without waking it). Only the latest
    /// change matters: a note about an earlier one that hasn't gone out is dropped.
    fn sandbox_note(&mut self, id: SessionId, level: SandboxLevel) -> Result<(), RpcError> {
        let now = self.clock.now();
        let data = &mut self.session_mut(id)?.data;
        data.injections
            .retain(|i| i.label != NOTE_LABEL || i.delivery.is_some());
        let injection = Injection::new(
            InjectOrigin::Workspace,
            NOTE_LABEL.into(),
            note(level),
            false,
            now,
        );
        self.inject(id, injection)
    }
}

/// The agent's note about a new level.
fn note(level: SandboxLevel) -> String {
    let (name, means) = match level {
        SandboxLevel::Off => (
            "off",
            "your shells, jobs and file tools on the hosts run unsandboxed",
        ),
        SandboxLevel::Relaxed => (
            "relaxed",
            "the workspaces plus read-only home configuration (such as git's); other paths ask \
             the user",
        ),
        SandboxLevel::Strict | SandboxLevel::Unknown => {
            ("strict", "the workspaces only; other paths ask the user")
        }
    };
    format!(
        "[controlroom] The user changed the sandbox level: it is now {name} ({means}). Your \
         shells restart under it (their variables are lost); background jobs already running \
         keep the old level until they end."
    )
}
