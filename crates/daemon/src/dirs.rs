//! Where controlroomd keeps its files (prototype plan §1, spec §14).
//!
//! ```text
//! ~/.local/share/controlroom/versions/<ver>/controlroomd   immutable, versioned binaries
//! ~/.local/share/controlroom/current -> versions/<ver>      swapped by the controller
//! ~/.local/state/controlroom/                               the state dir:
//!     daemon.log                                            stderr of a detached daemon
//!     run/daemon.sock, run/daemon.lock                      the daemon socket and instance lock
//!     run/<id>.sock                                         supervisor sockets
//!     sessions/<id>/                                        transcript, supervisor files, sandbox dirs
//!     nono/{state,config}/                                  nono's own XDG dirs
//! ~/.config/controlroom/{agents.toml,sandbox.toml}          configuration
//! ```
//!
//! Every path derives from `$HOME` alone, so an SSH exec channel, a systemd user unit and a
//! cron job all agree. `$XDG_RUNTIME_DIR` is never used: it is removed at logout when linger is
//! off, and the daemon and its supervisors must outlive the login session. Socket paths stay
//! short (`run/<id>.sock`) because of the 107-byte `sun_path` limit.

use controlroom_sandbox::AbsPath;

/// The daemon's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    home: AbsPath,
    state: AbsPath,
}

/// Why the directories can't be determined.
#[derive(Debug, thiserror::Error)]
pub enum DirsError {
    /// `$HOME` is unset or empty.
    #[error("HOME is not set; pass --state-dir")]
    NoHome,
    /// The `--state-dir` override can't be made absolute.
    #[error("--state-dir: {0}")]
    StateDir(std::io::Error),
    /// A path is not absolute UTF-8.
    #[error(transparent)]
    Path(#[from] controlroom_sandbox::paths::PathError),
}

impl Dirs {
    /// The directories for this process: `$HOME`-based, with an optional state dir override
    /// (`--state-dir`, used by tests and debugging).
    pub fn from_env(state_override: Option<&std::path::Path>) -> Result<Self, DirsError> {
        let home = std::env::var("HOME")
            .ok()
            .filter(|h| !h.is_empty())
            .ok_or(DirsError::NoHome)?;
        let home = AbsPath::new(&home)?;
        let mut dirs = Self::under_home(home);
        if let Some(state) = state_override {
            let state = std::path::absolute(state).map_err(DirsError::StateDir)?;
            dirs.state = AbsPath::from_path(&state)?;
        }
        Ok(dirs)
    }

    /// The default layout under `home`.
    pub fn under_home(home: AbsPath) -> Self {
        Self {
            state: home.join(".local/state/controlroom"),
            home,
        }
    }

    /// The user's home directory.
    pub fn home(&self) -> &AbsPath {
        &self.home
    }

    /// `~/.local/share/controlroom`: installed binaries (managed by the controller).
    pub fn data(&self) -> AbsPath {
        self.home.join(".local/share/controlroom")
    }

    /// `~/.config/controlroom`.
    pub fn config(&self) -> AbsPath {
        self.home.join(".config/controlroom")
    }

    /// `~/.config/controlroom/agents.toml`.
    pub fn agents_toml(&self) -> AbsPath {
        self.config().join("agents.toml")
    }

    /// `~/.config/controlroom/sandbox.toml`.
    pub fn sandbox_toml(&self) -> AbsPath {
        self.config().join("sandbox.toml")
    }

    /// The state dir, `~/.local/state/controlroom` unless overridden.
    pub fn state(&self) -> &AbsPath {
        &self.state
    }

    /// `<state>/run`: sockets and the instance lock.
    pub fn run(&self) -> AbsPath {
        self.state.join("run")
    }

    /// `<state>/run/daemon.sock`.
    pub fn daemon_socket(&self) -> AbsPath {
        self.run().join("daemon.sock")
    }

    /// `<state>/run/daemon.lock`, held by the running daemon.
    pub fn daemon_lock(&self) -> AbsPath {
        self.run().join("daemon.lock")
    }

    /// `<state>/daemon.log`: stderr of a detached daemon.
    pub fn daemon_log(&self) -> AbsPath {
        self.state.join("daemon.log")
    }

    /// `<state>/run/<id>.sock`: a supervisor socket.
    pub fn supervisor_socket(&self, id: &str) -> AbsPath {
        self.run().join(&format!("{id}.sock"))
    }
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
pub fn expand_home(path: &str, home: &AbsPath) -> Result<AbsPath, DirsError> {
    let expanded = match path.strip_prefix('~') {
        Some("") => return Ok(home.clone()),
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_owned(),
    };
    Ok(AbsPath::new(&expanded)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> AbsPath {
        AbsPath::new(s).unwrap()
    }

    #[test]
    fn layout() {
        let d = Dirs::under_home(p("/home/alice"));
        assert_eq!(d.data().as_str(), "/home/alice/.local/share/controlroom");
        assert_eq!(d.state().as_str(), "/home/alice/.local/state/controlroom");
        assert_eq!(
            d.daemon_socket().as_str(),
            "/home/alice/.local/state/controlroom/run/daemon.sock"
        );
        assert_eq!(
            d.agents_toml().as_str(),
            "/home/alice/.config/controlroom/agents.toml"
        );
        let sock = d.supervisor_socket("01HZY3K4M5N6P7Q8R9S0T1V2W3");
        assert!(sock.as_str().len() < controlroom_supervisor::SUN_PATH_MAX);
        assert!(!sock.as_str().contains("/run/user/"));
    }

    #[test]
    fn home_expansion() {
        let home = p("/home/alice");
        assert_eq!(expand_home("~", &home).unwrap(), home);
        assert_eq!(
            expand_home("~/.pi/agent", &home).unwrap().as_str(),
            "/home/alice/.pi/agent"
        );
        assert_eq!(
            expand_home("/nix/store", &home).unwrap().as_str(),
            "/nix/store"
        );
        assert!(expand_home("~bob/x", &home).is_err());
        assert!(expand_home("relative", &home).is_err());
    }
}
