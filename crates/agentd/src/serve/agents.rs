//! The runtime's [`Agents`] port: the `pi` entry of `agents.toml`, driver selection
//! (`pi` → [`PiDriver`]), and the launch glue ([`crate::launch`]) that turns a start into a
//! supervisor spawn spec.
//!
//! Only the session's sandbox *level* applies to pi on the server; its grants, domains and
//! mediated commands are the workspaces' and apply on their hosts.

use std::sync::{Arc, Mutex, PoisonError};

use controlroom_drivers::{Driver, FakeDriver, PiDriver};
use controlroom_protocol::session::{DriverKind, SandboxLevel as WireLevel};
use controlroom_protocol::{ErrorData, Resource, RpcError};
use controlroom_runtime::ports::{AgentLaunch, Agents};
use controlroom_sandbox::{AbsPath, SandboxLevel};
use controlroom_supervisor::SpawnSpec;

use super::agent::AgentSockets;
use crate::config::agents::{self as agents_config, AgentEntry, SESSION_AGENT};
use crate::config::sandbox as sandbox_config;
use crate::dirs::{Dirs, home};
use crate::launch::{self, Host, find_program};
use crate::shell::login_shell;

/// The server's agent.
pub struct ServerAgents {
    dirs: Dirs,
    agentd: AbsPath,
    /// nono, once found (looked up again while missing).
    nono: Mutex<Option<AbsPath>>,
    /// The OAuth helper, once found (looked up again while missing).
    oauth_helper: Mutex<Option<AbsPath>>,
    /// The agent sockets; a launch binds its session's first.
    sockets: Arc<AgentSockets>,
}

impl ServerAgents {
    /// Agents of `dirs`, launched by `agentd`, with the extension's socket served by
    /// `sockets`.
    pub fn new(dirs: Dirs, agentd: AbsPath, sockets: Arc<AgentSockets>) -> Self {
        Self {
            dirs,
            agentd,
            nono: Mutex::new(None),
            oauth_helper: Mutex::new(None),
            sockets,
        }
    }

    /// The session agent's entry, re-read from `agents.toml` so edits apply to the next start.
    fn entry(&self) -> Result<AgentEntry, RpcError> {
        let loaded = agents_config::load(&self.dirs);
        if let Some(e) = &loaded.error {
            tracing::warn!("{e}");
        }
        loaded.value.0.get(SESSION_AGENT).cloned().ok_or_else(|| {
            RpcError::new(
                ErrorData::NotFound {
                    resource: Resource::Agent,
                    id: SESSION_AGENT.to_owned(),
                },
                "no pi agent in agents.toml",
            )
        })
    }

    fn host(&self, sandboxed: bool) -> Result<Host, RpcError> {
        let loaded = sandbox_config::load(&self.dirs);
        if let Some(e) = &loaded.error {
            tracing::warn!("{e}");
        }
        let mut sandbox = loaded
            .value
            .resolve(home().as_ref(), &self.dirs.pi_agent_dir())
            .map_err(|e| sandbox_unavailable(e.to_string()))?;
        // Settings › Models, re-read so changes apply to the next start.
        crate::models::merge::apply(&self.dirs, &mut sandbox);
        let shell = login_shell();
        let (nono, oauth_helper) = if sandboxed {
            let nono = cached(&self.nono, &shell, &sandbox.nono)
                .map_err(|e| sandbox_unavailable(format!("nono: {e}")))?;
            (
                Some(nono),
                cached(&self.oauth_helper, &shell, &sandbox.oauth_helper).ok(),
            )
        } else {
            (None, None)
        };
        Ok(Host {
            dirs: self.dirs.clone(),
            agentd: self.agentd.clone(),
            shell,
            sandbox,
            nono,
            oauth_helper,
        })
    }
}

/// A program found once and remembered.
fn cached(
    slot: &Mutex<Option<AbsPath>>,
    shell: &AbsPath,
    setting: &str,
) -> Result<AbsPath, String> {
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = slot.as_ref() {
        return Ok(path.clone());
    }
    let path = find_program(shell, setting)?;
    *slot = Some(path.clone());
    Ok(path)
}

fn sandbox_unavailable(detail: String) -> RpcError {
    RpcError::new(
        ErrorData::SandboxUnavailable {
            detail: detail.clone(),
        },
        detail,
    )
}

impl Agents for ServerAgents {
    fn kind(&self) -> Result<DriverKind, RpcError> {
        Ok(self.entry()?.driver)
    }

    fn driver(&self, kind: DriverKind) -> Box<dyn Driver> {
        match kind {
            DriverKind::Fake => Box::new(FakeDriver::new(Default::default())),
            DriverKind::Pi | DriverKind::Unknown => Box::new(PiDriver::new()),
        }
    }

    fn spawn_spec(&self, start: &AgentLaunch) -> Result<SpawnSpec, RpcError> {
        let agent = self.entry()?;
        let level = match start.sandbox_level {
            WireLevel::Off => SandboxLevel::Off,
            WireLevel::Relaxed => SandboxLevel::Relaxed,
            WireLevel::Strict | WireLevel::Unknown => SandboxLevel::Strict,
        };
        let host = self.host(level != SandboxLevel::Off)?;
        let session = controlroom_sandbox::SessionId::new(&start.session.to_string())
            .map_err(|e| sandbox_unavailable(e.to_string()))?;
        if let Err(e) = self.sockets.ensure(start.session) {
            // The agent still runs; its extension keeps retrying the socket.
            tracing::warn!("session {}: binding its agent socket: {e}", start.session);
        }
        let launch = launch::AgentLaunch {
            session,
            agent_name: SESSION_AGENT.to_owned(),
            agent,
            args: start.args.clone(),
            level,
        };
        let prepared =
            launch::prepare(&host, &launch).map_err(|e| sandbox_unavailable(e.to_string()))?;
        Ok(prepared.request.spec)
    }
}
