//! The whole topology of one test: the controller (in process, next UI API), the real agent
//! daemon, and two real host daemons ("devbox", "buildbox") reached over SSH through relays
//! that can cut their links.

use std::path::{Path, PathBuf};
use std::time::Duration;

use controlroom_client_core::ssh::ConnectOptions;
use controlroom_controller::test_support::{
    Relay, RemoteHost, UiDriver, bundled_daemons, controlroomd, wait_until,
};
use controlroom_controller::{
    AuthPolicy, Backoff, Controller, ControllerConfig, Identity, MasterKey, Tuning, UiApi,
};
use controlroom_protocol::ui::AuthMode;
use serde_json::json;

use crate::agentd::Agentd;
use crate::procs;

/// A machine: an SSH server with its own `$HOME`, where the controller installs and runs the
/// real host daemon, behind a relay.
pub struct Host {
    /// The machine name the agent uses (`devbox:path`).
    pub name: &'static str,
    /// The machine id, once added.
    pub id: String,
    /// The relay in front of the SSH server: [`Relay::down`] cuts the link.
    pub relay: Relay,
    remote: RemoteHost,
}

impl Host {
    async fn start(name: &'static str) -> Self {
        let remote = RemoteHost::start().await;
        let relay = Relay::start(remote.port()).await;
        Self {
            name,
            id: String::new(),
            relay,
            remote,
        }
    }

    /// The remote `$HOME`.
    pub fn home(&self) -> &Path {
        self.remote.home()
    }

    /// A directory under the remote home, created if missing.
    pub fn dir(&self, rel: &str) -> PathBuf {
        let dir = self.home().join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The host daemon's state directory.
    pub fn state(&self) -> PathBuf {
        self.home().join(".local/state/controlroom")
    }

    /// SIGKILLs the host daemon (`controlroomd serve`); shells and jobs keep running.
    pub fn kill_daemon(&self) {
        let home = self.home().to_string_lossy().into_owned();
        let pids = procs::find(&[&home, "controlroomd serve"]);
        assert!(!pids.is_empty(), "{}: no host daemon is running", self.name);
        procs::kill(&pids);
    }

    /// "Reboots" the machine: SIGKILLs every process of this host (the daemon, its bridge,
    /// shells and jobs); the next connection starts a fresh daemon.
    pub fn reboot(&self) {
        procs::kill_all(&self.home().to_string_lossy());
    }

    /// Sets an environment variable of the remote user's commands (and so of the host daemon).
    pub fn set_env(&self, name: &str, value: &str) {
        self.remote.server().set_env(name, value);
    }

    /// Types `text` into a shell's terminal the way a user does: `controlroomd pty attach
    /// <shell id>` as the remote user (the UI runs it over an SSH exec channel).
    pub async fn type_into(&self, shell_id: &str, text: &str) {
        use std::io::Write as _;
        let mut attach = std::process::Command::new(controlroomd())
            .args(["pty", "attach", shell_id])
            .env("HOME", self.home())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        attach
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let status = tokio::task::spawn_blocking(move || attach.wait().unwrap())
            .await
            .unwrap();
        assert!(status.success(), "pty attach failed: {status}");
    }

    /// The pids of this host's processes whose command line also contains `needle`.
    pub fn processes(&self, needle: &str) -> Vec<i32> {
        self.remote.processes(needle)
    }
}

/// The controller, the agent daemon, two hosts and one browser tab.
pub struct World {
    /// A UI connection (next API) with the state feed mirrored.
    pub ui: UiDriver,
    /// The controller, in process.
    pub controller: Controller,
    /// The agent daemon.
    pub agentd: Agentd,
    /// The first machine.
    pub devbox: Host,
    /// The second machine.
    pub buildbox: Host,
    store: tempfile::TempDir,
    bundled: tempfile::TempDir,
}

/// Fast reconnects and keepalives for tests.
fn tuning() -> Tuning {
    Tuning {
        ssh: ConnectOptions {
            connect_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(5),
            keepalive_max: 3,
        },
        backoff: Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(300),
        },
        // Never upgrade automatically: the bundled daemon is the installed one.
        auto_upgrade_retry: Backoff {
            initial: Duration::from_secs(3600),
            max: Duration::from_secs(3600),
        },
        ..Tuning::default()
    }
}

impl World {
    /// Starts everything and adds both machines; returns once they are connected hosts.
    pub async fn start() -> Self {
        Self::start_with(|_, _, _| {}).await
    }

    /// Like [`World::start`], letting `configure` set up the agent daemon and the hosts before
    /// anything runs (e.g. real pi and nono).
    pub async fn start_with(configure: impl FnOnce(&mut Agentd, &Host, &Host)) -> Self {
        let mut agentd = Agentd::new();
        let (devbox, buildbox) = (Host::start("devbox").await, Host::start("buildbox").await);
        configure(&mut agentd, &devbox, &buildbox);
        agentd.start().await;
        let bundled = bundled_daemons();
        let store = tempfile::tempdir().unwrap();
        let controller = start_controller(store.path(), bundled.path(), &agentd.socket());
        let ui = open_ui(&controller).await;
        let mut world = Self {
            ui,
            controller,
            agentd,
            devbox,
            buildbox,
            store,
            bundled,
        };
        world.devbox.id = add_machine(&mut world.ui, &world.devbox).await;
        world.buildbox.id = add_machine(&mut world.ui, &world.buildbox).await;
        world
    }

    /// Restarts the controller on the same store (a web restart): every connection closes,
    /// then the new controller reconnects to the agent daemon and the hosts, and the browser
    /// tab connects again. Returns once both machines are connected.
    pub async fn restart_controller(&mut self) {
        self.controller.shutdown().await;
        let controller = start_controller(
            self.store.path(),
            self.bundled.path(),
            &self.agentd.socket(),
        );
        self.ui = open_ui(&controller).await;
        self.controller = controller;
        for id in [self.devbox.id.clone(), self.buildbox.id.clone()] {
            self.wait_connected(&id).await;
        }
    }

    /// Waits until machine `id` is connected.
    pub async fn wait_connected(&mut self, id: &str) {
        self.ui
            .wait("the machine connects", |m| {
                m.machine(id)["connectivity"]["state"] == "connected"
            })
            .await;
    }

    /// Waits until machine `id` is no longer connected.
    pub async fn wait_disconnected(&mut self, id: &str) {
        self.ui
            .wait("the machine drops", |m| {
                m.machine(id)["connectivity"]["state"] != "connected"
            })
            .await;
    }

    /// Cuts `host`'s link and waits until the controller sees it down.
    pub async fn cut(&mut self, host: Which) {
        let id = self.host(host).id.clone();
        self.host(host).relay.down();
        self.wait_disconnected(&id).await;
    }

    /// Restores `host`'s link and waits until it is connected again.
    pub async fn restore(&mut self, host: Which) {
        let id = self.host(host).id.clone();
        self.host(host).relay.up();
        self.wait_connected(&id).await;
    }

    /// One of the two hosts.
    pub fn host(&self, which: Which) -> &Host {
        match which {
            Which::Devbox => &self.devbox,
            Which::Buildbox => &self.buildbox,
        }
    }

    /// Restarts the agent daemon (SIGKILL, then a fresh start on the same state); the
    /// controller reconnects to it.
    pub async fn restart_agentd(&mut self) {
        self.agentd.kill();
        self.agentd.start().await;
    }
}

/// Adds `host` through its relay (authorizing the controlroom key first); returns the machine
/// id once it is connected.
async fn add_machine(ui: &mut UiDriver, host: &Host) -> String {
    let key = ui.ok("keys.list", json!({})).await["controlroom"]["public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    host.remote.authorize(&key);
    let mut new = host.remote.new_machine(host.name);
    new["port"] = json!(host.relay.port);
    let added = ui.ok("machines.add", new.clone()).await;
    let mut confirm = new;
    confirm["host_key_fingerprint"] = added["host_key"]["fingerprint"].clone();
    let result = ui.ok("machines.add_confirm", confirm).await;
    assert_eq!(result["outcome"], "added", "{result}");
    let id = result["machine"]["id"].as_str().unwrap().to_owned();
    ui.wait("the machine connects", |m| {
        m.machine(&id)["connectivity"]["state"] == "connected"
    })
    .await;
    id
}

/// Names one of the [`World`]'s hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// "devbox".
    Devbox,
    /// "buildbox".
    Buildbox,
}

/// Waits until `path` exists (e.g. a marker a remote command writes).
pub async fn wait_file(path: &Path) {
    wait_until(&format!("{}", path.display()), || path.exists()).await;
}

/// Waits until the file at `path` contains `needle`.
pub async fn wait_contains(path: &Path, needle: &str) {
    wait_until(&format!("{needle:?} in {}", path.display()), || {
        std::fs::read_to_string(path).is_ok_and(|t| t.contains(needle))
    })
    .await;
}

/// A controller on the store in `store`, installing the daemons in `bundled`, linked to the
/// agent daemon at `agentd`.
fn start_controller(store: &Path, bundled: &Path, agentd: &Path) -> Controller {
    Controller::start(ControllerConfig {
        store_path: store.join("controller.sqlite"),
        master_key: MasterKey::from_bytes([7; 32]),
        bundled_daemons_dir: Some(bundled.to_owned()),
        server_version: "0.0.0-e2e".into(),
        auth: AuthPolicy {
            mode: AuthMode::DevToken,
            allowed_subjects: vec![],
            allowed_groups: vec![],
        },
        push: None,
        key_comment: "controlroom@controlroom.example.com".into(),
        agentd_socket: Some(agentd.to_owned()),
        tuning: tuning(),
    })
    .unwrap()
}

/// A browser tab on the next UI API (device `laptop`), with the state feed mirrored.
async fn open_ui(controller: &Controller) -> UiDriver {
    let principal = controller.resolve(&Identity::DevToken).unwrap();
    let client = controller
        .open_trusted_client_with(principal, UiApi::Next)
        .unwrap();
    let mut ui = UiDriver::new(client);
    ui.hello("laptop", "laptop (Firefox on Linux)").await;
    ui.subscribe_state().await;
    ui
}
