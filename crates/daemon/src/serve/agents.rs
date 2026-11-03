//! The runtime's [`Agents`] port: `agents.toml`, driver selection (`pi` → [`PiDriver`]), and
//! the launch glue ([`crate::launch`]) that turns a start into a supervisor spawn spec.
//!
//! The sandbox policy of a launch merges three scopes ([`effective_policy`]): the default
//! mediated commands of spec §11.5b (a baseline every session gets), the session's defaults
//! from the controller (grants, domains, mediated commands), and the session's level. Entries
//! that can't be expressed are dropped when dropping only narrows access (paths, domains);
//! an unusable mediated command fails the launch, because dropping it would widen access.
//! Launches get the approval webhook's port when its listener runs; without it, `approve` rules
//! fail closed to `deny`.

use std::sync::{Arc, Mutex};

use controlroom_drivers::{Driver, FakeDriver, PiDriver};
use controlroom_protocol::session::{
    DriverKind, GrantKind, GrantScope, MediationAction, PathAccess, SandboxLevel as WireLevel,
    SandboxSpec,
};
use controlroom_protocol::{ErrorData, Resource, RpcError};
use controlroom_runtime::ports::{AgentLaunch, Agents};
use controlroom_sandbox::policy::{
    Access, CommandDecision, CommandRule, PathGrant, PathKind, ScopePolicy, default_command_rules,
};
use controlroom_sandbox::{AbsPath, Domain, EffectivePolicy, Name, SandboxLevel, effective_policy};
use controlroom_supervisor::SpawnSpec;

use super::agent::AgentSockets;
use crate::config::agents::{self as agents_config, AgentEntry};
use crate::config::sandbox as sandbox_config;
use crate::dirs::Dirs;
use crate::launch::{self, Host};
use crate::shell::login_shell;

/// The configured agents of this daemon.
pub struct DaemonAgents {
    dirs: Dirs,
    controlroomd: AbsPath,
    /// The approval webhook listener's port, if it runs.
    webhook_port: Option<u16>,
    /// nono, once found (looked up again while missing).
    nono: Mutex<Option<AbsPath>>,
    /// The OAuth helper, once found (looked up again while missing).
    oauth_helper: Mutex<Option<AbsPath>>,
    /// The agent sockets; a launch binds its session's first.
    sockets: Arc<AgentSockets>,
}

impl DaemonAgents {
    /// Agents of `dirs`, launched by `controlroomd`, with approvals sent to `webhook_port`
    /// and the extension's socket served by `sockets`.
    pub fn new(
        dirs: Dirs,
        controlroomd: AbsPath,
        webhook_port: Option<u16>,
        sockets: Arc<AgentSockets>,
    ) -> Self {
        Self {
            dirs,
            controlroomd,
            webhook_port,
            nono: Mutex::new(None),
            oauth_helper: Mutex::new(None),
            sockets,
        }
    }

    /// The agent entry, re-read from `agents.toml` so edits apply to the next start.
    fn entry(&self, agent: &str) -> Result<AgentEntry, RpcError> {
        let loaded = agents_config::load(&self.dirs);
        if let Some(e) = &loaded.error {
            tracing::warn!("{e}");
        }
        loaded.value.0.get(agent).cloned().ok_or_else(|| {
            RpcError::new(
                ErrorData::NotFound {
                    resource: Resource::Agent,
                    id: agent.to_owned(),
                },
                format!("no agent {agent:?} in agents.toml"),
            )
        })
    }

    fn host(&self, sandboxed: bool) -> Result<Host, RpcError> {
        let loaded = sandbox_config::load(&self.dirs);
        if let Some(e) = &loaded.error {
            tracing::warn!("{e}");
        }
        let pi_agent_dir = std::env::var("PI_CODING_AGENT_DIR").ok();
        let sandbox = loaded
            .value
            .resolve(self.dirs.home(), pi_agent_dir.as_deref())
            .map_err(|e| sandbox_unavailable(e.to_string()))?;
        let shell = login_shell();
        let (nono, oauth_helper) = if sandboxed {
            (
                Some(self.nono(&shell, &sandbox.nono)?),
                self.oauth_helper(&shell, &sandbox.oauth_helper),
            )
        } else {
            (None, None)
        };
        Ok(Host {
            dirs: self.dirs.clone(),
            controlroomd: self.controlroomd.clone(),
            shell,
            path_var: std::env::var("PATH").unwrap_or_default(),
            sandbox,
            nono,
            oauth_helper,
        })
    }

    /// The OAuth helper; OAuth providers get no credential route without it.
    fn oauth_helper(&self, shell: &AbsPath, setting: &str) -> Option<AbsPath> {
        let mut cached = self.oauth_helper.lock().unwrap_or_else(|e| e.into_inner());
        if cached.is_none() {
            match launch::find_program(shell, setting, self.dirs.home()) {
                Ok(path) => *cached = Some(path),
                Err(e) => tracing::debug!("no OAuth helper: {e}"),
            }
        }
        cached.clone()
    }

    fn nono(&self, shell: &AbsPath, setting: &str) -> Result<AbsPath, RpcError> {
        let mut cached = self.nono.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = cached.as_ref() {
            return Ok(path.clone());
        }
        let path = launch::find_program(shell, setting, self.dirs.home())
            .map_err(|e| sandbox_unavailable(format!("nono: {e} (see `controlroomd doctor`)")))?;
        *cached = Some(path.clone());
        Ok(path)
    }
}

fn sandbox_unavailable(detail: String) -> RpcError {
    RpcError::new(
        ErrorData::SandboxUnavailable {
            detail: detail.clone(),
        },
        detail,
    )
}

impl Agents for DaemonAgents {
    fn resolve(&self, agent: &str) -> Result<DriverKind, RpcError> {
        Ok(self.entry(agent)?.driver)
    }

    fn driver(&self, kind: DriverKind) -> Box<dyn Driver> {
        match kind {
            DriverKind::Fake => Box::new(FakeDriver::new(Default::default())),
            DriverKind::Pi | DriverKind::Unknown => Box::new(PiDriver::new()),
        }
    }

    fn spawn_spec(&self, start: &AgentLaunch) -> Result<SpawnSpec, RpcError> {
        let agent = self.entry(&start.agent)?;
        let policy = policy(start.sandbox_level, &start.sandbox)?;
        let host = self.host(policy.level != SandboxLevel::Off)?;
        let session = controlroom_sandbox::SessionId::new(&start.session.to_string())
            .map_err(|e| sandbox_unavailable(e.to_string()))?;
        let cwd =
            AbsPath::new(start.cwd.as_str()).map_err(|e| sandbox_unavailable(e.to_string()))?;
        if let Err(e) = self.sockets.ensure(start.session) {
            // The agent still runs; its extension keeps retrying the socket.
            tracing::warn!("session {}: binding its agent socket: {e}", start.session);
        }
        let launch = launch::AgentLaunch {
            session,
            cwd,
            agent_name: start.agent.clone(),
            agent,
            args: start.args.clone(),
            policy,
            webhook_port: self.webhook_port,
        };
        let prepared =
            launch::prepare(&host, &launch).map_err(|e| sandbox_unavailable(e.to_string()))?;
        Ok(prepared.request.spec)
    }
}

/// The effective policy of a launch; see the module docs.
pub fn policy(level: WireLevel, spec: &SandboxSpec) -> Result<EffectivePolicy, RpcError> {
    let baseline = ScopePolicy {
        commands: default_command_rules(),
        ..ScopePolicy::default()
    };
    let mut defaults = ScopePolicy::default();
    for grant in &spec.grants {
        if grant.scope == GrantScope::Once {
            continue;
        }
        match grant.kind {
            GrantKind::Path => {
                let Ok(path) = AbsPath::new(&grant.value) else {
                    tracing::warn!("ignoring the path grant {:?}", grant.value);
                    continue;
                };
                let kind = if path.as_path().is_file() {
                    PathKind::File
                } else {
                    PathKind::Dir
                };
                let access = match grant.access {
                    Some(PathAccess::Write) => Access::ReadWrite,
                    _ => Access::Read,
                };
                defaults.paths.push(PathGrant { path, kind, access });
            }
            GrantKind::Domain => match Domain::new(&grant.value) {
                Ok(domain) => defaults.domains.push(domain),
                Err(e) => tracing::warn!("ignoring the domain grant {:?}: {e}", grant.value),
            },
            GrantKind::Command | GrantKind::Unknown => {}
        }
    }
    for domain in &spec.domains {
        match Domain::new(domain) {
            Ok(domain) => defaults.domains.push(domain),
            Err(e) => tracing::warn!("ignoring the domain {domain:?}: {e}"),
        }
    }
    for mediated in &spec.mediated {
        let mut words = mediated.command.split_whitespace();
        let command = words
            .next()
            .and_then(|c| Name::new(c).ok())
            .ok_or_else(|| {
                sandbox_unavailable(format!("invalid mediated command {:?}", mediated.command))
            })?;
        defaults.commands.push(CommandRule {
            command,
            args: words.map(str::to_owned).collect(),
            decision: match mediated.action {
                MediationAction::Approve => CommandDecision::Approve,
                MediationAction::Deny | MediationAction::Unknown => CommandDecision::Deny,
            },
        });
    }
    let session = ScopePolicy {
        level: Some(match level {
            WireLevel::Off => SandboxLevel::Off,
            WireLevel::Relaxed => SandboxLevel::Relaxed,
            WireLevel::Strict | WireLevel::Unknown => SandboxLevel::Strict,
        }),
        ..ScopePolicy::default()
    };
    Ok(effective_policy([&baseline, &defaults], &session))
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::ids::GrantId;
    use controlroom_protocol::session::{MediatedCommand, SandboxGrant};

    use super::*;

    fn grant(kind: GrantKind, value: &str, scope: GrantScope) -> SandboxGrant {
        SandboxGrant {
            id: GrantId::generate(),
            kind,
            value: value.into(),
            access: Some(PathAccess::Write),
            scope,
            granted_at: UnixMillis(1),
            by: None,
        }
    }

    #[test]
    fn policies_merge_the_baseline_defaults_and_level() {
        let spec = SandboxSpec {
            level: WireLevel::Strict,
            grants: vec![
                grant(GrantKind::Path, "/srv/data", GrantScope::Standing),
                grant(GrantKind::Path, "/srv/once", GrantScope::Once),
                grant(GrantKind::Path, "relative", GrantScope::Standing),
                grant(
                    GrantKind::Domain,
                    "registry.example.com",
                    GrantScope::Session,
                ),
            ],
            domains: vec!["git.example.com".into(), "*.bad".into()],
            mediated: vec![MediatedCommand {
                command: "kubectl apply".into(),
                action: MediationAction::Approve,
            }],
        };
        let p = policy(WireLevel::Relaxed, &spec).unwrap();
        assert_eq!(p.level, SandboxLevel::Relaxed);
        assert_eq!(p.paths.len(), 1);
        assert_eq!(p.paths[0].access, Access::ReadWrite);
        let domains: Vec<&str> = p.domains.iter().map(Domain::as_str).collect();
        assert_eq!(domains, ["git.example.com", "registry.example.com"]);
        let rule = |c: &str| p.commands.iter().find(|r| r.command.as_str() == c).cloned();
        assert_eq!(rule("kubectl").unwrap().args, ["apply"]);
        assert_eq!(rule("ssh").unwrap().decision, CommandDecision::Deny);
        assert_eq!(rule("git").unwrap().decision, CommandDecision::Approve);

        let bad = SandboxSpec {
            mediated: vec![MediatedCommand {
                command: "../x".into(),
                action: MediationAction::Deny,
            }],
            ..spec
        };
        assert!(policy(WireLevel::Strict, &bad).is_err());
        assert_eq!(
            policy(
                WireLevel::Unknown,
                &SandboxSpec {
                    level: WireLevel::Off,
                    grants: vec![],
                    domains: vec![],
                    mediated: vec![],
                }
            )
            .unwrap()
            .level,
            SandboxLevel::Strict
        );
    }
}
