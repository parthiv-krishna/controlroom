//! Test harness (feature `test-support`, Unix only): fake execution machines, the built
//! daemon, and a UI client driver, for this crate's and the web shell's end-to-end tests.
//!
//! - [`RemoteHost`] is client-core's in-process SSH server with a scratch `$HOME`, a plain
//!   `/bin/sh` login shell and an `agents.toml` that runs `controlroom-fake-agent` (pi RPC)
//!   scenarios with gate steps. Dropping it SIGKILLs every process whose command line mentions
//!   its home (the daemon that `controlroomd connect` started, supervisors, agents), pass or
//!   fail.
//! - [`bundled_daemons`] makes a bundled-daemons directory from the freshly built
//!   `controlroomd` (without its debug info, see [`copy_daemon`]).
//! - [`UiDriver`] speaks the UI API over a [`UiClient`] and mirrors the state feed.
//!
//! **Finding the binaries.** Integration tests run from `target/<profile>/deps/`, so
//! `controlroomd` and `controlroom-fake-agent` are looked up in `target/<profile>/`, where
//! `cargo test` of the workspace (and crane's `cargoTest`) builds them.
//! `$CONTROLROOM_DAEMON_BIN` and `$CONTROLROOM_FAKE_AGENT` override the paths; a missing binary
//! is built once with `$CARGO` in the same profile.
//!
//! **No sleeps.** Waits poll for a condition with a deadline that only turns a hang into a
//! failure; agents stop at named gates that tests release.

#![allow(clippy::unwrap_used, clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

pub use controlroom_client_core::test_support::{TestServer, USER};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::{CloseReason, UiClient};

/// How long any wait may take before the test fails.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// A stand-in `$SHELL` that runs `-lc <command>` with a plain `/bin/sh`, so host profile
/// scripts can't print into transcripts.
const LOGIN_SHELL: &str = "#!/bin/sh\n[ \"$1\" = -lc ] || { echo \"bad shell args: $*\" >&2; exit 99; }\nexec /bin/sh -c \"$2\"\n";

fn target_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap();
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent().unwrap().to_owned()
    } else {
        dir.to_owned()
    }
}

/// A workspace binary from the same profile as the running test, built if missing.
fn workspace_binary(package: &str, bin: &str, env_override: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env_override) {
        return PathBuf::from(path);
    }
    let dir = target_dir();
    let path = dir.join(bin);
    if !path.exists() {
        let profile = match dir.file_name().and_then(|p| p.to_str()) {
            Some("debug") | None => "dev".to_owned(),
            Some(other) => other.to_owned(),
        };
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", package, "--bin", bin, "--profile", &profile])
            .status()
            .unwrap_or_else(|e| panic!("building {bin}: {e}"));
        assert!(status.success(), "building {bin} failed");
    }
    assert!(path.exists(), "{} not found", path.display());
    path
}

/// The built `controlroomd`.
pub fn controlroomd() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| workspace_binary("controlroomd", "controlroomd", "CONTROLROOM_DAEMON_BIN"))
}

/// The built `controlroom-fake-agent`.
pub fn fake_agent() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        workspace_binary(
            "controlroom-fake-agent",
            "controlroom-fake-agent",
            "CONTROLROOM_FAKE_AGENT",
        )
    })
}

/// The version the built `controlroomd` reports (the workspace version).
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The bundled-binary file name for this machine.
pub fn bundled_name() -> String {
    format!("controlroomd-{}-linux", std::env::consts::ARCH)
}

/// Writes an executable file from a child shell (writing it from this process could race with
/// concurrent forks and fail with ETXTBSY).
pub fn write_executable(path: &Path, content: &[u8]) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).unwrap();
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"cp "$1" "$2" && chmod 755 "$2" && rm -f "$1""#)
        .arg("sh")
        .arg(&tmp)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o111,
        0o111
    );
}

/// A bundled-daemons directory holding the built `controlroomd`.
pub fn bundled_daemons() -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("crbin").tempdir().unwrap();
    copy_daemon(&dir.path().join(bundled_name()));
    std::fs::write(dir.path().join("version"), format!("{DAEMON_VERSION}\n")).unwrap();
    dir
}

/// Copies the built `controlroomd` to `dest` without its debug info, from a child process (as
/// in [`write_executable`]). Debug info is most of a debug build (about 180 of 230 MB), and
/// tests upload the daemon over SSH, where a debug build's crypto makes the full binary take
/// many seconds of CPU per install (minutes under load). Copies it whole where `strip` is
/// missing.
pub fn copy_daemon(dest: &Path) {
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"{ strip --strip-debug -o "$2" "$1" 2>/dev/null || cp "$1" "$2"; } && chmod 755 "$2""#)
        .arg("sh")
        .arg(controlroomd())
        .arg(dest)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "copying controlroomd to {}",
        dest.display()
    );
}

/// A bundled-daemons directory whose "daemon" is `script`, claiming `version`.
pub fn bundled_script(script: &str, version: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("crbin").tempdir().unwrap();
    write_executable(&dir.path().join(bundled_name()), script.as_bytes());
    std::fs::write(dir.path().join("version"), format!("{version}\n")).unwrap();
    dir
}

/// Polls `cond` until it holds, failing after [`TIMEOUT`].
pub async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A fake execution machine reachable over SSH as [`USER`].
pub struct RemoteHost {
    server: TestServer,
    agents: Vec<(String, String)>,
}

impl RemoteHost {
    /// Starts the SSH server. No key is authorized yet: see [`Self::authorize`].
    pub async fn start() -> Self {
        let server = TestServer::start(Vec::new()).await;
        let home = server.home_path().to_owned();
        write_executable(&home.join(".login-shell"), LOGIN_SHELL.as_bytes());
        for dir in ["work", "gates", ".pi/agent"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        server.set_env("SHELL", home.join(".login-shell").to_str().unwrap());
        server.set_env(
            "PI_CODING_AGENT_DIR",
            home.join(".pi/agent").to_str().unwrap(),
        );
        Self {
            server,
            agents: Vec::new(),
        }
    }

    /// The SSH server.
    pub fn server(&self) -> &TestServer {
        &self.server
    }

    /// The SSH port.
    pub fn port(&self) -> u16 {
        self.server.addr.port()
    }

    /// The remote `$HOME`.
    pub fn home(&self) -> &Path {
        self.server.home_path()
    }

    /// A working directory for sessions.
    pub fn work(&self) -> String {
        self.home().join("work").to_string_lossy().into_owned()
    }

    /// The `machines.add` params for this host.
    pub fn new_machine(&self, name: &str) -> Value {
        json!({"host": "127.0.0.1", "port": self.port(), "user": USER, "name": name})
    }

    /// Registers agent `name` as the fake agent (pi RPC) running `scenario`.
    pub fn agent(&mut self, name: &str, scenario: Value) {
        let file = self.home().join(format!(".scenario-{name}.json"));
        std::fs::write(&file, scenario.to_string()).unwrap();
        let command = format!(
            "{} --scenario {} --gate-dir {}",
            fake_agent().display(),
            file.display(),
            self.home().join("gates").display()
        );
        self.agents.push((name.to_owned(), command));
        let toml: String = self
            .agents
            .iter()
            .map(|(name, command)| {
                format!("[agents.{name}]\ncommand = {command:?}\ndriver = \"pi\"\n")
            })
            .collect();
        let dir = self.home().join(".config/controlroom");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.toml"), toml).unwrap();
    }

    /// Appends a public key line to `~/.ssh/authorized_keys`.
    pub fn authorize(&self, public_key_line: &str) {
        let dir = self.home().join(".ssh");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("authorized_keys");
        let mut contents = std::fs::read_to_string(&path).unwrap_or_default();
        contents.push_str(public_key_line.trim());
        contents.push('\n');
        std::fs::write(path, contents).unwrap();
    }

    /// `~/.ssh/authorized_keys`.
    pub fn authorized_keys(&self) -> String {
        std::fs::read_to_string(self.home().join(".ssh/authorized_keys")).unwrap_or_default()
    }

    /// Runs a command as the remote user would in a terminal (e.g. the key install command).
    pub fn run_local(&self, command: &str) {
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("HOME", self.home())
            .current_dir(self.home())
            .status()
            .unwrap();
        assert!(status.success(), "{command} failed");
    }

    /// Lets the agent pass gate `name`.
    pub fn release(&self, name: &str) {
        std::fs::write(self.home().join(format!("gates/{name}.release")), b"").unwrap();
    }

    /// Waits until an agent reaches gate `name`.
    pub async fn wait_gate(&self, name: &str) {
        let path = self.home().join(format!("gates/{name}.reached"));
        wait_until(&format!("gate {name}"), || path.exists()).await;
    }

    /// The `current` daemon link target (e.g. `versions/0.1.0`).
    pub fn current_daemon(&self) -> Option<String> {
        std::fs::read_link(self.home().join(".local/share/controlroom/current"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Pids of processes whose command line mentions this host's home and `needle`.
    pub fn processes(&self, needle: &str) -> Vec<i32> {
        let home = self.home().to_string_lossy().into_owned();
        let me = std::process::id() as i32;
        let mut pids = Vec::new();
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            if pid != me && cmdline.contains(&home) && cmdline.contains(needle) {
                pids.push(pid);
            }
        }
        pids
    }
}

impl Drop for RemoteHost {
    fn drop(&mut self) {
        // A supervisor killed mid-launch may already have forked: scan until two scans in a
        // row find nothing.
        let mut quiet = 0;
        let deadline = std::time::Instant::now() + TIMEOUT;
        while quiet < 2 && std::time::Instant::now() < deadline {
            let pids = self.processes("");
            quiet = if pids.is_empty() { quiet + 1 } else { 0 };
            for pid in pids {
                let _ = Command::new("kill")
                    .arg("-KILL")
                    .arg(pid.to_string())
                    .status();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if std::thread::panicking() {
            let log = self.home().join(".local/state/controlroom/daemon.log");
            let text = std::fs::read_to_string(log).unwrap_or_default();
            eprintln!("--- daemon.log (last lines) ---");
            for line in text.lines().rev().take(40).collect::<Vec<_>>().iter().rev() {
                eprintln!("{line}");
            }
        }
    }
}

/// The state feed as a client sees it, by id.
#[derive(Debug, Default)]
pub struct Mirror {
    /// Machines.
    pub machines: HashMap<String, Value>,
    /// Sessions (`MachineSession`).
    pub sessions: HashMap<String, Value>,
    /// Tasks.
    pub tasks: HashMap<String, Value>,
    /// Projects.
    pub projects: HashMap<String, Value>,
    /// Devices.
    pub devices: HashMap<String, Value>,
    /// Schedules.
    pub schedules: HashMap<String, Value>,
    /// Grants.
    pub grants: HashMap<String, Value>,
    /// How often each machine became `connected`.
    pub connected_count: HashMap<String, u32>,
}

impl Mirror {
    /// A machine, or `null`.
    pub fn machine(&self, id: &str) -> Value {
        self.machines.get(id).cloned().unwrap_or(Value::Null)
    }

    /// A session, or `null`.
    pub fn session(&self, id: &str) -> Value {
        self.sessions.get(id).cloned().unwrap_or(Value::Null)
    }

    fn apply_snapshot(&mut self, snapshot: &Value) {
        let index = |list: &Value| -> HashMap<String, Value> {
            list.as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    let id = v["id"]
                        .as_str()
                        .or(v["device_id"].as_str())
                        .unwrap_or_default();
                    (id.to_owned(), v.clone())
                })
                .collect()
        };
        self.machines = index(&snapshot["machines"]);
        self.sessions = index(&snapshot["sessions"]);
        self.tasks = index(&snapshot["tasks"]);
        self.projects = index(&snapshot["projects"]);
        self.devices = index(&snapshot["devices"]);
        self.schedules = index(&snapshot["schedules"]);
        self.grants = index(&snapshot["grants"]);
    }

    fn apply(&mut self, change: &Value) {
        let id = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        match change["kind"].as_str().unwrap_or_default() {
            "machine_upserted" => {
                let m = &change["machine"];
                let key = id(&m["id"]);
                let was = self
                    .machines
                    .get(&key)
                    .map(|o| o["connectivity"]["state"].clone());
                if m["connectivity"]["state"] == "connected"
                    && was.as_ref().is_none_or(|w| *w != "connected")
                {
                    *self.connected_count.entry(key.clone()).or_default() += 1;
                }
                self.machines.insert(key, m.clone());
            }
            "machine_removed" => {
                let key = id(&change["machine_id"]);
                self.machines.remove(&key);
                self.sessions
                    .retain(|_, s| s["machine_id"] != change["machine_id"]);
            }
            "session_upserted" => {
                let s = &change["session"];
                self.sessions.insert(id(&s["id"]), s.clone());
            }
            "session_removed" => {
                self.sessions.remove(&id(&change["session_id"]));
            }
            "task_upserted" => {
                self.tasks
                    .insert(id(&change["task"]["id"]), change["task"].clone());
            }
            "task_removed" => {
                self.tasks.remove(&id(&change["task_id"]));
            }
            "project_upserted" => {
                self.projects
                    .insert(id(&change["project"]["id"]), change["project"].clone());
            }
            "project_removed" => {
                self.projects.remove(&id(&change["project_id"]));
            }
            "device_upserted" => {
                self.devices
                    .insert(id(&change["device"]["device_id"]), change["device"].clone());
            }
            "device_removed" => {
                self.devices.remove(&id(&change["device_id"]));
            }
            "schedule_upserted" => {
                self.schedules
                    .insert(id(&change["schedule"]["id"]), change["schedule"].clone());
            }
            "schedule_removed" => {
                self.schedules.remove(&id(&change["schedule_id"]));
            }
            "grant_upserted" => {
                self.grants
                    .insert(id(&change["grant"]["id"]), change["grant"].clone());
            }
            "grant_removed" => {
                self.grants.remove(&id(&change["grant_id"]));
            }
            _ => {}
        }
    }
}

/// Drives one UI connection: calls, the mirrored state feed, and the other notifications.
pub struct UiDriver {
    client: UiClient,
    next_id: i64,
    responses: HashMap<i64, Value>,
    /// The mirrored state feed.
    pub mirror: Mirror,
    /// `session.events` and other notifications, in arrival order (state events are applied
    /// to [`Self::mirror`] instead).
    pub notifications: Vec<Value>,
    /// The last state cursor applied.
    pub cursor: Option<Value>,
    closed: bool,
}

impl UiDriver {
    /// Wraps a connection.
    pub fn new(client: UiClient) -> Self {
        Self {
            client,
            next_id: 0,
            responses: HashMap::new(),
            mirror: Mirror::default(),
            notifications: Vec::new(),
            cursor: None,
            closed: false,
        }
    }

    /// Sends a raw frame.
    pub async fn send_raw(&mut self, frame: &str) {
        let _ = self.client.send(frame.to_owned()).await;
    }

    /// Sends a request; returns its id.
    pub async fn send(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_raw(&frame.to_string()).await;
        id
    }

    /// Handles one inbound frame; `false` once the connection is closed.
    async fn pump(&mut self, deadline: Instant) -> bool {
        if self.closed {
            return false;
        }
        let frame = tokio::time::timeout_at(deadline, self.client.recv()).await;
        let Ok(frame) = frame else { return true };
        let Some(frame) = frame else {
            self.closed = true;
            return false;
        };
        let message: Value = serde_json::from_str(&frame).unwrap();
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            self.responses.insert(id, message);
        } else if message["method"] == "state.events" {
            for change in message["params"]["changes"].as_array().unwrap() {
                self.mirror.apply(change);
                self.cursor = Some(json!({
                    "epoch": self.cursor.as_ref().map(|c| c["epoch"].clone()).unwrap_or(Value::Null),
                    "rev": change["rev"],
                }));
            }
        } else {
            self.notifications.push(message);
        }
        true
    }

    /// The whole response to request `id`.
    pub async fn response(&mut self, id: i64) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(r) = self.responses.remove(&id) {
                return r;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for response {id}"
            );
            assert!(
                self.pump(deadline).await,
                "the connection closed before response {id}"
            );
        }
    }

    /// Calls `method`: `Ok(result)` or `Err(error)`.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.send(method, params).await;
        let response = self.response(id).await;
        match response.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(response["result"].clone()),
        }
    }

    /// Calls `method` and expects success.
    pub async fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params.clone())
            .await
            .unwrap_or_else(|e| panic!("{method} {params} failed: {e}"))
    }

    /// Calls `method` and expects an error of `kind`.
    pub async fn err(&mut self, method: &str, params: Value, kind: &str) -> Value {
        let error = self
            .call(method, params.clone())
            .await
            .map(|r| panic!("{method} {params} succeeded: {r}"))
            .unwrap_err();
        assert_eq!(error["data"]["kind"], kind, "{method}: {error}");
        error
    }

    /// `hello` (with `device`) and `devices.register`.
    pub async fn hello(&mut self, device: &str, label: &str) -> Value {
        let hello = self
            .ok(
                "hello",
                json!({"protocol_version": {"major": 1, "minor": 0}, "device_id": device}),
            )
            .await;
        self.ok(
            "devices.register",
            json!({"device_id": device, "label": label}),
        )
        .await;
        hello
    }

    /// `state.subscribe` from scratch, mirroring the snapshot.
    pub async fn subscribe_state(&mut self) -> Value {
        let result = self.ok("state.subscribe", json!({"since": null})).await;
        self.mirror.apply_snapshot(&result["snapshot"]);
        self.cursor = Some(result["cursor"].clone());
        result
    }

    /// Waits until `pred` holds for the mirrored state, failing after [`TIMEOUT`].
    pub async fn wait(&mut self, what: &str, pred: impl Fn(&Mirror) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !pred(&self.mirror) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; machines: {:#?}",
                self.mirror.machines
            );
            if !self.pump(deadline).await {
                assert!(pred(&self.mirror), "closed while waiting for {what}");
            }
        }
    }

    /// Waits for a notification matching `pred`, taking it out of the log.
    pub async fn notification(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(i) = self.notifications.iter().position(&pred) {
                return self.notifications.remove(i);
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            assert!(self.pump(deadline).await, "closed while waiting for {what}");
        }
    }

    /// Waits until the controller closes the connection; returns why.
    pub async fn closed(&mut self) -> Option<CloseReason> {
        let deadline = Instant::now() + TIMEOUT;
        while self.pump(deadline).await {
            assert!(Instant::now() < deadline, "timed out waiting for the close");
        }
        self.client.close_reason()
    }
}

/// A text prompt's content.
pub fn text(s: &str) -> Value {
    json!([{"kind": "text", "text": s}])
}

/// A fresh op id.
pub fn op() -> String {
    controlroom_protocol::ids::OpId::generate().0
}

/// A scenario whose replies are plain texts, in order.
pub fn texts(replies: &[&str]) -> Value {
    json!({"replies": replies.iter().map(|t| json!({"content": [{"type": "text", "text": t}]})).collect::<Vec<_>>()})
}

/// A reply that streams `before`, waits at `gate`, then streams `after`.
pub fn gated(before: &str, gate: &str, after: &str) -> Value {
    json!({"content": [
        {"type": "text", "text": before},
        {"type": "gate", "name": gate},
        {"type": "text", "text": after},
    ]})
}

/// A reply that runs a tool behind a permission dialog.
pub fn permission(title: &str) -> Value {
    json!({"content": [{"type": "tool_call", "id": "call_1", "name": "bash",
        "args": {"command": "rm -rf build"}, "result": "removed",
        "permission": {"title": title}}]})
}
