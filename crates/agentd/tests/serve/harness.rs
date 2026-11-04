//! Harness for end-to-end tests: a real `controlroom-agentd serve` in a scratch directory, real
//! supervisors, `controlroom-fake-agent` (pi RPC) as the agent, and a fake controller (see
//! [`crate::controller`]) that serves one machine, `devbox`, whose host runs in this process.
//!
//! **Finding the fake agent.** `CARGO_BIN_EXE_*` only covers this package's binaries, so the
//! harness looks for `controlroom-fake-agent` next to `controlroom-agentd`: both land in the
//! same `target/<profile>/` under `cargo test` of the workspace and under crane's `cargoTest`.
//! `$CONTROLROOM_FAKE_AGENT` overrides the path; if it is missing (`cargo test -p
//! controlroom-agentd` on a clean tree) the harness builds it once through `$CARGO`.
//!
//! **No sleeps.** The agent stops at named gates (files in a shared directory) that tests
//! release; waits poll for a condition with a deadline that only turns a hang into a failure.
//! **Cleanup.** Dropping an [`Env`] SIGKILLs every process whose command line mentions its
//! scratch directory (daemon, supervisors, agents), pass or fail.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use controlroom_transcript::{Body, Record, read_all};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};

use crate::controller::FakeController;

pub const BIN: &str = env!("CARGO_BIN_EXE_controlroom-agentd");
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// A stand-in `$SHELL`: runs `-lc <command>` with a plain `/bin/sh`, so host profile scripts
/// can't print into transcripts.
const LOGIN_SHELL: &str = "#!/bin/sh\n[ \"$1\" = -lc ] || { echo \"bad shell args: $*\" >&2; exit 99; }\nexec /bin/sh -c \"$2\"\n";

/// The fake agent binary; see the module docs.
pub fn fake_agent() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Some(path) = std::env::var_os("CONTROLROOM_FAKE_AGENT") {
            return PathBuf::from(path);
        }
        let path = Path::new(BIN).with_file_name("controlroom-fake-agent");
        if !path.exists() {
            let profile = match Path::new(BIN)
                .parent()
                .and_then(Path::file_name)
                .and_then(|p| p.to_str())
            {
                Some("debug") | None => "dev".to_owned(),
                Some(other) => other.to_owned(),
            };
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let status = Command::new(cargo)
                .args([
                    "build",
                    "-p",
                    "controlroom-fake-agent",
                    "--profile",
                    &profile,
                ])
                .status()
                .expect("building controlroom-fake-agent");
            assert!(status.success(), "building controlroom-fake-agent failed");
        }
        assert!(path.exists(), "{} not found", path.display());
        path
    })
}

/// Polls `cond` until it holds, failing after [`TIMEOUT`].
pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A text prompt's content.
pub fn text(s: &str) -> Value {
    json!([{"kind": "text", "text": s}])
}

/// A fresh op id.
pub fn op() -> String {
    ulid::Ulid::generate().to_string()
}

/// A scratch directory with an agent daemon, its agent, a fake controller and gates.
pub struct Env {
    dir: tempfile::TempDir,
    daemon: Option<Child>,
    controller: Option<FakeController>,
    /// The fake controller's machine (stable across agent daemon restarts).
    machine: String,
}

impl Env {
    /// An empty environment (no daemon yet).
    pub fn new() -> Self {
        Self::new_in(&std::env::temp_dir())
    }

    /// An empty environment in a scratch directory under `parent`.
    pub fn new_in(parent: &Path) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("cr")
            .tempdir_in(parent)
            .unwrap();
        let shell = dir.path().join("login-shell");
        write_executable(&shell, LOGIN_SHELL);
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        std::fs::create_dir_all(dir.path().join("gates")).unwrap();
        Env {
            dir,
            daemon: None,
            controller: None,
            machine: ulid::Ulid::generate().to_string(),
        }
    }

    /// An environment whose agent runs `scenario`, with a started daemon.
    pub fn with(scenario: Value) -> Self {
        let mut env = Env::new();
        env.agent(scenario);
        env.start();
        env
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn state(&self) -> PathBuf {
        self.path().join("state")
    }

    pub fn work(&self) -> String {
        self.path().join("work").to_string_lossy().into_owned()
    }

    /// One workspace, `devbox:<work>`, as `session.create` takes it.
    pub fn workspaces(&self) -> Value {
        json!([{"machine_id": self.machine, "path": self.work()}])
    }

    pub fn socket(&self) -> PathBuf {
        self.state().join("run/agentd.sock")
    }

    /// The fake controller (while the daemon runs).
    pub fn controller(&self) -> &FakeController {
        self.controller.as_ref().expect("the daemon is running")
    }

    pub fn transcript_path(&self, session: &str) -> PathBuf {
        self.state()
            .join("sessions")
            .join(session)
            .join("transcript.jsonl")
    }

    /// Makes the sessions' agent the fake agent running `scenario` (pi driver).
    pub fn agent(&mut self, scenario: Value) {
        let file = self.path().join("scenario.json");
        std::fs::write(&file, scenario.to_string()).unwrap();
        let command = format!(
            "{} --scenario {} --gate-dir {}",
            fake_agent().display(),
            file.display(),
            self.path().join("gates").display()
        );
        let config = self.state().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("agents.toml"),
            format!("[agents.pi]\ncommand = {command:?}\ndriver = \"pi\"\n"),
        )
        .unwrap();
    }

    /// `controlroom-agentd` with this environment. The fake agent's directory is on its `PATH`, as
    /// agents must be for nono (`doctor` reports agents elsewhere as missing).
    pub fn command(&self) -> Command {
        let agent_dir = fake_agent()
            .parent()
            .expect("the fake agent has a directory");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(agent_dir.to_owned()).chain(std::env::split_paths(&path)),
        )
        .expect("PATH");
        let mut cmd = Command::new(BIN);
        cmd.env("SHELL", self.path().join("login-shell"))
            .env("PATH", path)
            .env("HOME", self.path())
            .env_remove("PI_CODING_AGENT_DIR")
            .env("CONTROLROOM_AGENTD_LOG", "debug");
        cmd
    }

    /// Starts `controlroom-agentd serve` and waits for its socket, then connects the fake
    /// controller.
    pub fn start(&mut self) {
        assert!(self.daemon.is_none(), "the daemon is already running");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path().join("daemon.log"))
            .unwrap();
        let child = self
            .command()
            .args(["serve", "--state-dir"])
            .arg(self.state())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.daemon = Some(child);
        let socket = self.socket();
        wait_until("the daemon socket", || UnixStream::connect(&socket).is_ok());
        self.controller = Some(FakeController::connect(&socket, &self.machine, self.path()));
    }

    /// SIGKILLs the daemon (supervisors and agents keep running).
    pub fn kill_daemon(&mut self) {
        let mut child = self.daemon.take().expect("the daemon is running");
        child.kill().unwrap();
        child.wait().unwrap();
        self.controller = None;
    }

    /// SIGTERMs the daemon and waits for it to exit.
    pub fn stop_daemon(&mut self) -> std::process::ExitStatus {
        self.controller = None;
        let mut child = self.daemon.take().expect("the daemon is running");
        let pid = Pid::from_raw(child.id() as i32).unwrap();
        kill_process(pid, Signal::TERM).unwrap();
        child.wait().unwrap()
    }

    /// A connected debug client.
    pub fn client(&self) -> Client {
        let client = Client::connect(&self.socket());
        client.hello_debug();
        client
    }

    /// Lets the agent pass gate `name`.
    pub fn release(&self, name: &str) {
        std::fs::write(self.path().join(format!("gates/{name}.release")), b"").unwrap();
    }

    /// Waits until the agent reaches gate `name`.
    pub fn wait_gate(&self, name: &str) {
        let path = self.path().join(format!("gates/{name}.reached"));
        wait_until(&format!("gate {name}"), || path.exists());
    }

    /// The session's transcript records.
    pub fn records(&self, session: &str) -> Vec<Record> {
        read_all(&self.transcript_path(session), 0)
            .map(|all| all.records)
            .unwrap_or_default()
    }

    /// The JSON lines written to the agent, in order.
    pub fn lines_in(&self, session: &str) -> Vec<Value> {
        self.records(session)
            .into_iter()
            .filter_map(|r| match r.body {
                Body::In(line) => serde_json::from_str(&line.raw).ok(),
                _ => None,
            })
            .collect()
    }

    /// The prompt texts written to the agent (prompt, steer and follow-up commands).
    pub fn prompts_sent(&self, session: &str) -> Vec<String> {
        self.lines_in(session)
            .into_iter()
            .filter(|l| matches!(l["type"].as_str(), Some("prompt" | "steer" | "follow_up")))
            .filter_map(|l| l["message"].as_str().map(str::to_owned))
            .collect()
    }

    /// The pid of the session's running supervisor and agent.
    pub fn supervisor_pids(&self, session: &str) -> (u32, u32) {
        let info: Value = serde_json::from_slice(
            &std::fs::read(
                self.state()
                    .join("sessions")
                    .join(session)
                    .join("supervisor.json"),
            )
            .unwrap(),
        )
        .unwrap();
        (
            info["pid"].as_u64().unwrap() as u32,
            info["child_pid"].as_u64().unwrap() as u32,
        )
    }

    /// Processes whose command line mentions this environment.
    fn processes(&self) -> Vec<i32> {
        let needle = self.path().to_string_lossy().into_owned();
        let me = std::process::id() as i32;
        let mut pids = Vec::new();
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            if pid != me && String::from_utf8_lossy(&cmdline).contains(&needle) {
                pids.push(pid);
            }
        }
        pids
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // A supervisor launcher killed mid-launch may already have forked its detached child:
        // scan until two scans in a row find nothing.
        let mut quiet = 0;
        let deadline = Instant::now() + TIMEOUT;
        while quiet < 2 && Instant::now() < deadline {
            let pids = self.processes();
            quiet = if pids.is_empty() { quiet + 1 } else { 0 };
            for pid in pids {
                if let Some(pid) = Pid::from_raw(pid) {
                    let _ = kill_process(pid, Signal::KILL);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if std::thread::panicking() {
            let log = std::fs::read_to_string(self.path().join("daemon.log")).unwrap_or_default();
            let tail: Vec<&str> = log.lines().rev().take(60).collect();
            eprintln!("--- daemon.log (last lines, newest first) ---");
            for line in tail {
                eprintln!("{line}");
            }
            let sessions = std::fs::read_dir(self.state().join("sessions"));
            for session in sessions.into_iter().flatten().flatten() {
                for file in ["supervisor.log", "transcript.jsonl"] {
                    let text =
                        std::fs::read_to_string(session.path().join(file)).unwrap_or_default();
                    eprintln!("--- {} ---", session.path().join(file).display());
                    for line in text.lines().rev().take(25).collect::<Vec<_>>().iter().rev() {
                        eprintln!("{}", line.chars().take(400).collect::<String>());
                    }
                }
            }
        }
    }
}

/// Writes an executable script from a child shell (writing it from this process would race
/// with concurrent forks and fail with ETXTBSY).
fn write_executable(path: &Path, content: &str) {
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
        .arg("sh")
        .arg(content)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111);
}

#[derive(Default)]
struct Inbox {
    responses: HashMap<i64, Value>,
    notifications: Vec<Value>,
    /// Latest summary per session, from `sessions.events` of this client's subscriptions.
    summaries: HashMap<String, Value>,
    closed: bool,
}

/// A JSON-RPC client over the daemon socket; a reader thread collects everything.
pub struct Client {
    writer: Mutex<UnixStream>,
    inbox: Arc<(Mutex<Inbox>, Condvar)>,
    next_id: Mutex<i64>,
}

impl Client {
    /// Connects without saying hello.
    pub fn connect(socket: &Path) -> Client {
        let stream = UnixStream::connect(socket).unwrap();
        let inbox: Arc<(Mutex<Inbox>, Condvar)> = Arc::default();
        let reader = stream.try_clone().unwrap();
        let shared = inbox.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                let value: Value = serde_json::from_str(&line).expect("daemon sent JSON");
                let (lock, cond) = &*shared;
                let mut inbox = lock.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(id) = value.get("id").and_then(Value::as_i64) {
                    inbox.responses.insert(id, value);
                } else {
                    if value["method"] == "sessions.events" {
                        for change in value["params"]["changes"].as_array().unwrap() {
                            if change["kind"] == "session_upserted" {
                                let session = change["session"].clone();
                                inbox
                                    .summaries
                                    .insert(session["id"].as_str().unwrap().to_owned(), session);
                            }
                        }
                    }
                    inbox.notifications.push(value);
                }
                cond.notify_all();
            }
            let (lock, cond) = &*shared;
            lock.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
            cond.notify_all();
        });
        Client {
            writer: Mutex::new(stream),
            inbox,
            next_id: Mutex::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inbox> {
        self.inbox.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until `f` returns `Some`, failing after [`TIMEOUT`].
    fn wait<T>(&self, what: &str, mut f: impl FnMut(&mut Inbox) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        let (_, cond) = &*self.inbox;
        let mut inbox = self.lock();
        loop {
            if let Some(v) = f(&mut inbox) {
                return v;
            }
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for {what}");
            inbox = cond
                .wait_timeout(inbox, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Sends a raw line.
    pub fn send_raw(&self, line: &str) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(line.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
    }

    /// Sends a request and returns its id.
    pub fn send(&self, method: &str, params: Value) -> i64 {
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            *next
        };
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_raw(&request.to_string());
        id
    }

    /// The response to request `id` (the whole message).
    pub fn response(&self, id: i64) -> Value {
        self.wait(&format!("response {id}"), |inbox| {
            inbox.responses.remove(&id)
        })
    }

    /// Calls `method`: `Ok(result)` or `Err(error)`.
    pub fn try_call(&self, method: &str, params: Value) -> Result<Value, Value> {
        let response = self.response(self.send(method, params));
        match response.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(response["result"].clone()),
        }
    }

    /// Calls `method` and expects success.
    pub fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params.clone())
            .unwrap_or_else(|e| panic!("{method} {params} failed: {e}"))
    }

    /// Calls `method` and expects an error of `kind`.
    pub fn call_err(&self, method: &str, params: Value, kind: &str) -> Value {
        let error = self
            .try_call(method, params.clone())
            .map(|r| panic!("{method} {params} succeeded: {r}"))
            .unwrap_err();
        assert_eq!(error["data"]["kind"], kind, "{method}: {error}");
        error
    }

    pub fn hello_debug(&self) -> Value {
        self.call("hello", hello_params(json!({"kind": "debug"})))
    }

    /// Subscribes to the sessions feed so [`Client::wait_session`] works.
    pub fn watch_sessions(&self) -> Value {
        let result = self.call("sessions.subscribe", json!({"since_rev": null}));
        let mut inbox = self.lock();
        for session in result["snapshot"]["sessions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            inbox
                .summaries
                .entry(session["id"].as_str().unwrap().to_owned())
                .or_insert_with(|| session.clone());
        }
        result
    }

    /// Waits for a summary of `session` matching `pred` (needs [`Client::watch_sessions`]).
    pub fn wait_session(&self, session: &str, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        self.wait(&format!("session {what}"), |inbox| {
            inbox.summaries.get(session).filter(|s| pred(s)).cloned()
        })
    }

    /// Every notification received so far.
    pub fn notifications(&self) -> Vec<Value> {
        self.lock().notifications.clone()
    }

    /// Waits for a notification matching `pred`, taking it out of the log.
    pub fn wait_notification(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        self.wait(what, |inbox| {
            let index = inbox.notifications.iter().position(&pred)?;
            Some(inbox.notifications.remove(index))
        })
    }

    /// Waits (without a deadline) for a notification matching `pred`, taking it out of the
    /// log; `None` once the connection is closed.
    pub fn next_notification(&self, pred: impl Fn(&Value) -> bool) -> Option<Value> {
        let (_, cond) = &*self.inbox;
        let mut inbox = self.lock();
        loop {
            if let Some(index) = inbox.notifications.iter().position(&pred) {
                return Some(inbox.notifications.remove(index));
            }
            if inbox.closed {
                return None;
            }
            inbox = cond.wait(inbox).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Waits until the daemon closes the connection.
    pub fn wait_closed(&self) {
        self.wait("the connection to close", |inbox| {
            inbox.closed.then_some(())
        });
    }

    /// Creates a session (sandbox off) on `workspaces` ([`Env::workspaces`]); returns its id.
    pub fn create(&self, workspaces: &Value) -> String {
        let result = self.call(
            "session.create",
            json!({"op_id": op(), "workspaces": workspaces, "sandbox_level": "off"}),
        );
        result["session"]["id"].as_str().unwrap().to_owned()
    }

    /// Sends a text prompt; returns the result.
    pub fn prompt(&self, session: &str, text_: &str) -> Value {
        self.call(
            "session.prompt",
            json!({"op_id": op(), "id": session, "content": text(text_)}),
        )
    }

    /// The session's `last_turn_end_seq` now, from a fresh snapshot (the feed's latest
    /// summary may lag behind).
    pub fn turn_mark(&self, session: &str) -> u64 {
        let result = self.call("sessions.subscribe", json!({"since_rev": null}));
        self.call(
            "unsubscribe",
            json!({"subscription_id": result["subscription_id"]}),
        );
        result["snapshot"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == session)
            .and_then(|s| s["last_turn_end_seq"].as_u64())
            .expect("the session is in the snapshot")
    }

    /// Waits until a run ended after `mark` (see [`Client::turn_mark`]) and the session is
    /// idle with an empty queue.
    pub fn wait_turn(&self, session: &str, mark: u64) -> Value {
        self.wait_session(session, "a turn end", |s| {
            s["state"] == "idle"
                && s["queue"]["len"] == 0
                && s["last_turn_end_seq"].as_u64() > Some(mark)
        })
    }

    /// Waits until `session` is idle with an empty queue. Only for states the session can't
    /// have been in before (e.g. right after `create`); after a prompt use
    /// [`Client::wait_turn`].
    pub fn wait_idle(&self, session: &str) -> Value {
        self.wait_session(session, "idle", |s| {
            s["state"] == "idle" && s["queue"]["len"] == 0
        })
    }

    /// Subscribes to `session` from `since` and collects its ops until `until` holds for the
    /// collected text; returns the subscription id and the ops.
    pub fn session_ops(&self, session: &str, since: u64) -> (Value, Vec<Value>) {
        let result = self.call(
            "session.subscribe",
            json!({"id": session, "since_seq": since}),
        );
        let sub = result["subscription_id"].clone();
        let ops = self.wait("the session backlog", |inbox| {
            let index = inbox.notifications.iter().position(|n| {
                n["method"] == "session.events" && n["params"]["subscription_id"] == sub
            })?;
            Some(inbox.notifications.remove(index)["params"]["ops"].clone())
        });
        (result, ops.as_array().cloned().unwrap_or_default())
    }
}

/// `hello` params for `role`.
pub fn hello_params(role: Value) -> Value {
    json!({
        "protocol_version": {"major": 1, "minor": 0},
        "client_name": "e2e",
        "role": role,
    })
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

/// Pending queue items of a session's detail, via a fresh subscription.
pub fn queue_texts(client: &Client, session: &str) -> Vec<String> {
    let result = client.call("session.subscribe", json!({"id": session, "since_seq": 0}));
    client.call(
        "unsubscribe",
        json!({"subscription_id": result["subscription_id"]}),
    );
    result["detail"]["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["content"][0]["text"].as_str().unwrap_or("").to_owned())
        .collect()
}

impl Client {
    /// Closes the connection in both directions (the reader thread then sees it closed).
    pub fn shutdown(&self) {
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writer.shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The reader thread holds a clone of the stream: close both directions explicitly.
        self.shutdown();
    }
}
