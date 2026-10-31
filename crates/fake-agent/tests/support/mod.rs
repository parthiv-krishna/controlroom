//! Running the fake-agent binary in tests: stdio capture, waits on output,
//! and cleanup (the process is SIGKILLed when dropped).

#![allow(dead_code)] // Each test binary uses a different subset.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::watch;

/// How long any single wait may take.
pub const WAIT: Duration = Duration::from_secs(10);

/// An isolated directory layout: `agent/` (`PI_CODING_AGENT_DIR`),
/// `project/` (the cwd), `gates/`.
pub struct Sandbox {
    pub dir: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("project")).expect("project dir");
        Self { dir }
    }

    pub fn path(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    pub fn gates(&self) -> controlroom_fake_agent::Gates {
        controlroom_fake_agent::Gates::new(self.path("gates")).expect("gates")
    }

    /// Spawn the fake with `--mode rpc <args>` and this scenario.
    pub fn spawn(&self, scenario: &str, args: &[String]) -> Fake {
        let mut command = Command::new(env!("CARGO_BIN_EXE_controlroom-fake-agent"));
        command
            .args(["--mode", "rpc", "--scenario-json", scenario, "--gate-dir"])
            .arg(self.path("gates"))
            .args(args)
            .current_dir(self.path("project"))
            .env("PI_CODING_AGENT_DIR", self.path("agent"))
            .env("HOME", self.path("home"))
            .env_remove("CONTROLROOM_FAKE_SCENARIO")
            .env_remove("CONTROLROOM_FAKE_GATES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        Fake::spawn(command)
    }
}

/// A running fake agent.
pub struct Fake {
    pub child: Child,
    stdin: Option<ChildStdin>,
    lines: Arc<Mutex<Vec<Value>>>,
    changed: watch::Receiver<usize>,
    reader: tokio::task::JoinHandle<()>,
    /// Lines before this index were consumed by `next_*`.
    cursor: usize,
}

impl Fake {
    fn spawn(mut command: Command) -> Self {
        let mut child = command.spawn().expect("spawning fake-agent");
        let stdout = child.stdout.take().expect("stdout");
        let lines: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (tx, changed) = watch::channel(0);
        let sink = lines.clone();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        // Only protocol records count; anything else is noise.
                        if let Ok(value) = serde_json::from_slice::<Value>(&buf) {
                            let mut lines = sink.lock().unwrap_or_else(PoisonError::into_inner);
                            lines.push(value);
                            tx.send_replace(lines.len());
                        }
                    }
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            changed,
            reader,
            cursor: 0,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id().expect("running")
    }

    /// All records so far.
    pub fn records(&self) -> Vec<Value> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub async fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(line.as_bytes()).await.expect("write");
        stdin.write_all(b"\n").await.expect("write");
        stdin.flush().await.expect("flush");
    }

    pub async fn send(&mut self, command: Value) {
        self.send_raw(&command.to_string()).await;
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Wait until at least `n` records arrived.
    pub async fn wait_count(&mut self, n: usize) -> Result<(), String> {
        let wait = self.changed.wait_for(|len| *len >= n);
        let reached = matches!(tokio::time::timeout(WAIT, wait).await, Ok(Ok(_)));
        if reached {
            return Ok(());
        }
        Err(format!(
            "timed out waiting for {n} records, got {}",
            self.records().len()
        ))
    }

    /// Wait for the next record (after earlier `next_*` calls) matching `pred`.
    pub async fn next(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            let records = self.records();
            if let Some(i) = (self.cursor..records.len()).find(|&i| pred(&records[i])) {
                self.cursor = i + 1;
                return records[i].clone();
            }
            let len = records.len();
            let more = self.changed.wait_for(|n| *n > len);
            if !matches!(tokio::time::timeout(WAIT, more).await, Ok(Ok(_))) {
                panic!("timed out waiting for {what}; records: {records:#?}");
            }
        }
    }

    /// Wait for the next event of `kind`.
    pub async fn next_event(&mut self, kind: &str) -> Value {
        self.next(kind, |v| v["type"] == kind).await
    }

    /// Send a command with `id` and wait for its response.
    pub async fn call(&mut self, id: &str, mut command: Value) -> Value {
        command["id"] = id.into();
        self.send(command).await;
        self.next(&format!("response {id}"), |v| {
            v["type"] == "response" && v["id"] == id
        })
        .await
    }

    /// Wait for the exit; returns `(code, signal)`.
    pub async fn wait_exit(&mut self) -> (Option<i32>, Option<i32>) {
        use std::os::unix::process::ExitStatusExt;
        let status = tokio::time::timeout(WAIT, self.child.wait())
            .await
            .expect("fake-agent did not exit")
            .expect("wait");
        let _ = tokio::time::timeout(WAIT, &mut self.reader).await;
        (status.code(), status.signal())
    }

    /// Send a signal by name (`KILL`, `HUP`, ...).
    pub fn signal(&self, signal: &str) {
        let status = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &self.pid().to_string()])
            .status()
            .expect("running kill");
        assert!(status.success(), "kill -{signal} failed");
    }
}

/// The fixtures directory of the pinned pi version.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pi-rpc/1.2.0")
}
