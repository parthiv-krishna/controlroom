//! Which shell a stream attaches to, and whether it is still there.

use controlroom_protocol::ids::{MachineId, SessionId, ShellId};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::{ErrorData, RpcError};

use crate::hub::{BusEvent, Hub};

/// A session shell on its machine.
#[derive(Debug, Clone)]
pub(super) struct Target {
    pub session: SessionId,
    pub shell: String,
    pub shell_id: ShellId,
    pub machine: MachineId,
}

impl Target {
    /// Looks the shell up in the agent daemon's sessions.
    pub(super) fn resolve(hub: &Hub, session: SessionId, shell: &str) -> Result<Self, RpcError> {
        let summary = hub.session(session).ok_or_else(|| {
            RpcError::new(
                ErrorData::UnknownSession { id: session },
                format!("no session {session}"),
            )
        })?;
        let machine = machine_of(&summary, shell).ok_or_else(|| {
            RpcError::new(
                ErrorData::ShellGone {
                    shell: shell.to_owned(),
                },
                format!("session {session} has no shell {shell:?}"),
            )
        })?;
        Ok(Self {
            session,
            shell: shell.to_owned(),
            shell_id: ShellId::of(session, shell),
            machine,
        })
    }

    /// Whether `event` may concern this shell (then check [`Self::gone`]).
    pub(super) fn concerns(&self, event: &BusEvent) -> bool {
        match event {
            BusEvent::SessionRemoved { session_id } => *session_id == self.session,
            BusEvent::Session { current, .. } => current.id == self.session,
            other => other.machine_id() == Some(self.machine),
        }
    }

    /// Why the stream must end, if it must: its session, shell or machine is gone.
    pub(super) fn gone(&self, hub: &Hub) -> Option<String> {
        if hub.machine_name(self.machine).is_none() {
            return Some("its machine was removed".to_owned());
        }
        let Some(session) = hub.session(self.session) else {
            return Some("the session was deleted".to_owned());
        };
        if machine_of(&session, &self.shell) != Some(self.machine) {
            return Some(format!("the shell {:?} was closed", self.shell));
        }
        None
    }

    /// The machine's display name, for messages.
    pub(super) fn machine_name(&self, hub: &Hub) -> String {
        hub.machine_name(self.machine)
            .unwrap_or_else(|| self.machine.to_string())
    }
}

/// The machine of `session`'s shell `name`.
fn machine_of(session: &SessionSummary, name: &str) -> Option<MachineId> {
    let shell = session.shells.iter().find(|s| s.name == name)?;
    session
        .workspaces
        .iter()
        .find(|w| w.id == shell.workspace_id)
        .map(|w| w.machine_id)
}
