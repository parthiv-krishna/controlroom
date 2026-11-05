//! A real `controlroomd serve` in a scratch home, and a JSON-RPC client for it.
//!
//! Dropping a [`HostEnv`] SIGKILLs every process whose command line mentions the scratch dir
//! or whose cwd is inside it (daemon, supervisors, shells, jobs, fs workers), scanning until
//! two scans in a row find nothing, pass or fail.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use controlroom_protocol::agentd::{ClientRole, HelloParams};
use controlroom_protocol::host::{
    HostHelloResult, HostOp, HostResult, ShellOpenParams, ToolRunParams, WorkspaceOpenParams,
    WorkspaceOpenResult, methods,
};
use controlroom_protocol::ids::{CallId, ControllerId, SessionId, WorkspaceId};
use controlroom_protocol::jsonrpc::{Method, RpcRequest};
use controlroom_protocol::session::{SandboxLevel, SandboxSpec};
use controlroom_protocol::{HOST_PROTOCOL_VERSION, RpcError};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

pub const BIN: &str = env!("CARGO_BIN_EXE_controlroomd");
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// A scratch home with a host daemon.
pub struct HostEnv {
    pub dir: tempfile::TempDir,
    /// The daemon's `$SHELL` (jobs run in it): a stand-in by default.
    pub login_shell: PathBuf,
    /// The `controlroomd` that runs: the build, or its installed copy.
    bin: PathBuf,
    daemon: Option<Child>,
}

impl HostEnv {
    /// A scratch home (no daemon yet).
    pub fn new() -> Self {
        Self::new_in("/tmp")
    }

    /// A scratch home under `base` (real-nono tests use `/var/tmp`: nono's system groups grant
    /// `/tmp`, which would hide confinement).
    pub fn new_in(base: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("crh")
            .tempdir_in(base)
            .unwrap();
        std::fs::create_dir_all(dir.path().join("home/project/sub")).unwrap();
        let login_shell = dir.path().join("login-shell");
        write_executable(&login_shell, LOGIN_SHELL);
        Self {
            dir,
            login_shell,
            bin: PathBuf::from(BIN),
            daemon: None,
        }
    }

    /// Installs the built daemon and `nono` (a file) as the controller does, and runs the
    /// installed daemon from now on:
    /// `versions/<ver>/{controlroomd, nono -> ../../nono/<nono_version>/nono}`.
    pub fn install(&mut self, nono: &Path, nono_version: &str) -> PathBuf {
        let base = self.home().join(".local/share/controlroom");
        let version = base.join(concat!("versions/", env!("CARGO_PKG_VERSION")));
        let installed_nono = base.join("nono").join(nono_version).join("nono");
        // Copied by a child: writing an executable from this process could race with
        // concurrent forks (ETXTBSY).
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(
                r#"mkdir -p "$3" "${4%/nono}" && cp "$1" "$3/controlroomd" && cp "$2" "$4" \
                && chmod 755 "$3/controlroomd" "$4" && ln -s "../../nono/$5/nono" "$3/nono""#,
            )
            .arg("sh")
            .arg(BIN)
            .arg(nono)
            .arg(&version)
            .arg(&installed_nono)
            .arg(nono_version)
            .status()
            .unwrap();
        assert!(status.success());
        self.bin = version.join("controlroomd");
        installed_nono
    }

    /// A scratch home with a running daemon.
    pub async fn start() -> Self {
        let mut env = Self::new();
        env.start_daemon().await;
        env
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    pub fn project(&self) -> PathBuf {
        self.home().join("project")
    }

    pub fn socket(&self) -> PathBuf {
        self.state().join("run/daemon.sock")
    }

    /// `controlroomd` with the scratch home.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("HOME", self.home())
            .env("SHELL", &self.login_shell)
            .env("CONTROLROOMD_LOG", "debug")
            .env_remove("PI_CODING_AGENT_DIR");
        cmd
    }

    /// Starts `serve` in the foreground and waits for its socket.
    pub async fn start_daemon(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join("host.log"))
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
        let deadline = Instant::now() + TIMEOUT;
        while UnixStream::connect(self.socket()).await.is_err() {
            assert!(Instant::now() < deadline, "the host daemon didn't come up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// SIGKILLs the daemon (shells and jobs keep running).
    pub fn kill_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// A controller connection.
    pub async fn controller(&self) -> RpcClient {
        self.connect(ClientRole::Controller {
            controller_id: controller_id(),
        })
        .await
        .0
    }

    /// A connection with `role`; also returns the hello result.
    pub async fn connect(&self, role: ClientRole) -> (RpcClient, HostHelloResult) {
        let client = RpcClient::connect(&self.socket()).await;
        let hello = client
            .call::<methods::Hello>(HelloParams {
                protocol_version: HOST_PROTOCOL_VERSION,
                client_name: "test".into(),
                role,
            })
            .await
            .unwrap();
        (client, hello)
    }

    /// The daemon's log, for failure messages.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("host.log")).unwrap_or_default()
    }
}

impl Drop for HostEnv {
    fn drop(&mut self) {
        self.kill_daemon();
        kill_everything_under(self.dir.path());
        // What killed nono processes leave (read-only trees the temp dir removal can't delete).
        controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(
            &self.dir.path().join("t"),
            Duration::ZERO,
        );
        if std::thread::panicking() {
            eprintln!("--- host.log ---\n{}", self.log());
        }
    }
}

/// A stand-in `$SHELL` for jobs: the host's real login shell may print to the terminal (e.g. a
/// title escape on logout), which would make job logs host-dependent. Agent shells still run
/// bash, found on `PATH`.
const LOGIN_SHELL: &str = "#!/bin/sh\n[ \"$1\" = -lc ] || { echo \"bad shell args: $*\" >&2; exit 99; }\nexec /bin/sh -c \"$2\"\n";

/// Writes an executable script from a child shell (written from this process, a concurrently
/// forked child could inherit the open fd and make executing it fail with ETXTBSY).
pub fn write_executable(path: &Path, content: &str) {
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

/// The one controller id the tests use.
pub fn controller_id() -> ControllerId {
    static ID: std::sync::OnceLock<ControllerId> = std::sync::OnceLock::new();
    *ID.get_or_init(ControllerId::generate)
}

/// `bash` from `PATH`.
pub fn bash() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join("bash"))
                .find(|p| p.is_file())
        })
        .expect("bash on PATH")
}

/// SIGKILLs processes that mention `dir` in their command line or run inside it.
pub fn kill_everything_under(dir: &Path) {
    let needle = dir.to_string_lossy().into_owned();
    let me = std::process::id();
    let mut quiet = 0;
    for _ in 0..200 {
        let mut found = false;
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == me {
                continue;
            }
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap_or_default();
            let mentions =
                String::from_utf8_lossy(&cmdline).contains(&needle) || cwd.starts_with(dir);
            if mentions
                && let Some(pid) = i32::try_from(pid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
            {
                found = true;
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
        quiet = if found { 0 } else { quiet + 1 };
        if quiet >= 2 {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Polls `cond` until it holds (bounded). For conditions with no event to wait on.
pub async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// A newline-delimited JSON-RPC client.
pub struct RpcClient {
    tx: mpsc::Sender<String>,
    pending: Pending,
    next_id: Mutex<i64>,
    /// Notifications as `(method, params)`.
    pub notes: tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, Value)>>,
}

impl RpcClient {
    pub async fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        let (tx, mut rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
        let pending: Pending = Arc::default();
        let (notes_tx, notes) = mpsc::unbounded_channel();
        let routing = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let msg: Value = serde_json::from_str(&line).unwrap();
                if let Some(method) = msg["method"].as_str() {
                    let _ = notes_tx.send((method.to_owned(), msg["params"].clone()));
                    continue;
                }
                let Some(id) = msg["id"].as_i64() else {
                    continue;
                };
                let reply = routing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                if let Some(reply) = reply {
                    let result = match msg.get("error") {
                        Some(error) => Err(serde_json::from_value(error.clone()).unwrap()),
                        None => Ok(msg["result"].clone()),
                    };
                    let _ = reply.send(result);
                }
            }
        });
        Self {
            tx,
            pending,
            next_id: Mutex::new(0),
            notes: tokio::sync::Mutex::new(notes),
        }
    }

    /// Sends a request; the receiver resolves with its outcome.
    pub async fn send<M: Method>(
        &self,
        params: M::Params,
    ) -> oneshot::Receiver<Result<Value, RpcError>> {
        let id = {
            let mut next = self.next_id.lock().unwrap_or_else(PoisonError::into_inner);
            *next += 1;
            *next
        };
        let (reply, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, reply);
        let line = serde_json::to_string(&RpcRequest::new::<M>(id, params)).unwrap() + "\n";
        self.tx.send(line).await.unwrap();
        rx
    }

    /// Calls `M` and decodes its result.
    pub async fn call<M: Method>(&self, params: M::Params) -> Result<M::Result, RpcError> {
        let rx = self.send::<M>(params).await;
        let value = tokio::time::timeout(TIMEOUT, rx)
            .await
            .expect("call timed out")
            .expect("connection closed")?;
        Ok(serde_json::from_value(value).unwrap())
    }

    /// The next notification named `method` (others are dropped).
    pub async fn note(&self, method: &str) -> Value {
        let mut notes = self.notes.lock().await;
        loop {
            let (name, params) = tokio::time::timeout(TIMEOUT, notes.recv())
                .await
                .unwrap_or_else(|_| panic!("no {method} notification"))
                .expect("connection closed");
            if name == method {
                return params;
            }
        }
    }
}

/// A sandbox spec at `level` with nothing else.
pub fn sandbox(level: SandboxLevel) -> SandboxSpec {
    SandboxSpec {
        level,
        grants: Vec::new(),
        domains: Vec::new(),
        mediated: Vec::new(),
    }
}

/// A session with one workspace (at `level`) on the scratch project, and a shell `name` in it.
pub struct Session {
    pub id: SessionId,
    pub workspace: WorkspaceId,
    pub opened: WorkspaceOpenResult,
}

impl Session {
    pub async fn open(env: &HostEnv, client: &RpcClient, level: SandboxLevel) -> Self {
        let id = SessionId::generate();
        let workspace = WorkspaceId::generate();
        let opened = client
            .call::<methods::WorkspaceOpen>(WorkspaceOpenParams {
                workspace_id: workspace,
                session_id: id,
                path: "~/project".into(),
                sandbox: sandbox(level),
            })
            .await
            .unwrap_or_else(|e| panic!("workspace.open: {e}\n{}", env.log()));
        Self {
            id,
            workspace,
            opened,
        }
    }

    pub async fn shell(&self, client: &RpcClient, name: &str) {
        client
            .call::<methods::ShellOpen>(ShellOpenParams {
                workspace_id: self.workspace,
                name: name.into(),
                cwd: None,
            })
            .await
            .unwrap();
    }

    pub fn run_params(&self, call: &str, op: HostOp) -> ToolRunParams {
        ToolRunParams {
            call_id: CallId::new(call),
            workspace_id: self.workspace,
            op,
        }
    }

    /// Runs `op` as call `call`.
    pub async fn run(
        &self,
        client: &RpcClient,
        call: &str,
        op: HostOp,
    ) -> Result<HostResult, RpcError> {
        client
            .call::<methods::ToolRun>(self.run_params(call, op))
            .await
    }

    /// Runs `command` in shell `shell` and returns `(exit, output, result)`.
    pub async fn bash(
        &self,
        client: &RpcClient,
        call: &str,
        shell: &str,
        command: &str,
    ) -> (Option<i32>, String, HostResult) {
        let result = self
            .run(client, call, bash_op(shell, command, None))
            .await
            .unwrap_or_else(|e| panic!("bash {command:?}: {e}"));
        let HostResult::Bash {
            exit_code,
            ref output,
            ..
        } = result
        else {
            panic!("not a bash result: {result:?}");
        };
        (exit_code, output.clone(), result)
    }
}

pub fn bash_op(shell: &str, command: &str, timeout_s: Option<u32>) -> HostOp {
    HostOp::Bash {
        shell: shell.into(),
        command: command.into(),
        timeout_s,
    }
}

/// Makes a FIFO at `path` (a gate a command blocks on until the test writes to it).
pub fn fifo(path: &Path) {
    let status = Command::new("mkfifo").arg(path).status().unwrap();
    assert!(status.success());
}

/// Opens the FIFO at `path` for writing and writes a line (releases a `cat` blocked on it).
pub async fn release(path: &Path) {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::write(path, "go\n").unwrap())
        .await
        .unwrap();
}

/// Types `text` into a shell's terminal the way a user does: `controlroomd pty attach`.
pub fn type_into(env: &HostEnv, shell_id: &str, text: &[u8]) {
    use std::io::Write as _;
    let mut attach = env
        .command()
        .args(["pty", "attach", shell_id, "--state-dir"])
        .arg(env.state())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    attach.stdin.as_mut().unwrap().write_all(text).unwrap();
    drop(attach.stdin.take());
    let status = attach.wait().unwrap();
    assert!(status.success(), "pty attach failed: {status}");
}

/// `controlroomd pty resize`.
pub fn resize(env: &HostEnv, shell_id: &str, cols: u16, rows: u16) {
    let status = env
        .command()
        .args([
            "pty",
            "resize",
            shell_id,
            &cols.to_string(),
            &rows.to_string(),
        ])
        .arg("--state-dir")
        .arg(env.state())
        .status()
        .unwrap();
    assert!(status.success());
}
