//! Agent launch glue: from "start this agent for this session" to a running, detached
//! supervisor (spec §6.1, §11). The runtime's launcher port calls [`prepare`] and [`start`].
//!
//! For levels strict and relaxed, [`prepare`]:
//!
//! 1. builds the session dirs from the user's real pi agent dir (`agent/` with fake
//!    credentials, `home/`, `tmp/`) and finds the API-key providers;
//! 2. assembles the [`SessionSandboxSpec`]: routes for the configured providers that have a
//!    key, mediated commands pinned from the agent's `PATH`, the approval webhook (or, without
//!    one, every `approve` rule turned into `deny`), and the protected controlroom paths;
//! 3. generates, checks and writes `profile.json`;
//! 4. builds `nono run … -- $SHELL -lc 'exec <command> <driver args>'` and an env file with
//!    nono's own environment and a short `TMPDIR` (nono binds sockets there; `sun_path`);
//! 5. returns the supervisor [`LaunchRequest`]: the session dir holds the transcript, the
//!    socket is `<state>/run/<id>.sock`.
//!
//! Level off runs `$SHELL -lc '…'` directly with the user's own pi config.
//!
//! Signals: the supervisor runs in its own session and ignores SIGHUP, and the agent is the
//! leader of its own process group, so no HUP ever reaches pi (which exits on HUP). Inherited
//! `NONO_*` variables are removed from the whole daemon at startup (see `main`); [`prepare`]
//! refuses to run if one is present anyway, because nono reads flags such as `NONO_ALLOW`
//! from the environment.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

use controlroom_protocol::session::DriverKind;
use controlroom_sandbox::agent_dir::{
    AgentDirError, BuiltAgentDir, build_session_dirs, write_profile,
};
use controlroom_sandbox::launch::{LaunchCommand, NONO_ENV_PREFIX, Nono, NonoDirs, shell_quote};
use controlroom_sandbox::pins::resolve_pin;
use controlroom_sandbox::policy::{CommandDecision, EffectivePolicy, SandboxLevel};
use controlroom_sandbox::spec::{Approvals, CredentialHelper};
use controlroom_sandbox::webhook::{HookSecret, HookUrl};
use controlroom_sandbox::{
    AbsPath, ProfileError, SessionId, SessionLayout, SessionSandboxSpec, generate_profile,
};
use controlroom_supervisor::{Exec, LaunchRequest, Paths, SpawnSpec, SupervisorInfo};
use controlroom_transcript::{ProcessKind, StdinMode};

use crate::config::agents::AgentEntry;
use crate::config::sandbox::SandboxSettings;
use crate::dirs::{Dirs, create_private_dir};

/// Machine-level inputs of every launch.
#[derive(Debug, Clone)]
pub struct Host {
    /// The daemon's directories.
    pub dirs: Dirs,
    /// This `controlroomd` (its immutable versioned path): runs supervisors and is nono's
    /// credential helper.
    pub controlroomd: AbsPath,
    /// The login shell agents run in.
    pub shell: AbsPath,
    /// The agent's `PATH`, used to pin mediated commands.
    pub path_var: String,
    /// `sandbox.toml`, resolved.
    pub sandbox: SandboxSettings,
    /// The nono binary, when available.
    pub nono: Option<AbsPath>,
}

/// One agent start.
#[derive(Debug, Clone)]
pub struct AgentLaunch {
    /// The controlroom session.
    pub session: SessionId,
    /// The agent's working directory.
    pub cwd: AbsPath,
    /// The `agents.toml` key, recorded in the transcript's meta labels.
    pub agent_name: String,
    /// The agent entry.
    pub agent: AgentEntry,
    /// The driver's arguments for this start (`Driver::launch_args`).
    pub args: Vec<String>,
    /// The session's effective sandbox policy.
    pub policy: EffectivePolicy,
    /// The port of the daemon's approval webhook listener, if it runs.
    pub webhook_port: Option<u16>,
}

/// A launch ready to start.
#[derive(Debug, Clone)]
pub struct PreparedLaunch {
    /// What the supervisor runs.
    pub request: LaunchRequest,
    /// Sandbox details, unless the level is off.
    pub sandbox: Option<PreparedSandbox>,
}

/// The sandbox side of a prepared launch.
#[derive(Debug, Clone)]
pub struct PreparedSandbox {
    /// The session's directories.
    pub layout: SessionLayout,
    /// The approval webhook URL with its fresh secret; the listener must accept it.
    pub hook: Option<HookUrl>,
    /// What the agent dir build found.
    pub built: BuiltAgentDir,
}

/// Why a launch could not be prepared or started.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// A sandboxed level was requested but nono is unavailable.
    #[error("nono is not available, so sandboxed sessions can't start (see `controlroomd doctor`)")]
    NoNono,
    /// An inherited `NONO_*` variable would reach nono.
    #[error("refusing to launch with {0} in the environment: nono would read it as a flag")]
    InheritedNonoVariable(String),
    /// Building the session dirs or writing the profile failed.
    #[error("session dirs: {0}")]
    AgentDir(#[from] AgentDirError),
    /// The sandbox spec was rejected.
    #[error("sandbox profile: {0}")]
    Profile(#[from] ProfileError),
    /// Generating the webhook secret failed.
    #[error("webhook secret: {0}")]
    Secret(String),
    /// Writing the env file or creating the run dir failed.
    #[error("writing {path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// The error.
        #[source]
        source: std::io::Error,
    },
    /// The supervisor didn't start.
    #[error(transparent)]
    Supervisor(#[from] controlroom_supervisor::LaunchError),
}

/// Prepares everything on disk for `launch`; see the module docs.
pub fn prepare(host: &Host, launch: &AgentLaunch) -> Result<PreparedLaunch, LaunchError> {
    if let Some((key, _)) =
        std::env::vars_os().find(|(k, _)| k.to_string_lossy().starts_with(NONO_ENV_PREFIX))
    {
        return Err(LaunchError::InheritedNonoVariable(
            key.to_string_lossy().into_owned(),
        ));
    }
    let command = agent_command(&launch.agent, &launch.args);
    let layout = SessionLayout::new(host.dirs.state(), &launch.session);
    let (exec, env_file, sandbox) = if launch.policy.level == SandboxLevel::Off {
        let cmd = LaunchCommand::new(None, &host.shell, &launch.cwd, &command);
        (argv(&cmd), None, None)
    } else {
        let nono = Nono {
            binary: host.nono.clone().ok_or(LaunchError::NoNono)?,
            dirs: NonoDirs::under(host.dirs.state()),
        };
        let built = build_session_dirs(&layout, &host.sandbox.agent_dir)?;
        let hook = match launch.webhook_port {
            Some(port) => Some(HookUrl {
                port,
                session: launch.session.clone(),
                secret: HookSecret::generate().map_err(|e| LaunchError::Secret(e.to_string()))?,
            }),
            None => None,
        };
        let spec = sandbox_spec(host, launch, &layout, &built, hook.clone());
        write_profile(&layout, &generate_profile(&spec)?)?;
        let cmd = LaunchCommand::new(
            Some((&nono, &layout.profile())),
            &host.shell,
            &launch.cwd,
            &command,
        );
        let mut env = cmd.env.clone();
        env.push(("TMPDIR".to_owned(), host.sandbox.nono_tmp.to_string()));
        let env_file = layout.root().join("launch.env");
        write_env_file(&env_file, &env)?;
        let sandbox = PreparedSandbox {
            layout: layout.clone(),
            hook,
            built,
        };
        (argv(&cmd), Some(env_file), Some(sandbox))
    };
    let run = host.dirs.run();
    create_private_dir(&run).map_err(|source| LaunchError::Io {
        path: run.to_string(),
        source,
    })?;
    let id = launch.session.as_str().to_owned();
    let labels = BTreeMap::from([
        ("agent".to_owned(), launch.agent_name.clone()),
        (
            "driver".to_owned(),
            driver_label(launch.agent.driver).to_owned(),
        ),
        (
            "sandbox".to_owned(),
            level_label(launch.policy.level).to_owned(),
        ),
    ]);
    Ok(PreparedLaunch {
        request: LaunchRequest {
            paths: Paths {
                dir: layout.root().as_path().to_owned(),
                socket: host.dirs.supervisor_socket(&id).as_path().to_owned(),
            },
            id,
            spec: SpawnSpec {
                kind: ProcessKind::Agent,
                exec,
                cwd: launch.cwd.as_path().to_owned(),
                env_file: env_file.map(|f| f.as_path().to_owned()),
                stdin: StdinMode::Pipe,
                timeout_ms: None,
                max_line_bytes: None,
                labels,
            },
        },
        sandbox,
    })
}

/// Finds nono from `sandbox.toml`'s `nono` setting: an absolute (or `~/`) path as is, a
/// program name through the login shell; then canonicalized (store paths, not profile links).
pub fn find_nono(shell: &AbsPath, setting: &str, home: &AbsPath) -> Result<AbsPath, String> {
    let path = if setting.starts_with('/') || setting.starts_with('~') {
        crate::dirs::expand_home(setting, home)
            .map_err(|e| e.to_string())?
            .to_string()
    } else {
        crate::shell::resolve_program(shell, setting)
            .map_err(|e| e.describe(shell.as_str()))?
            .ok_or_else(|| format!("`{setting}` not found by the login shell ({shell} -lc)"))?
    };
    let real = std::fs::canonicalize(&path).map_err(|e| format!("{path}: {e}"))?;
    AbsPath::from_path(&real).map_err(|e| e.to_string())
}

/// Starts the supervisor of a prepared launch and returns once it is ready. The supervisor is
/// detached: it outlives this process.
pub async fn start(host: &Host, prepared: &PreparedLaunch) -> Result<SupervisorInfo, LaunchError> {
    Ok(controlroom_supervisor::launch(host.controlroomd.as_path(), &prepared.request).await?)
}

/// `exec <command> <args…>`: the login shell replaces itself with the agent.
fn agent_command(agent: &AgentEntry, args: &[String]) -> String {
    let mut command = format!("exec {}", agent.command.trim());
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
}

fn argv(cmd: &LaunchCommand) -> Exec {
    Exec::Argv(
        std::iter::once(cmd.program.clone())
            .chain(cmd.args.iter().cloned())
            .collect(),
    )
}

fn driver_label(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Pi => "pi",
        DriverKind::Fake => "fake",
        DriverKind::Unknown => "unknown",
    }
}

fn level_label(level: SandboxLevel) -> &'static str {
    match level {
        SandboxLevel::Off => "off",
        SandboxLevel::Relaxed => "relaxed",
        SandboxLevel::Strict => "strict",
    }
}

fn sandbox_spec(
    host: &Host,
    launch: &AgentLaunch,
    layout: &SessionLayout,
    built: &BuiltAgentDir,
    hook: Option<HookUrl>,
) -> SessionSandboxSpec {
    let mut policy = launch.policy.clone();
    if hook.is_none() {
        // Nobody could answer an approval: fail closed.
        for rule in &mut policy.commands {
            if rule.decision == CommandDecision::Approve {
                rule.decision = CommandDecision::Deny;
            }
        }
    }
    let pins = policy
        .commands
        .iter()
        .filter_map(|rule| {
            let pin = resolve_pin(rule.command.as_str(), &host.path_var)?;
            Some((rule.command.clone(), pin))
        })
        .collect();
    let dirs = &host.dirs;
    let mut protected = vec![dirs.state().clone(), dirs.data(), dirs.config()];
    protected.extend(host.sandbox.agent_dir.user_agent_dir.clone());
    SessionSandboxSpec {
        layout: layout.clone(),
        cwd: launch.cwd.clone(),
        policy,
        read_only: host.sandbox.read_only.clone(),
        relaxed_reads: host.sandbox.relaxed_reads.clone(),
        // The controller socket is not for agents; @controlroom/pi gets its own (P5).
        daemon_socket: None,
        protected,
        credential_routes: host
            .sandbox
            .routes
            .iter()
            .filter(|r| {
                built
                    .api_key_providers
                    .iter()
                    .any(|p| p == r.provider.as_str())
            })
            .cloned()
            .collect(),
        credential_helper: CredentialHelper {
            controlroomd: host.controlroomd.clone(),
            state_dir: dirs.state().clone(),
        },
        approvals: hook.map(|hook| Approvals {
            hook,
            timeout_secs: host.sandbox.approval_timeout_secs,
        }),
        pins,
    }
}

/// Writes `KEY=VALUE` lines with mode 0600, replacing the file atomically.
fn write_env_file(path: &AbsPath, vars: &[(String, String)]) -> Result<(), LaunchError> {
    let io = |source| LaunchError::Io {
        path: path.to_string(),
        source,
    };
    let tmp = path
        .parent()
        .expect("the env file has a parent")
        .join(".launch.env.tmp");
    let _ = std::fs::remove_file(tmp.as_path());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp.as_path())
        .map_err(io)?;
    for (key, value) in vars {
        writeln!(file, "{key}={value}").map_err(io)?;
    }
    file.sync_all().map_err(io)?;
    std::fs::rename(tmp.as_path(), path.as_path()).map_err(io)
}

#[cfg(test)]
mod tests;
