//! Agent launch glue: from "start this agent for this session" to a running, detached
//! supervisor (spec §6.1, §11). The runtime's launcher port calls [`prepare`] and [`start`].
//!
//! For levels strict and relaxed, [`prepare`]:
//!
//! 1. builds the session dirs from the user's real pi agent dir (`agent/` with fake
//!    credentials, `home/`, `tmp/`) and finds the API-key and OAuth providers;
//! 2. assembles the [`SessionSandboxSpec`]: routes for the configured providers that have a
//!    key (or an OAuth token, given the OAuth helper), read access to the `agent/npm` link's
//!    target, mediated commands pinned from the agent's `PATH`, the approval webhook (or, without
//!    one, every `approve` rule turned into `deny`), the session's agent socket, and the
//!    protected controlroom paths;
//! 3. generates, checks and writes `profile.json`;
//! 4. builds `nono run … -- $SHELL -lc 'exec <command> <driver args>'` and an env file with
//!    nono's own environment and a short `TMPDIR` (nono binds sockets there; `sun_path`);
//! 5. returns the supervisor [`LaunchRequest`]: the session dir holds the transcript, the
//!    socket is `<state>/run/<id>.sock`.
//!
//! Level off runs `$SHELL -lc '…'` directly with the user's own pi config.
//!
//! Every level's env file carries what `@controlroom/pi` needs to reach its session
//! (`CONTROLROOM_SESSION_ID`, `CONTROLROOM_AGENT_SOCKET`, and a fresh `CONTROLROOM_AGENT_TOKEN`
//! that the agent socket's listener checks against `<session>/agent.token`).
//!
//! Signals: the supervisor runs in its own session and ignores SIGHUP, and the agent is the
//! leader of its own process group, so no HUP ever reaches pi (which exits on HUP). Inherited
//! `NONO_*` variables are removed from the whole daemon at startup (see `main`); [`prepare`]
//! refuses to run if one is present anyway, because nono reads flags such as `NONO_ALLOW`
//! from the environment.

use std::collections::BTreeMap;

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
use crate::dirs::{Dirs, create_private_dir, write_private_file};

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
    /// The OAuth helper (`sandbox.toml` `oauth_helper`), when found. Without it, OAuth providers
    /// get no credential route.
    pub oauth_helper: Option<AbsPath>,
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
    let mut env = agent_env(&host.dirs, &launch.session)?;
    let (exec, sandbox) = if launch.policy.level == SandboxLevel::Off {
        store_hook_secret(&host.dirs, None, &launch.session)?;
        let cmd = LaunchCommand::new(None, &host.shell, &launch.cwd, &command);
        (argv(&cmd), None)
    } else {
        let nono = Nono {
            binary: host.nono.clone().ok_or(LaunchError::NoNono)?,
            dirs: NonoDirs::under(host.dirs.state()),
        };
        let mut agent_dir = host.sandbox.agent_dir.clone();
        agent_dir.oauth_helper = host.oauth_helper.clone();
        let built = build_session_dirs(&layout, &agent_dir)?;
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
        store_hook_secret(&host.dirs, hook.as_ref(), &launch.session)?;
        let cmd = LaunchCommand::new(
            Some((&nono, &layout.profile())),
            &host.shell,
            &launch.cwd,
            &command,
        );
        env.extend(cmd.env.iter().cloned());
        env.push(("TMPDIR".to_owned(), host.sandbox.nono_tmp.to_string()));
        let sandbox = PreparedSandbox {
            layout: layout.clone(),
            hook,
            built,
        };
        (argv(&cmd), Some(sandbox))
    };
    let env_file = layout.root().join("launch.env");
    write_env_file(&env_file, &env)?;
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
            paths: supervisor_paths(&host.dirs, &launch.session),
            id,
            spec: SpawnSpec {
                kind: ProcessKind::Agent,
                exec,
                cwd: launch.cwd.as_path().to_owned(),
                env_file: Some(env_file.as_path().to_owned()),
                stdin: StdinMode::Pipe,
                timeout_ms: None,
                max_line_bytes: None,
                labels,
                plain_log: None,
            },
        },
        sandbox,
    })
}

/// Where the supervisor of session `id`'s agent keeps its files: the session dir (transcript,
/// supervisor files) and the short socket path `<state>/run/<id>.sock`.
pub fn supervisor_paths(dirs: &Dirs, id: &SessionId) -> Paths {
    Paths {
        dir: SessionLayout::new(dirs.state(), id)
            .root()
            .as_path()
            .to_owned(),
        socket: dirs.supervisor_socket(id.as_str()).as_path().to_owned(),
    }
}

/// Finds a program from a `sandbox.toml` setting (`nono`, `oauth_helper`): an absolute (or `~/`)
/// path as is, a program name through the login shell; then canonicalized (store paths, not
/// profile links).
pub fn find_program(shell: &AbsPath, setting: &str, home: &AbsPath) -> Result<AbsPath, String> {
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
        read_only: read_only(host),
        relaxed_reads: host.sandbox.relaxed_reads.clone(),
        // nono refuses a missing executable directory.
        exec_dirs: host
            .sandbox
            .exec_dirs
            .iter()
            .filter(|d| d.as_path().is_dir())
            .cloned()
            .collect(),
        // Not the controller socket: @controlroom/pi's own socket for this session only.
        daemon_socket: Some(dirs.agent_socket(launch.session.as_str())),
        protected,
        credential_routes: host
            .sandbox
            .routes
            .iter()
            .filter(|r| has_credential(host, built, r.provider.as_str()))
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

/// The configured read-only paths, plus the `agent/npm` link's target (e.g. the user's installed pi
/// packages, which `controlroom-pi --keep npm:<name>` loads) unless they already cover it.
fn read_only(host: &Host) -> Vec<AbsPath> {
    let mut read_only = host.sandbox.read_only.clone();
    if let Some(npm) = &host.sandbox.agent_dir.npm
        && !read_only.iter().any(|r| r.contains(npm))
    {
        read_only.push(npm.clone());
    }
    read_only
}

/// Whether `provider` has a credential `controlroomd credential` can serve: an API key, or an
/// OAuth token given the helper.
fn has_credential(host: &Host, built: &BuiltAgentDir, provider: &str) -> bool {
    let listed = |providers: &[String]| providers.iter().any(|p| p == provider);
    listed(&built.api_key_providers)
        || (host.oauth_helper.is_some() && listed(&built.oauth_providers))
}

/// The variables `@controlroom/pi` finds its session by: the agent socket and a fresh token,
/// stored for the socket's listener (`<session>/agent.token`) before the agent starts.
fn agent_env(dirs: &Dirs, session: &SessionId) -> Result<Vec<(String, String)>, LaunchError> {
    let token = HookSecret::generate().map_err(|e| LaunchError::Secret(e.to_string()))?;
    let path = dirs.agent_token(session.as_str());
    let io = |source| LaunchError::Io {
        path: path.to_string(),
        source,
    };
    let root = SessionLayout::new(dirs.state(), session).root().clone();
    create_private_dir(&root).map_err(io)?;
    write_private_file(&path, token.as_str().as_bytes()).map_err(io)?;
    Ok(vec![
        (
            "CONTROLROOM_SESSION_ID".to_owned(),
            session.as_str().to_owned(),
        ),
        (
            "CONTROLROOM_AGENT_SOCKET".to_owned(),
            dirs.agent_socket(session.as_str()).to_string(),
        ),
        (
            "CONTROLROOM_AGENT_TOKEN".to_owned(),
            token.as_str().to_owned(),
        ),
    ])
}

/// The agent token of the session's current launch, if it has one.
pub fn load_agent_token(dirs: &Dirs, session: &str) -> Option<HookSecret> {
    let text = std::fs::read_to_string(dirs.agent_token(session).as_path()).ok()?;
    HookSecret::parse(text.trim())
}

/// Writes `KEY=VALUE` lines with mode 0600, replacing the file atomically.
fn write_env_file(path: &AbsPath, vars: &[(String, String)]) -> Result<(), LaunchError> {
    let text: String = vars
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect();
    write_private_file(path, text.as_bytes()).map_err(|source| LaunchError::Io {
        path: path.to_string(),
        source,
    })
}

/// Records the launch's webhook secret for the listener (`<session>/hook.secret`), or removes
/// a previous launch's when this one has no webhook. It must be in place before the agent
/// starts.
fn store_hook_secret(
    dirs: &Dirs,
    hook: Option<&HookUrl>,
    session: &SessionId,
) -> Result<(), LaunchError> {
    let path = dirs.hook_secret(session.as_str());
    let result = match hook {
        Some(hook) => write_private_file(&path, hook.secret.as_str().as_bytes()),
        None => match std::fs::remove_file(path.as_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    };
    result.map_err(|source| LaunchError::Io {
        path: path.to_string(),
        source,
    })
}

/// The webhook secret of the session's current launch, if it has one.
pub fn load_hook_secret(dirs: &Dirs, session: &str) -> Option<HookSecret> {
    let text = std::fs::read_to_string(dirs.hook_secret(session).as_path()).ok()?;
    HookSecret::parse(text.trim())
}

#[cfg(test)]
mod tests;
