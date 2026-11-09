//! Agent launch glue: from "start pi for this session" to a running, detached supervisor
//! (spec §6.1, §11, design §4). The runtime's agents port calls [`prepare`].
//!
//! pi runs on the server with no project files: its working directory is the session's empty
//! scratch directory (`sessions/<id>/work`), and it starts with `--no-builtin-tools
//! --no-context-files --no-skills`, so every tool it has is @controlroom/pi's remote one and
//! its project context comes from the workspaces (`session.info`).
//!
//! For levels strict and relaxed, [`prepare`]:
//!
//! 1. builds the session dirs from the agent daemon's pi agent dir (`agent/` with fake
//!    credentials, `home/`, `tmp/`) and finds the API-key and OAuth providers;
//! 2. assembles the [`SessionSandboxSpec`]: the scratch directory, read access to the configured
//!    system paths and the `agent/npm` link's target, the session's agent socket, credential
//!    routes for the providers that have a key (or an OAuth token, given the OAuth helper), and
//!    the protected agent daemon paths. No grants, domains or mediated commands: those are the
//!    workspaces' (their hosts apply them), and pi runs no commands here;
//! 3. generates, checks and writes `profile.json`;
//! 4. builds `nono run … -- $SHELL -lc 'exec <command> <args>'` and an env file with nono's own
//!    environment and a short `TMPDIR` (nono binds sockets there; `sun_path`);
//! 5. returns the supervisor [`LaunchRequest`]: the session dir holds the transcript, the
//!    socket is `<state>/run/<id>.sock`.
//!
//! Level off runs `$SHELL -lc '…'` directly with the agent dir as is.
//!
//! Every level's env file carries what @controlroom/pi needs to reach its session
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
    AgentDirError, AgentDirSpec, BuiltAgentDir, build_session_dirs, write_profile,
};
use controlroom_sandbox::launch::{LaunchCommand, NONO_ENV_PREFIX, Nono, NonoDirs, shell_quote};
use controlroom_sandbox::policy::{EffectivePolicy, SandboxLevel};
use controlroom_sandbox::spec::CredentialHelper;
use controlroom_sandbox::webhook::HookSecret;
use controlroom_sandbox::{
    AbsPath, ProfileError, SessionId, SessionLayout, SessionSandboxSpec, generate_profile,
};
use controlroom_supervisor::{Exec, LaunchRequest, Paths, SpawnSpec};
use controlroom_transcript::{ProcessKind, StdinMode};

use crate::config::agents::AgentEntry;
use crate::config::sandbox::SandboxSettings;
use crate::dirs::{Dirs, create_private_dir, write_private_file};

/// pi's flags for remote tools: no local tools, context files or skills.
pub const REMOTE_TOOLS_ARGS: [&str; 3] =
    ["--no-builtin-tools", "--no-context-files", "--no-skills"];

/// Server-level inputs of every launch.
#[derive(Debug, Clone)]
pub struct Host {
    /// The agent daemon's directories.
    pub dirs: Dirs,
    /// This `controlroom-agentd`: runs supervisors and is nono's credential helper.
    pub agentd: AbsPath,
    /// The login shell agents run in.
    pub shell: AbsPath,
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
    /// The `agents.toml` key, recorded in the transcript's meta labels.
    pub agent_name: String,
    /// The agent entry.
    pub agent: AgentEntry,
    /// The driver's arguments for this start (`Driver::launch_args`).
    pub args: Vec<String>,
    /// The level the agent runs at.
    pub level: SandboxLevel,
}

/// A launch ready to start.
#[derive(Debug, Clone)]
pub struct PreparedLaunch {
    /// What the supervisor runs.
    pub request: LaunchRequest,
    /// What the agent dir build found, unless the level is off.
    pub built: Option<BuiltAgentDir>,
}

/// Why a launch could not be prepared.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// A sandboxed level was requested but nono is unavailable.
    #[error(
        "nono is not available, so sandboxed sessions can't start (see `controlroom-agentd doctor`)"
    )]
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
    /// Generating the agent token failed.
    #[error("agent token: {0}")]
    Secret(String),
    /// Writing the env file or creating a directory failed.
    #[error("writing {path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// The error.
        #[source]
        source: std::io::Error,
    },
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
    let workdir = host.dirs.workdir(launch.session.as_str());
    create_private_dir(&workdir).map_err(|source| LaunchError::Io {
        path: workdir.to_string(),
        source,
    })?;
    let mut env = agent_env(&host.dirs, &launch.session)?;
    let (exec, built) = if launch.level == SandboxLevel::Off {
        if let Some(dir) = &host.sandbox.agent_dir.user_agent_dir {
            env.push(("PI_CODING_AGENT_DIR".to_owned(), dir.to_string()));
        }
        let cmd = LaunchCommand::new(None, &host.shell, &workdir, &command);
        (argv(&cmd), None)
    } else {
        let nono = Nono {
            binary: host.nono.clone().ok_or(LaunchError::NoNono)?,
            dirs: NonoDirs::under(host.dirs.state()),
        };
        let agent_dir = AgentDirSpec {
            oauth_helper: host.oauth_helper.clone(),
            ..host.sandbox.agent_dir.clone()
        };
        let built = build_session_dirs(&layout, &agent_dir)?;
        let spec = sandbox_spec(host, launch, &layout, &workdir, &built);
        write_profile(&layout, &generate_profile(&spec)?)?;
        let cmd = LaunchCommand::new(
            Some((&nono, &layout.profile())),
            &host.shell,
            &workdir,
            &command,
        );
        env.extend(cmd.env.iter().cloned());
        env.push(("TMPDIR".to_owned(), host.sandbox.nono_tmp.to_string()));
        (argv(&cmd), Some(built))
    };
    let env_file = layout.root().join("launch.env");
    write_env_file(&env_file, &env)?;
    let run = host.dirs.run();
    create_private_dir(&run).map_err(|source| LaunchError::Io {
        path: run.to_string(),
        source,
    })?;
    let labels = BTreeMap::from([
        ("agent".to_owned(), launch.agent_name.clone()),
        (
            "driver".to_owned(),
            driver_label(launch.agent.driver).to_owned(),
        ),
        ("sandbox".to_owned(), level_label(launch.level).to_owned()),
    ]);
    Ok(PreparedLaunch {
        request: LaunchRequest {
            paths: supervisor_paths(&host.dirs, &launch.session),
            id: launch.session.as_str().to_owned(),
            spec: SpawnSpec {
                kind: ProcessKind::Agent,
                exec,
                cwd: workdir.as_path().to_owned(),
                env_file: Some(env_file.as_path().to_owned()),
                stdin: StdinMode::Pipe,
                timeout_ms: None,
                max_line_bytes: None,
                labels,
                plain_log: None,
            },
        },
        built,
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

/// `exec <command> <remote tools flags> <args…>`: the login shell replaces itself with pi.
fn agent_command(agent: &AgentEntry, args: &[String]) -> String {
    let mut command = format!("exec {}", agent.command.trim());
    let flags = REMOTE_TOOLS_ARGS.iter().map(|f| (*f).to_owned());
    for arg in flags.chain(args.iter().cloned()) {
        command.push(' ');
        command.push_str(&shell_quote(&arg));
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
    workdir: &AbsPath,
    built: &BuiltAgentDir,
) -> SessionSandboxSpec {
    let dirs = &host.dirs;
    let mut protected = vec![dirs.state().clone(), dirs.config().clone()];
    protected.extend(host.sandbox.agent_dir.user_agent_dir.clone());
    SessionSandboxSpec {
        layout: layout.clone(),
        cwd: workdir.clone(),
        policy: EffectivePolicy {
            level: launch.level,
            paths: Vec::new(),
            domains: Vec::new(),
            commands: Vec::new(),
        },
        read_only: read_only(host),
        relaxed_reads: Vec::new(),
        exec_dirs: Vec::new(),
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
            controlroomd: host.agentd.clone(),
            state_dir: dirs.state().clone(),
        },
        approvals: None,
        pins: BTreeMap::new(),
    }
}

/// The configured read-only paths, plus the `agent/npm` link's target (e.g. installed pi
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

/// Whether `provider` has a credential `controlroom-agentd credential` can serve: an API key
/// (the server's, or one stored in Settings › Models), or an OAuth token given the helper.
fn has_credential(host: &Host, built: &BuiltAgentDir, provider: &str) -> bool {
    let listed = |providers: &[String]| providers.iter().any(|p| p == provider);
    listed(&built.api_key_providers)
        || listed(&built.stored_key_providers)
        || (host.oauth_helper.is_some() && listed(&built.oauth_providers))
}

/// The variables @controlroom/pi finds its session by: the agent socket and a fresh token,
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

/// Finds a program from a setting (`nono`, `oauth_helper`, an agent command): an absolute path
/// as is, a program name through the login shell; then canonicalized (store paths, not profile
/// links).
pub fn find_program(shell: &AbsPath, setting: &str) -> Result<AbsPath, String> {
    let path = if setting.starts_with('/') {
        setting.to_owned()
    } else {
        crate::shell::resolve_program(shell, setting)
            .map_err(|e| e.describe(shell.as_str()))?
            .ok_or_else(|| format!("`{setting}` not found by the login shell ({shell} -lc)"))?
    };
    let real = std::fs::canonicalize(&path).map_err(|e| format!("{path}: {e}"))?;
    AbsPath::from_path(&real).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
