//! Where `controlroom-agentd` keeps its files (spec §14, design §2).
//!
//! ```text
//! <state>/                                   the state dir ($STATE_DIRECTORY under systemd)
//!     agent.sqlite                           sessions, queue, timeline, job cursors, ...
//!     agent/                                 pi's agent dir: auth.json (sign-ins),
//!                                            models.json, settings.json, npm/
//!     models/                                Settings › Models: providers.json, keys.json (0600)
//!     run/agentd.lock                        the instance lock
//!     run/<id>.sock                          supervisor sockets
//!     run/<id>.agent.sock                    agent sockets (@controlroom/pi)
//!     sessions/<id>/                         transcript, supervisor files, sandbox dirs
//!     sessions/<id>/work/                    the agent's empty working directory
//!     sessions/<id>/agent.token              the running launch's agent token
//!     nono/{state,config}/                   nono's own XDG dirs
//! <config>/{agents.toml,sandbox.toml}        configuration ($CONFIGURATION_DIRECTORY)
//! <socket>                                   the agent daemon API ($RUNTIME_DIRECTORY/agentd.sock)
//! ```
//!
//! Each location comes from, in order: its flag (`--state-dir`, `--config-dir`, `--socket`),
//! the systemd variable of the service, the installed locations file
//! [`INSTALLED`] (`/etc/controlroom-agent/agentd.toml`, written by the NixOS module, so the CLI
//! run as the service user finds the daemon's files), and only then `$HOME`
//! (`~/.local/state/controlroom-agent`, `~/.config/controlroom-agent`, the socket at
//! `<state>/run/agentd.sock`; development and tests). An unreadable or invalid locations file,
//! or one that contradicts the systemd variables, is an error: a CLI never silently works on
//! other files than the daemon's. Socket paths stay short (`run/<id>.sock`) because of the
//! 107-byte `sun_path` limit.

use std::path::Path;

use controlroom_sandbox::AbsPath;
use serde::Deserialize;

/// The installed locations file. `CONTROLROOM_AGENTD_DIRS` names another one (tests).
pub const INSTALLED: &str = "/etc/controlroom-agent/agentd.toml";

/// The variable naming another installed locations file.
pub const INSTALLED_VAR: &str = "CONTROLROOM_AGENTD_DIRS";

/// The agent daemon's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    state: AbsPath,
    config: AbsPath,
    socket: AbsPath,
}

/// Why the directories can't be determined.
#[derive(Debug, thiserror::Error)]
pub enum DirsError {
    /// Neither the systemd variables nor `$HOME` are set.
    #[error("set HOME or pass --state-dir")]
    NoHome,
    /// An override can't be made absolute.
    #[error("{0}: {1}")]
    Override(&'static str, std::io::Error),
    /// A path is not absolute UTF-8.
    #[error(transparent)]
    Path(#[from] controlroom_sandbox::paths::PathError),
    /// The installed locations file can't be used.
    #[error("{path}: {problem}")]
    Installed {
        /// The file.
        path: String,
        /// What is wrong with it.
        problem: String,
    },
    /// The systemd variables and the installed locations file disagree.
    #[error("{var}={from_env}, but {file} says {from_file}; fix the file or the service")]
    Contradiction {
        /// The variable.
        var: &'static str,
        /// Its value.
        from_env: String,
        /// The installed locations file.
        file: String,
        /// Its value.
        from_file: String,
    },
}

/// Explicit locations (command-line flags); `None` picks the default.
#[derive(Debug, Clone, Copy, Default)]
pub struct Overrides<'a> {
    /// `--state-dir`.
    pub state: Option<&'a Path>,
    /// `--config-dir`.
    pub config: Option<&'a Path>,
    /// `--socket`.
    pub socket: Option<&'a Path>,
}

/// The installed locations file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    /// The state dir.
    pub state_dir: String,
    /// The configuration dir.
    pub config_dir: String,
    /// The agent daemon API socket.
    pub socket: String,
}

/// What [`Dirs::resolve`] reads from the environment.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `$STATE_DIRECTORY`.
    pub state: Option<String>,
    /// `$CONFIGURATION_DIRECTORY`.
    pub config: Option<String>,
    /// `$RUNTIME_DIRECTORY`.
    pub runtime: Option<String>,
    /// `$HOME`.
    pub home: Option<String>,
    /// The installed locations file, read: its path and contents (`None` when absent).
    pub installed: Option<(String, Result<String, String>)>,
}

impl Env {
    /// This process's environment and installed locations file.
    pub fn current() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let path = var(INSTALLED_VAR).unwrap_or_else(|| INSTALLED.to_owned());
        let installed = match std::fs::read_to_string(&path) {
            Ok(text) => Some((path, Ok(text))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some((path, Err(e.to_string()))),
        };
        Self {
            state: var("STATE_DIRECTORY"),
            config: var("CONFIGURATION_DIRECTORY"),
            runtime: var("RUNTIME_DIRECTORY"),
            home: var("HOME"),
            installed,
        }
    }
}

impl Dirs {
    /// The directories for this process (see the module docs).
    pub fn from_env(overrides: Overrides<'_>) -> Result<Self, DirsError> {
        Self::resolve(overrides, &Env::current())
    }

    /// The directories for `overrides` in `env` (see the module docs).
    pub fn resolve(overrides: Overrides<'_>, env: &Env) -> Result<Self, DirsError> {
        let absolute = |what: &'static str, path: &Path| -> Result<AbsPath, DirsError> {
            let path = std::path::absolute(path).map_err(|e| DirsError::Override(what, e))?;
            Ok(AbsPath::from_path(&path)?)
        };
        let from_home = |rest: &str| -> Result<AbsPath, DirsError> {
            let home = env.home.as_deref().ok_or(DirsError::NoHome)?;
            Ok(AbsPath::new(home)?.join(rest))
        };
        let installed = match &env.installed {
            None => None,
            Some((path, text)) => Some((path.as_str(), parse_installed(path, text)?)),
        };
        let systemd =
            |var: &'static str, value: &Option<String>, suffix: &str, file: Option<&str>| {
                let Some(value) = value else { return Ok(None) };
                let dir = AbsPath::new(first(value))?;
                let path = if suffix.is_empty() {
                    dir
                } else {
                    dir.join(suffix)
                };
                if let (Some((name, _)), Some(from_file)) = (installed.as_ref(), file)
                    && path.as_str() != from_file
                {
                    return Err(DirsError::Contradiction {
                        var,
                        from_env: path.to_string(),
                        file: (*name).to_owned(),
                        from_file: from_file.to_owned(),
                    });
                }
                Ok(Some(path))
            };
        let from_file = |pick: fn(&Installed) -> &str| -> Result<Option<AbsPath>, DirsError> {
            installed
                .as_ref()
                .map(|(_, i)| AbsPath::new(pick(i)).map_err(DirsError::from))
                .transpose()
        };
        let file = |pick: fn(&Installed) -> &str| installed.as_ref().map(|(_, i)| pick(i));

        let state_env = systemd("STATE_DIRECTORY", &env.state, "", file(|i| &i.state_dir))?;
        let state = match (overrides.state, state_env) {
            (Some(dir), _) => absolute("--state-dir", dir)?,
            (None, Some(dir)) => dir,
            (None, None) => match from_file(|i| &i.state_dir)? {
                Some(dir) => dir,
                None => from_home(".local/state/controlroom-agent")?,
            },
        };
        let config_env = systemd(
            "CONFIGURATION_DIRECTORY",
            &env.config,
            "",
            file(|i| &i.config_dir),
        )?;
        let config = match (overrides.config, config_env) {
            (Some(dir), _) => absolute("--config-dir", dir)?,
            (None, Some(dir)) => dir,
            (None, None) if overrides.state.is_some() => state.join("config"),
            (None, None) => match from_file(|i| &i.config_dir)? {
                Some(dir) => dir,
                None => from_home(".config/controlroom-agent")?,
            },
        };
        let socket_env = systemd(
            "RUNTIME_DIRECTORY",
            &env.runtime,
            "agentd.sock",
            file(|i| &i.socket),
        )?;
        let socket = match (overrides.socket, socket_env) {
            (Some(path), _) => absolute("--socket", path)?,
            (None, Some(path)) => path,
            (None, None) if overrides.state.is_some() => state.join("run/agentd.sock"),
            (None, None) => match from_file(|i| &i.socket)? {
                Some(path) => path,
                None => state.join("run/agentd.sock"),
            },
        };
        Ok(Self {
            state,
            config,
            socket,
        })
    }

    /// A layout with everything under `state` (tests).
    pub fn under(state: AbsPath) -> Self {
        Self {
            config: state.join("config"),
            socket: state.join("run/agentd.sock"),
            state,
        }
    }

    /// The state dir.
    pub fn state(&self) -> &AbsPath {
        &self.state
    }

    /// The configuration dir.
    pub fn config(&self) -> &AbsPath {
        &self.config
    }

    /// The agent daemon API socket.
    pub fn socket(&self) -> &AbsPath {
        &self.socket
    }

    /// `<config>/agents.toml`.
    pub fn agents_toml(&self) -> AbsPath {
        self.config.join("agents.toml")
    }

    /// `<config>/sandbox.toml`.
    pub fn sandbox_toml(&self) -> AbsPath {
        self.config.join("sandbox.toml")
    }

    /// `<state>/agent`: pi's agent dir (credentials, models, settings).
    pub fn pi_agent_dir(&self) -> AbsPath {
        self.state.join("agent")
    }

    /// `<state>/models`: what Settings › Models manages (providers, keys).
    pub fn models(&self) -> AbsPath {
        self.state.join("models")
    }

    /// `<state>/agent.sqlite`.
    pub fn database(&self) -> AbsPath {
        self.state.join("agent.sqlite")
    }

    /// `<state>/run`: sockets and the instance lock.
    pub fn run(&self) -> AbsPath {
        self.state.join("run")
    }

    /// `<state>/run/agentd.lock`, held by the running agent daemon.
    pub fn lock(&self) -> AbsPath {
        self.run().join("agentd.lock")
    }

    /// `<state>/run/<id>.sock`: a supervisor socket.
    pub fn supervisor_socket(&self, id: &str) -> AbsPath {
        self.run().join(&format!("{id}.sock"))
    }

    /// `<state>/run/<id>.agent.sock`: the session's agent socket, which its sandbox may reach.
    pub fn agent_socket(&self, id: &str) -> AbsPath {
        self.run().join(&format!("{id}.agent.sock"))
    }

    /// `<state>/sessions/<id>/agent.token`: the agent token of the session's current launch.
    pub fn agent_token(&self, id: &str) -> AbsPath {
        self.state.join("sessions").join(id).join("agent.token")
    }

    /// `<state>/sessions/<id>/work`: the agent's (empty) working directory.
    pub fn workdir(&self, id: &str) -> AbsPath {
        self.state.join("sessions").join(id).join("work")
    }
}

/// Parses the installed locations file; every path must be absolute.
fn parse_installed(path: &str, text: &Result<String, String>) -> Result<Installed, DirsError> {
    let problem = |problem: String| DirsError::Installed {
        path: path.to_owned(),
        problem,
    };
    let text = text.as_ref().map_err(|e| problem(e.clone()))?;
    let installed: Installed = toml::from_str(text).map_err(|e| problem(e.message().to_owned()))?;
    for value in [
        &installed.state_dir,
        &installed.config_dir,
        &installed.socket,
    ] {
        AbsPath::new(value).map_err(|e| problem(e.to_string()))?;
    }
    Ok(installed)
}

/// systemd joins several directories with `:`; the first one is ours.
fn first(dirs: &str) -> &str {
    dirs.split(':').next().unwrap_or(dirs)
}

/// Writes `bytes` to `path` with mode 0600, replacing the file atomically (a temporary file
/// next to it, fsynced, then renamed).
pub fn write_private_file(path: &AbsPath, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let name = path
        .as_path()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let tmp = path.as_path().with_file_name(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path.as_path())
}

/// Creates `dir` and its missing parents with mode 0700.
pub fn create_private_dir(dir: &AbsPath) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.as_path())
}

/// Expands a leading `~/` (or a lone `~`) against `home` and checks the result is absolute.
pub fn expand_home(path: &str, home: Option<&AbsPath>) -> Result<AbsPath, DirsError> {
    let expanded = match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => return Ok(home.clone()),
        (Some(rest), Some(home)) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_owned(),
    };
    Ok(AbsPath::new(&expanded)?)
}

/// `$HOME`, when set.
pub fn home() -> Option<AbsPath> {
    std::env::var("HOME")
        .ok()
        .and_then(|h| AbsPath::new(&h).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> AbsPath {
        AbsPath::new(s).unwrap()
    }

    #[test]
    fn layout() {
        let d = Dirs::under(p("/var/lib/controlroom-agent"));
        assert_eq!(
            d.database().as_str(),
            "/var/lib/controlroom-agent/agent.sqlite"
        );
        assert_eq!(
            d.pi_agent_dir().as_str(),
            "/var/lib/controlroom-agent/agent"
        );
        assert_eq!(
            d.agents_toml().as_str(),
            "/var/lib/controlroom-agent/config/agents.toml"
        );
        let id = "01HZY3K4M5N6P7Q8R9S0T1V2W3";
        assert!(d.supervisor_socket(id).as_str().len() < controlroom_supervisor::SUN_PATH_MAX);
        assert!(d.agent_socket(id).as_str().len() < controlroom_supervisor::SUN_PATH_MAX);
        assert_eq!(
            d.workdir(id).as_str(),
            format!("/var/lib/controlroom-agent/sessions/{id}/work")
        );
        assert_eq!(first("/a:/b"), "/a");
    }

    fn env(installed: Option<&str>) -> Env {
        Env {
            home: Some("/var/lib/controlroom-agent".into()),
            installed: installed
                .map(|t| ("/etc/controlroom-agent/agentd.toml".into(), Ok(t.into()))),
            ..Env::default()
        }
    }

    const INSTALLED_TOML: &str = "state_dir = \"/var/lib/controlroom-agent\"\nconfig_dir = \"/etc/controlroom-agent\"\nsocket = \"/run/controlroom-agent/agentd.sock\"\n";

    #[test]
    fn the_cli_finds_the_installed_locations() {
        // `sudo -u controlroom-agent -i controlroom-agentd login`: no systemd variables, HOME
        // is the service user's, and the installed file names the daemon's directories.
        let d = Dirs::resolve(Overrides::default(), &env(Some(INSTALLED_TOML))).unwrap();
        assert_eq!(d.state().as_str(), "/var/lib/controlroom-agent");
        assert_eq!(d.config().as_str(), "/etc/controlroom-agent");
        assert_eq!(d.socket().as_str(), "/run/controlroom-agent/agentd.sock");
        assert_eq!(
            d.pi_agent_dir().as_str(),
            "/var/lib/controlroom-agent/agent"
        );

        // Without it, HOME (development).
        let d = Dirs::resolve(Overrides::default(), &env(None)).unwrap();
        assert_eq!(
            d.state().as_str(),
            "/var/lib/controlroom-agent/.local/state/controlroom-agent"
        );

        // Flags still win.
        let state = Path::new("/srv/agentd");
        let d = Dirs::resolve(
            Overrides {
                state: Some(state),
                ..Overrides::default()
            },
            &env(Some(INSTALLED_TOML)),
        )
        .unwrap();
        assert_eq!(d.config().as_str(), "/srv/agentd/config");
        assert_eq!(d.socket().as_str(), "/srv/agentd/run/agentd.sock");
    }

    #[test]
    fn a_wrong_location_is_rejected() {
        let bad = |text: &str| Dirs::resolve(Overrides::default(), &env(Some(text))).unwrap_err();
        assert!(matches!(bad("state_dir = 1"), DirsError::Installed { .. }));
        assert!(matches!(
            bad(&INSTALLED_TOML.replace("/var/lib", "var/lib")),
            DirsError::Installed { .. }
        ));
        let unreadable = Env {
            installed: Some(("/etc/x.toml".into(), Err("permission denied".into()))),
            ..env(None)
        };
        assert!(Dirs::resolve(Overrides::default(), &unreadable).is_err());

        // The service and the file must agree.
        let service = Env {
            state: Some("/var/lib/controlroom-agent".into()),
            config: Some("/etc/controlroom-agent".into()),
            runtime: Some("/run/controlroom-agent".into()),
            ..env(Some(INSTALLED_TOML))
        };
        assert!(Dirs::resolve(Overrides::default(), &service).is_ok());
        let moved = Env {
            state: Some("/var/lib/elsewhere".into()),
            ..service
        };
        let err = Dirs::resolve(Overrides::default(), &moved).unwrap_err();
        assert!(err.to_string().contains("STATE_DIRECTORY"), "{err}");
    }

    #[test]
    fn home_expansion() {
        let home = p("/home/alice");
        assert_eq!(expand_home("~", Some(&home)).unwrap(), home);
        assert_eq!(
            expand_home("~/.pi/agent", Some(&home)).unwrap().as_str(),
            "/home/alice/.pi/agent"
        );
        assert!(expand_home("~/x", None).is_err());
        assert!(expand_home("relative", Some(&home)).is_err());
    }
}
