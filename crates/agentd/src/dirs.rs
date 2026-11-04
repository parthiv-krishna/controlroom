//! Where `controlroom-agentd` keeps its files (spec §14, design §2).
//!
//! ```text
//! <state>/                                   the state dir ($STATE_DIRECTORY under systemd)
//!     agent.sqlite                           sessions, queue, timeline, job cursors, ...
//!     agent/                                 pi's agent dir: auth.json (from `login`),
//!                                            models.json, settings.json, npm/
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
//! Without the systemd variables (development, tests) everything derives from `$HOME`:
//! `~/.local/state/controlroom-agent`, `~/.config/controlroom-agent`, and the socket at
//! `<state>/run/agentd.sock`. Socket paths stay short (`run/<id>.sock`) because of the 107-byte
//! `sun_path` limit.

use std::path::Path;

use controlroom_sandbox::AbsPath;

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

impl Dirs {
    /// The directories for this process (see the module docs).
    pub fn from_env(overrides: Overrides<'_>) -> Result<Self, DirsError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let home = var("HOME");
        let absolute = |what: &'static str, path: &Path| -> Result<AbsPath, DirsError> {
            let path = std::path::absolute(path).map_err(|e| DirsError::Override(what, e))?;
            Ok(AbsPath::from_path(&path)?)
        };
        let from_home = |rest: &str| -> Result<AbsPath, DirsError> {
            let home = home.as_deref().ok_or(DirsError::NoHome)?;
            Ok(AbsPath::new(home)?.join(rest))
        };
        let state = match (overrides.state, var("STATE_DIRECTORY")) {
            (Some(dir), _) => absolute("--state-dir", dir)?,
            (None, Some(dir)) => AbsPath::new(first(&dir))?,
            (None, None) => from_home(".local/state/controlroom-agent")?,
        };
        let config = match (overrides.config, var("CONFIGURATION_DIRECTORY")) {
            (Some(dir), _) => absolute("--config-dir", dir)?,
            (None, Some(dir)) => AbsPath::new(first(&dir))?,
            (None, None) if overrides.state.is_some() => state.join("config"),
            (None, None) => from_home(".config/controlroom-agent")?,
        };
        let socket = match (overrides.socket, var("RUNTIME_DIRECTORY")) {
            (Some(path), _) => absolute("--socket", path)?,
            (None, Some(dir)) => AbsPath::new(first(&dir))?.join("agentd.sock"),
            (None, None) => state.join("run/agentd.sock"),
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
