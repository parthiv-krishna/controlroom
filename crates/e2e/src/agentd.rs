//! The real `controlroom-agentd serve` in a scratch directory, with `controlroom-fake-agent`
//! (pi RPC, driver `pi`) as its agent.
//!
//! The agent runs the scenario in `scenario.json` (see [`Agentd::scenario`]); it is read when a
//! session's agent starts, so a test writes it before creating sessions. The daemon can be
//! SIGKILLed and started again on the same state ([`Agentd::kill`], [`Agentd::start`]); its
//! supervisors and agents outlive it, as in production. Dropping it SIGKILLs every process
//! whose command line mentions the scratch directory (daemon, supervisors, agents).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use controlroom_controller::test_support::{fake_agent, wait_until, workspace_binary};
use serde_json::Value;

use crate::procs;

/// A stand-in `$SHELL` for the agent's launch: runs `-lc <command>` with a plain `/bin/sh`, so
/// host profile scripts can't print into transcripts.
const LOGIN_SHELL: &str = "#!/bin/sh\n[ \"$1\" = -lc ] || { echo \"bad shell args: $*\" >&2; exit 99; }\nexec /bin/sh -c \"$2\"\n";

/// The built `controlroom-agentd` (`$CONTROLROOM_AGENTD_BIN` overrides it).
pub fn binary() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        workspace_binary(
            "controlroom-agentd",
            "controlroom-agentd",
            "CONTROLROOM_AGENTD_BIN",
        )
    })
}

/// An agent daemon and its scratch directory.
pub struct Agentd {
    dir: tempfile::TempDir,
    child: Option<Child>,
}

impl Agentd {
    /// Prepares the scratch directory (the agent runs the empty scenario until
    /// [`Agentd::scenario`]); nothing runs until [`Agentd::start`].
    pub fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("cre").tempdir().unwrap();
        controlroom_controller::test_support::write_executable(
            &dir.path().join("login-shell"),
            LOGIN_SHELL.as_bytes(),
        );
        std::fs::create_dir_all(dir.path().join("gates")).unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        let mut agentd = Self { dir, child: None };
        agentd.scenario(&serde_json::json!({}));
        agentd
    }

    /// The scratch directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The state directory.
    pub fn state(&self) -> PathBuf {
        self.path().join("state")
    }

    /// The configuration directory (`agents.toml`, `sandbox.toml`).
    pub fn config(&self) -> PathBuf {
        self.path().join("config")
    }

    /// The agent daemon API socket.
    pub fn socket(&self) -> PathBuf {
        self.state().join("run/agentd.sock")
    }

    /// Makes new sessions' agents run `scenario` (see `controlroom-fake-agent`'s scenario).
    pub fn scenario(&mut self, scenario: &Value) {
        let file = self.path().join("scenario.json");
        std::fs::write(&file, scenario.to_string()).unwrap();
        let command = format!(
            "{} --scenario {} --gate-dir {}",
            fake_agent().display(),
            file.display(),
            self.path().join("gates").display()
        );
        self.agent(&command);
    }

    /// Makes new sessions' agent `command` (driver `pi`).
    pub fn agent(&self, command: &str) {
        std::fs::write(
            self.config().join("agents.toml"),
            format!("[agents.pi]\ncommand = {command:?}\ndriver = \"pi\"\n"),
        )
        .unwrap();
    }

    /// Replaces the stand-in login shell (`$SHELL`) with `program`.
    pub fn login_shell(&self, program: &Path) {
        let shell = self.path().join("login-shell");
        std::fs::remove_file(&shell).unwrap();
        std::os::unix::fs::symlink(program, shell).unwrap();
    }

    /// Starts `controlroom-agentd serve` and waits for its socket.
    pub async fn start(&mut self) {
        assert!(self.child.is_none(), "the agent daemon is already running");
        // The fake agent's directory goes first on `PATH`, as agents must be for nono.
        let agent_dir = fake_agent().parent().unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(agent_dir.to_owned()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path().join("agentd.log"))
            .unwrap();
        let child = Command::new(binary())
            .args(["serve", "--state-dir"])
            .arg(self.state())
            .arg("--config-dir")
            .arg(self.path().join("config"))
            .env("SHELL", self.path().join("login-shell"))
            .env("PATH", path)
            .env("HOME", self.path())
            .env_remove("PI_CODING_AGENT_DIR")
            .env("CONTROLROOM_AGENTD_LOG", "debug")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
        let socket = self.socket();
        wait_until("the agent daemon's socket", || {
            std::os::unix::net::UnixStream::connect(&socket).is_ok()
        })
        .await;
    }

    /// SIGKILLs the daemon; its supervisors and agents keep running.
    pub fn kill(&mut self) {
        let mut child = self.child.take().expect("the agent daemon is running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// Lets an agent pass gate `name`.
    pub fn release(&self, name: &str) {
        std::fs::write(self.path().join(format!("gates/{name}.release")), b"").unwrap();
    }

    /// Waits until an agent reaches gate `name`.
    pub async fn wait_gate(&self, name: &str) {
        let path = self.path().join(format!("gates/{name}.reached"));
        wait_until(&format!("gate {name}"), || path.exists()).await;
    }

    /// The end of the daemon's log, for failure reports.
    pub fn log_tail(&self, lines: usize) -> String {
        let log = std::fs::read_to_string(self.path().join("agentd.log")).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(lines).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

impl Default for Agentd {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Agentd {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        procs::kill_all(&self.path().to_string_lossy());
        if std::thread::panicking() {
            eprintln!("--- agentd.log (last lines) ---\n{}", self.log_tail(80));
        }
    }
}
