//! Harness for driving `controlroomd supervise` with shell one-liners as children.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use controlroom_supervisor::{
    Client, Exec, Paths, ReadyLine, RecordStream, SpawnSpec, SupervisorInfo,
};
use controlroom_transcript::{Body, ProcessKind, Record, StdinMode};
use rustix::process::{Pid, Signal, kill_process, kill_process_group};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub const BIN: &str = env!("CARGO_BIN_EXE_controlroomd");
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// A stand-in `$SHELL`. A real `sh -l` sources the host's profile, which may print
/// to stdout and makes transcripts host-dependent. This one checks it is invoked as
/// `$SHELL -lc <command>` and runs the command with a plain `/bin/sh`.
const LOGIN_SHELL: &str = "#!/bin/sh\n[ \"$1\" = -lc ] || { echo \"bad shell args: $*\" >&2; exit 99; }\nexec /bin/sh -c \"$2\"\n";

/// The stand-in login shell in `state`, created on first use.
pub fn login_shell(state: &Path) -> PathBuf {
    let shell = state.join("login-shell");
    if !shell.exists() {
        write_executable(&shell, LOGIN_SHELL);
    }
    shell
}

/// Writes an executable script from a child shell. Writing it from this process
/// would race with concurrent tests: a child forked while the write fd is open
/// inherits it until it execs, and executing the script then fails with ETXTBSY.
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
}

/// `controlroomd` with the stand-in shell and no real home directory.
pub fn base_command(state: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env("SHELL", login_shell(state)).env("HOME", state);
    cmd
}

/// The default spec: an agent running `command` through the login shell.
pub fn shell_spec(state: &Path, command: &str) -> SpawnSpec {
    SpawnSpec {
        kind: ProcessKind::Agent,
        exec: Exec::Shell(command.into()),
        cwd: state.to_path_buf(),
        env_file: None,
        stdin: StdinMode::Pipe,
        timeout_ms: None,
        max_line_bytes: None,
        labels: BTreeMap::new(),
        plain_log: None,
    }
}

pub fn paths(state: &Path, id: &str) -> Paths {
    Paths {
        dir: state.join("procs").join(id),
        socket: state.join(format!("{id}.sock")),
    }
}

/// `supervise` arguments for `id` in `state`.
pub fn supervise_args(cmd: &mut Command, paths: &Paths, id: &str, spec: &SpawnSpec) {
    cmd.arg("supervise")
        .arg(id)
        .arg("--dir")
        .arg(&paths.dir)
        .arg("--socket")
        .arg(&paths.socket)
        .arg("--spec")
        .arg(serde_json::to_string(spec).unwrap());
}

/// Runs a foreground supervisor to completion of its startup; returns its ready line.
pub fn run_foreground(
    state: &Path,
    paths: &Paths,
    id: &str,
    spec: &SpawnSpec,
) -> (Child, ReadyLine) {
    let mut cmd = base_command(state);
    supervise_args(&mut cmd, paths, id, spec);
    let mut child = cmd
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Diagnostics go to the test output; an unread pipe could fill up.
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
    (child, ready)
}

/// A foreground supervisor. Dropping it kills the supervisor and the child's group.
pub struct Sup {
    pub child: Child,
    pub state: tempfile::TempDir,
    pub paths: Paths,
    pub info: SupervisorInfo,
}

impl Sup {
    /// An agent running `command` through the login shell.
    pub fn start(command: &str) -> Self {
        Self::start_with(command, |_| {})
    }

    /// Like [`start`](Self::start), adjusting the spec first.
    pub fn start_with(command: &str, adjust: impl FnOnce(&mut SpawnSpec)) -> Self {
        let state = tempfile::tempdir().unwrap();
        let mut spec = shell_spec(state.path(), command);
        adjust(&mut spec);
        Self::start_in(state, &spec)
    }

    pub fn start_in(state: tempfile::TempDir, spec: &SpawnSpec) -> Self {
        let paths = paths(state.path(), "s1");
        let (child, ready) = run_foreground(state.path(), &paths, "s1", spec);
        let ReadyLine::Ready(info) = ready else {
            panic!("not ready: {ready:?}");
        };
        Self {
            child,
            state,
            paths,
            info,
        }
    }

    pub fn transcript(&self) -> PathBuf {
        self.paths.transcript()
    }

    pub async fn connect(&self, writer: bool) -> Client {
        Client::connect(&self.paths.socket, writer).await.unwrap()
    }

    /// Waits for the supervisor to exit successfully and checks its cleanup. Returns
    /// the state directory, which is deleted when dropped.
    #[must_use]
    pub fn wait_exit(mut self) -> tempfile::TempDir {
        let status = wait_child(&mut self.child);
        assert!(status.success(), "supervisor exited with {status}");
        assert!(!self.paths.socket.exists(), "socket not removed");
        assert!(
            !self.paths.info_file().exists(),
            "supervisor.json not removed"
        );
        std::mem::replace(&mut self.state, tempfile::tempdir().unwrap())
    }
}

impl Drop for Sup {
    fn drop(&mut self) {
        kill_group(self.info.child_pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Waits (bounded) for a child process to exit.
pub fn wait_child(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "process did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Polls `cond` until it holds (bounded). For conditions with no event to wait on.
pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn kill_group(pgid: u32) {
    if let Some(pid) = i32::try_from(pgid)
        .ok()
        .filter(|&p| p > 1)
        .and_then(Pid::from_raw)
    {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

pub fn signal(pid: u32, signal: Signal) {
    let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    kill_process(pid, signal).unwrap();
}

/// Whether `pid` is gone or a zombie.
pub fn is_dead(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state == "Z" || state == "X"),
    }
}

/// `(ppid, session id)` from `/proc/<pid>/stat`.
pub fn proc_ids(pid: u32) -> Option<(u32, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields[1].parse().ok()?, fields[3].parse().ok()?))
}

/// The next record, failing the test after [`TIMEOUT`].
pub async fn next(stream: &mut RecordStream) -> Option<Record> {
    tokio::time::timeout(TIMEOUT, stream.next())
        .await
        .expect("timed out waiting for a record")
}

/// Records until (and including) the first one matching `stop`.
pub async fn records_until(
    stream: &mut RecordStream,
    stop: impl Fn(&Record) -> bool,
) -> Vec<Record> {
    let mut out = Vec::new();
    loop {
        let record = next(stream).await.expect("stream ended early");
        let done = stop(&record);
        out.push(record);
        if done {
            return out;
        }
    }
}

pub fn is_out(raw: &'static str) -> impl Fn(&Record) -> bool {
    move |r| matches!(&r.body, Body::Out(line) if line.raw == raw)
}

pub fn is_exit(r: &Record) -> bool {
    matches!(r.body, Body::Exit(_))
}

pub fn dir(r: &Record) -> &'static str {
    match r.body {
        Body::In(_) => "in",
        Body::Out(_) => "out",
        Body::Stderr(_) => "stderr",
        Body::Exit(_) => "exit",
        Body::Meta(_) => "meta",
        Body::Unknown(_) => "unknown",
    }
}

pub fn seqs_increase_by_one(records: &[Record]) {
    for pair in records.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1, "{records:#?}");
    }
}

/// A raw JSON client for protocol-level checks.
pub struct Raw {
    lines: tokio::io::Lines<AsyncBufReader<OwnedReadHalf>>,
    pub writer: OwnedWriteHalf,
    next_id: u64,
}

impl Raw {
    pub async fn connect(socket: &Path) -> Self {
        let (rd, writer) = UnixStream::connect(socket).await.unwrap().into_split();
        Self {
            lines: AsyncBufReader::new(rd).lines(),
            writer,
            next_id: 0,
        }
    }

    pub async fn send_line(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    /// Sends a request without waiting; returns its id.
    pub async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let request = json!({"id": self.next_id, "method": method, "params": params});
        self.send_line(&request.to_string()).await;
        self.next_id
    }

    /// The next non-record message.
    pub async fn message(&mut self) -> Value {
        loop {
            let line = tokio::time::timeout(TIMEOUT, self.lines.next_line())
                .await
                .expect("timed out")
                .unwrap()
                .expect("connection closed");
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg.get("record").is_none() {
                return msg;
            }
        }
    }

    /// Sends a request and returns its whole response message.
    pub async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        let msg = self.message().await;
        assert_eq!(msg["id"], id, "{msg}");
        msg
    }
}
