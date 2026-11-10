//! Named persistent shells (`shell.open`, `shell.close`, `shell.list`, `shell.changed`) and
//! running agent commands in them (spec §22.1, design §3.5).
//!
//! A shell is an interactive bash on a PTY supervisor (`kind: pty`, detached, under the
//! workspace's profile) with the shell integration ([`integration`]). The host daemon reads the
//! supervisor's byte records and parses the integration's markers ([`parser`]): it knows the
//! cwd (OSC 7), when the shell waits at a prompt, and where an agent command's output starts
//! and ends with its exit status (OSC 133). The user's terminal (`controlroomd pty attach`)
//! attaches to the same supervisor.
//!
//! - **One command at a time** per shell ([`Shell::lock_run`]).
//! - **Gone and back.** A shell whose process exits (`exit`, a crash, a host reboot that took
//!   its supervisor) is `gone`; the next command (or `shell.open`) reopens it in its last cwd,
//!   and that command's result says its variables were lost (`reset_pending` until then).
//! - **Daemon restarts** keep shells: their supervisors are detached. The new daemon reattaches
//!   and, for a command that was running, re-reads the output from where it started.
//! - **The user's own terminals** ([`ShellOwner::User`]) are shells too, with the same
//!   supervisor and rcfile, under the workspace's profile for user terminals (its approvals are
//!   marked as the user's) or, `unsandboxed`, without nono. The agent's commands never run in
//!   them ([`Shells::require`] doesn't find them), and they are never reopened: when their
//!   process ends, or a restart finds their supervisor gone, they are forgotten.

pub mod integration;
mod output;
mod parser;
pub(crate) mod proc;
pub mod run;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use controlroom_protocol::host::{
    HostShell, ShellChangedParams, ShellCloseParams, ShellListParams, ShellListResult,
    ShellOpenParams, notifications,
};
use controlroom_protocol::ids::{CallId, SessionId, ShellId, WorkspaceId};
use controlroom_protocol::session::{
    DeviceRef, Shell as WireShell, ShellOwner, ShellSandbox, ShellState,
};
use controlroom_protocol::{ErrorData, RemotePath, RpcError};
use controlroom_sandbox::AbsPath;
use controlroom_supervisor::{Client, Exec, LaunchRequest, Paths, RecordStream, SpawnSpec, launch};
use controlroom_transcript::{Body, ExitInfo, ProcessKind, PtyChunk, StdinMode};
use rusqlite::params;
use tokio::sync::{oneshot, watch};

pub use run::{BashRun, Resume};

use self::output::Output;
use self::parser::{Event, Parser};
use super::Host;
use super::sandbox::Wrapped;
use super::store::internal;
use crate::dirs::{create_private_dir, write_private_file};

/// How long a new shell may take to show its first prompt.
const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a restarting shell may take to end (the supervisor kills it 5 s after SIGTERM).
const RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How an agent command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The shell showed its prompt again, with the command's exit status.
    Prompt(Option<i32>),
    /// The shell itself exited (e.g. the command ran `exit`), with the supervisor's record of
    /// it; `None` when the supervisor vanished without one.
    Exited(Option<ExitInfo>),
}

/// The running agent command of a shell.
struct Active {
    call_id: CallId,
    marker: String,
    started: bool,
    exit: Option<i32>,
    output: Output,
    last_output: Instant,
    waiting: bool,
    waited: bool,
    timed_out: bool,
    cancelled: bool,
    done: Option<oneshot::Sender<End>>,
}

/// The shell's supervisor, while it runs.
struct Proc {
    generation: u64,
    child_pid: u32,
    /// The shell itself (the supervisor's child may be a sandbox wrapper), from its prompt.
    shell_pid: Option<u32>,
    control: Arc<Client>,
}

struct Inner {
    cwd: String,
    reset_reason: Option<String>,
    /// Why the shell is being restarted: its reset notice once its process has ended.
    restart_reason: Option<String>,
    foreground: Option<String>,
    proc: Option<Proc>,
    active: Option<Active>,
    generation: u64,
}

/// One named shell.
pub struct Shell {
    /// `<session>/<name>`.
    pub id: ShellId,
    /// Its name within the session.
    pub name: String,
    /// The session.
    pub session_id: SessionId,
    /// The workspace it runs in.
    pub workspace_id: WorkspaceId,
    /// Whose shell it is.
    pub owner: ShellOwner,
    /// How it is confined.
    pub sandbox: ShellSandbox,
    inner: Mutex<Inner>,
    run_lock: Arc<tokio::sync::Mutex<()>>,
    /// True while the shell waits at its prompt with no agent command pending.
    ready: watch::Sender<bool>,
    /// The generation of the shell's last process that ended.
    exited: watch::Sender<u64>,
}

impl Shell {
    fn new(key: Key, cwd: String, reset_reason: Option<String>) -> Self {
        Self {
            id: key.id,
            name: key.name,
            session_id: key.session_id,
            workspace_id: key.workspace_id,
            owner: key.owner,
            sandbox: key.sandbox,
            inner: Mutex::new(Inner {
                cwd,
                reset_reason,
                restart_reason: None,
                foreground: None,
                proc: None,
                active: None,
                generation: 0,
            }),
            run_lock: Arc::new(tokio::sync::Mutex::new(())),
            ready: watch::Sender::new(false),
            exited: watch::Sender::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this is one of the user's terminals (an unknown owner counts as the user's).
    pub fn is_user(&self) -> bool {
        self.owner != ShellOwner::Agent
    }

    /// Serializes agent commands: held while one runs.
    pub async fn lock_run(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.run_lock.clone().lock_owned().await
    }

    /// The shell as the protocol shows it.
    pub fn describe(&self) -> HostShell {
        let inner = self.lock();
        let state = match (&inner.proc, &inner.active) {
            (None, _) => ShellState::Gone,
            (Some(_), Some(a)) if a.waiting => ShellState::WaitingInput {
                call_id: a.call_id.clone(),
            },
            (Some(_), Some(a)) => ShellState::Running {
                call_id: a.call_id.clone(),
            },
            (Some(_), None) => ShellState::Idle,
        };
        HostShell {
            shell_id: self.id.clone(),
            shell: WireShell {
                name: self.name.clone(),
                workspace_id: self.workspace_id,
                owner: self.owner,
                sandbox: self.sandbox,
                cwd: RemotePath::from(inner.cwd.as_str()),
                state,
                reset_pending: inner.reset_reason.is_some(),
                foreground: inner.foreground.clone(),
            },
        }
    }

    /// The shell's cwd.
    pub fn cwd(&self) -> String {
        self.lock().cwd.clone()
    }

    fn is_running(&self) -> bool {
        self.lock().proc.is_some()
    }

    /// Starts the shell's process if it isn't running: in `cwd` (default its last one). The
    /// caller holds [`lock_run`](Self::lock_run) or is opening it.
    async fn ensure_running(
        self: &Arc<Self>,
        host: &Arc<Host>,
        cwd: Option<String>,
    ) -> Result<(), RpcError> {
        if self.is_running() {
            return Ok(());
        }
        let cwd = cwd.unwrap_or_else(|| self.cwd());
        let (control, stream, child_pid) = spawn(host, self, &cwd).await?;
        let generation = {
            let mut inner = self.lock();
            inner.generation += 1;
            inner.cwd = cwd;
            inner.proc = Some(Proc {
                generation: inner.generation,
                child_pid,
                shell_pid: None,
                control: Arc::new(control),
            });
            inner.generation
        };
        self.ready.send_replace(false);
        // Saved before anything can attach: `shell.open` answers once the prompt shows, and a
        // `pty attach` right after finds the supervisor in the store. Saved before the read loop
        // starts, so no later save (e.g. with the shell's pid) can be overtaken by this one.
        self.save(host).await;
        tokio::spawn(read_loop(host.clone(), self.clone(), generation, stream));
        host.shells.changed(host, self);
        let mut ready = self.ready.subscribe();
        match tokio::time::timeout(START_TIMEOUT, ready.wait_for(|r| *r)).await {
            Ok(Ok(_)) => Ok(()),
            _ => Err(internal(format!(
                "shell {} showed no prompt within {}s",
                self.name,
                START_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Handles one integration event from the shell's output.
    fn on_event(&self, host: &Arc<Host>, event: Event) {
        let mut changed = false;
        {
            let mut inner = self.lock();
            match event {
                Event::Started(marker) => {
                    if let Some(active) = inner.active.as_mut()
                        && active.marker == marker
                    {
                        active.started = true;
                        changed = true;
                    }
                }
                Event::Output(bytes) => {
                    if let Some(active) = inner.active.as_mut().filter(|a| a.started) {
                        active.output.push(&bytes);
                        active.last_output = Instant::now();
                        if active.waiting {
                            active.waiting = false;
                            host.tools.waiting(&active.call_id, false);
                            changed = true;
                        }
                    }
                }
                Event::Ended(status) => {
                    if let Some(active) = inner.active.as_mut().filter(|a| a.started) {
                        active.exit = status;
                    }
                }
                Event::Cwd(cwd) => {
                    if inner.cwd != cwd {
                        inner.cwd = cwd;
                        changed = true;
                    }
                }
                Event::Prompt(pid) => {
                    if let Some(proc) = inner.proc.as_mut()
                        && pid.is_some()
                        && proc.shell_pid != pid
                    {
                        proc.shell_pid = pid;
                        changed = true;
                    }
                    // The runner takes the finished command (and its output) out of `active`.
                    if let Some(active) = inner
                        .active
                        .as_mut()
                        .filter(|a| a.started && a.exit.is_some())
                        && let Some(done) = active.done.take()
                    {
                        let _ = done.send(End::Prompt(active.exit));
                        inner.foreground = None;
                        changed = true;
                    }
                    if inner.active.is_none() {
                        self.ready.send_replace(true);
                    }
                }
            }
        }
        if changed {
            self.persist(host);
            host.shells.changed(host, self);
        }
    }

    /// The supervisor of `generation` ended (with its exit record, if it wrote one). A user's
    /// terminal is forgotten; an agent shell is `gone` until it is reopened.
    fn on_exit(self: &Arc<Self>, host: &Arc<Host>, generation: u64, exit: Option<ExitInfo>) {
        if self.is_user() {
            let current = self
                .lock()
                .proc
                .as_ref()
                .is_some_and(|p| p.generation == generation);
            if current {
                self.lock().proc = None;
                self.ready.send_replace(false);
                host.shells.forget(host, self, "its shell exited");
            }
            return;
        }
        {
            let mut inner = self.lock();
            if inner
                .proc
                .as_ref()
                .is_none_or(|p| p.generation != generation)
            {
                return;
            }
            inner.proc = None;
            inner.foreground = None;
            let status = match exit {
                Some(ExitInfo {
                    code: Some(code), ..
                }) => format!("exited with status {code}"),
                Some(ExitInfo {
                    signal: Some(signal),
                    ..
                }) => format!("was killed by signal {signal}"),
                _ => "is gone (its supervisor died, e.g. the host restarted)".to_owned(),
            };
            // Why it is reopened; the extension says what that means for the agent.
            let reason = inner.restart_reason.take();
            inner.reset_reason = Some(reason.unwrap_or_else(|| format!("the shell {status}")));
            if let Some(active) = inner.active.as_mut()
                && let Some(done) = active.done.take()
            {
                let _ = done.send(End::Exited(exit));
            }
        }
        self.ready.send_replace(false);
        self.exited.send_replace(generation);
        self.persist(host);
        host.shells.changed(host, self);
    }

    /// Restarts a running agent shell under its workspace's current profile, in its last
    /// directory: its process ends (and with it a running command), and the next command's
    /// result gives `reason` as its reset notice. A shell that isn't running starts under the
    /// current profile anyway when it is next used.
    async fn restart(self: &Arc<Self>, host: &Arc<Host>, reason: &str) -> Result<(), RpcError> {
        let generation = {
            let mut inner = self.lock();
            let Some(proc) = inner.proc.as_ref() else {
                return Ok(());
            };
            let generation = proc.generation;
            inner.restart_reason = Some(reason.to_owned());
            generation
        };
        self.terminate().await;
        let mut exited = self.exited.subscribe();
        let ended = exited.wait_for(|g| *g >= generation);
        if tokio::time::timeout(RESTART_TIMEOUT, ended).await.is_err() {
            return Err(internal(format!(
                "shell {} didn't end within {}s",
                self.name,
                RESTART_TIMEOUT.as_secs()
            )));
        }
        let _turn = self.lock_run().await;
        self.ensure_running(host, None).await
    }

    /// Saves the cwd, supervisor and reset state (in the background).
    fn persist(&self, host: &Arc<Host>) {
        let save = self.save_op(host);
        tokio::spawn(save);
    }

    /// Saves the cwd, supervisor and reset state, and waits for it.
    async fn save(&self, host: &Arc<Host>) {
        self.save_op(host).await;
    }

    /// The save, as of now.
    fn save_op(&self, host: &Arc<Host>) -> impl std::future::Future<Output = ()> + Send + 'static {
        let (id, cwd, reset, supervisor, shell_pid) = {
            let inner = self.lock();
            (
                self.id.to_string(),
                inner.cwd.clone(),
                inner.reset_reason.clone(),
                inner.proc.as_ref().map(|p| p.control.hello().id.clone()),
                inner.proc.as_ref().and_then(|p| p.shell_pid),
            )
        };
        let store = host.store.clone();
        async move {
            let saved = store
                .run(move |c| {
                    c.execute(
                        "UPDATE shells SET cwd = ?2, reset_reason = ?3, supervisor = ?4, shell_pid = ?5
                         WHERE id = ?1",
                        params![id, cwd, reset, supervisor, shell_pid],
                    )
                })
                .await;
            if let Err(e) = saved {
                tracing::warn!("saving a shell: {}", e.message);
            }
        }
    }

    /// Ends the shell (SIGHUP to its group, SIGKILL after the grace).
    async fn terminate(&self) {
        let control = self.lock().proc.as_ref().map(|p| p.control.clone());
        if let Some(control) = control
            && let Err(e) = control.terminate(None).await
        {
            tracing::warn!("closing shell {}: {e}", self.id);
        }
    }
}

/// Starts a shell supervisor; returns its control connection, a record stream from its start
/// and its child pid.
async fn spawn(
    host: &Arc<Host>,
    shell: &Shell,
    cwd: &str,
) -> Result<(Client, Reading, u32), RpcError> {
    let workspace = host.workspaces.require(&shell.workspace_id)?;
    let argv = vec![
        host.shells.bash.to_string(),
        "--noprofile".to_owned(),
        "--rcfile".to_owned(),
        workspace.files.bashrc().to_string(),
        "-i".to_owned(),
    ];
    let wrapped = match (shell.owner, shell.sandbox) {
        (ShellOwner::Agent, _) => workspace.wrap(host, argv)?,
        (_, ShellSandbox::Workspace) => workspace.wrap_user(host, argv)?,
        // Only a confirmed request of the user's opens an unsandboxed terminal (see `open`).
        (_, ShellSandbox::Unsandboxed | ShellSandbox::Unknown) => Wrapped {
            argv,
            env: Vec::new(),
        },
    };
    let supervisor = ulid::Ulid::generate().to_string();
    let dir = host.dirs.shell_dir(&supervisor);
    create_private_dir(&dir).map_err(internal)?;
    let mut env = vec![
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("CR_CWD".to_owned(), cwd.to_owned()),
    ];
    env.extend(wrapped.env);
    let env_file = dir.join("launch.env");
    let text: String = env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    write_private_file(&env_file, text.as_bytes()).map_err(internal)?;
    create_private_dir(&host.dirs.run()).map_err(internal)?;
    let request = LaunchRequest {
        id: supervisor.clone(),
        paths: Paths {
            dir: dir.as_path().to_owned(),
            socket: host
                .dirs
                .supervisor_socket(&supervisor)
                .as_path()
                .to_owned(),
        },
        spec: SpawnSpec {
            kind: ProcessKind::Pty,
            exec: Exec::Argv(wrapped.argv),
            cwd: workspace.root.as_path().to_owned(),
            env_file: Some(env_file.as_path().to_owned()),
            stdin: StdinMode::Pipe,
            timeout_ms: None,
            max_line_bytes: None,
            labels: BTreeMap::from([("shell".to_owned(), shell.id.to_string())]),
            plain_log: None,
        },
    };
    let info = launch(host.exe.as_path(), &request)
        .await
        .map_err(|e| internal(format!("starting shell {}: {e}", shell.name)))?;
    let (control, reader, stream) = connect(&info.socket, 1).await?;
    Ok((
        control,
        Reading {
            _reader: reader,
            stream,
        },
        info.child_pid,
    ))
}

/// A control connection (the writer: it may terminate), and a reader connection with its
/// record stream from `from_seq`.
async fn connect(
    socket: &std::path::Path,
    from_seq: u64,
) -> Result<(Client, Client, RecordStream), RpcError> {
    let control = Client::connect(socket, true)
        .await
        .map_err(|e| internal(format!("connecting to a shell supervisor: {e}")))?;
    let reader = Client::connect(socket, false)
        .await
        .map_err(|e| internal(format!("connecting to a shell supervisor: {e}")))?;
    let stream = reader
        .read(from_seq)
        .await
        .map_err(|e| internal(format!("reading a shell: {e}")))?;
    Ok((control, reader, stream))
}

/// A record stream and the connection it arrives on (dropping the connection ends it).
struct Reading {
    _reader: Client,
    stream: RecordStream,
}

/// Feeds a shell's records to its parser until the supervisor ends.
async fn read_loop(host: Arc<Host>, shell: Arc<Shell>, generation: u64, mut reading: Reading) {
    let mut parser = Parser::default();
    while let Some(record) = reading.stream.next().await {
        if let Some(chunk) = PtyChunk::from_record(&record) {
            for event in parser.feed(&chunk.0) {
                shell.on_event(&host, event);
            }
        } else if let Body::Exit(exit) = record.body {
            shell.on_exit(&host, generation, Some(exit));
            return;
        }
    }
    shell.on_exit(&host, generation, None);
}

/// What identifies a shell and doesn't change while it lives.
struct Key {
    id: ShellId,
    name: String,
    session_id: SessionId,
    workspace_id: WorkspaceId,
    owner: ShellOwner,
    sandbox: ShellSandbox,
}

/// A shell enum as stored in its text column (its wire name).
fn enum_text<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A shell enum from its text column (`Unknown` for a value this build doesn't know).
fn enum_parse<T: serde::de::DeserializeOwned>(text: String) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(text)).ok()
}

/// `shell_gone` for `name`.
fn gone(name: &str) -> RpcError {
    RpcError::new(
        ErrorData::ShellGone {
            shell: name.to_owned(),
        },
        format!("there is no shell {name:?}; open it first"),
    )
}

/// The device of a user's terminal, for the log.
fn device(by: Option<&DeviceRef>) -> String {
    by.map_or_else(
        || "an unknown device".to_owned(),
        |d| format!("{} ({})", d.label, d.device_id),
    )
}

/// The host's shells.
pub struct Shells {
    map: Mutex<HashMap<ShellId, Arc<Shell>>>,
    /// The bash agent shells run.
    pub bash: AbsPath,
}

impl Shells {
    /// No shells yet; agent shells run `bash`.
    pub fn new(bash: AbsPath) -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            bash,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ShellId, Arc<Shell>>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The agent shell named `name` in `session`, for an agent command; `shell_gone` if there
    /// is none, or it is one of the user's terminals (the agent can't reach those).
    pub fn require(&self, session: SessionId, name: &str) -> Result<Arc<Shell>, RpcError> {
        self.lock()
            .get(&ShellId::of(session, name))
            .filter(|s| !s.is_user())
            .cloned()
            .ok_or_else(|| gone(name))
    }

    /// Forgets a user's terminal whose process ended: drops it from the map and the store and
    /// tells the controller it is `gone`.
    fn forget(&self, host: &Arc<Host>, shell: &Arc<Shell>, why: &str) {
        let removed = {
            let mut map = self.lock();
            match map.get(&shell.id) {
                Some(current) if Arc::ptr_eq(current, shell) => map.remove(&shell.id).is_some(),
                _ => false,
            }
        };
        if !removed {
            return;
        }
        tracing::info!(shell = %shell.id, "user terminal ended: {why}");
        let id = shell.id.to_string();
        let store = host.store.clone();
        tokio::spawn(async move {
            let deleted = store
                .run(move |c| c.execute("DELETE FROM shells WHERE id = ?1", [id]))
                .await;
            if let Err(e) = deleted {
                tracing::warn!("forgetting a user terminal: {}", e.message);
            }
        });
        let mut ended = shell.describe();
        ended.shell.state = ShellState::Gone;
        if let Some(controller) = host.conns.controller() {
            controller.notify::<notifications::ShellChanged>(ShellChangedParams { shell: ended });
        }
    }

    /// Tells the controller a shell changed.
    fn changed(&self, host: &Arc<Host>, shell: &Shell) {
        if let Some(controller) = host.conns.controller() {
            controller.notify::<notifications::ShellChanged>(ShellChangedParams {
                shell: shell.describe(),
            });
        }
    }

    /// `shell.open`: idempotent by name; a gone agent shell is reopened in its last directory
    /// (or `cwd`) and keeps `reset_pending`. A user's terminal is started once (and forgotten
    /// when it ends); opening it is logged with its device.
    pub async fn open(
        &self,
        host: &Arc<Host>,
        params: ShellOpenParams,
    ) -> Result<HostShell, RpcError> {
        let workspace = host.workspaces.require(&params.workspace_id)?;
        if params.name.is_empty() || params.name.contains('/') {
            return Err(RpcError::new(
                ErrorData::InvalidParams,
                "a shell name is non-empty and has no '/'",
            ));
        }
        if params.owner == ShellOwner::Agent && params.sandbox != ShellSandbox::Workspace {
            return Err(RpcError::new(
                ErrorData::InvalidParams,
                "agent shells run in the workspace's sandbox",
            ));
        }
        let id = ShellId::of(workspace.session_id, &params.name);
        let start = params.cwd.as_ref().map(|c| {
            let ctx = super::fsops::Context {
                root: workspace.root.to_string(),
                home: host.dirs.home().to_string(),
            };
            ctx.resolve(c.as_str())
        });
        let shell = {
            let mut map = self.lock();
            map.entry(id.clone())
                .or_insert_with(|| {
                    Arc::new(Shell::new(
                        Key {
                            id: id.clone(),
                            name: params.name.clone(),
                            session_id: workspace.session_id,
                            workspace_id: workspace.id,
                            owner: params.owner,
                            sandbox: params.sandbox,
                        },
                        start.clone().unwrap_or_else(|| workspace.root.to_string()),
                        None,
                    ))
                })
                .clone()
        };
        if shell.owner != params.owner || shell.sandbox != params.sandbox {
            // Never hand one owner's shell to the other.
            return Err(gone(&params.name));
        }
        if shell.workspace_id != workspace.id {
            return Err(RpcError::new(
                ErrorData::InvalidParams,
                format!("shell {:?} belongs to another workspace", params.name),
            ));
        }
        let (row_id, session, name, ws, cwd) = (
            id.to_string(),
            workspace.session_id.to_string(),
            params.name.clone(),
            workspace.id.to_string(),
            shell.cwd(),
        );
        let (owner, sandbox) = (enum_text(shell.owner), enum_text(shell.sandbox));
        host.store
            .run(move |c| {
                c.execute(
                    "INSERT OR IGNORE INTO shells (id, session_id, name, workspace_id, cwd, owner,
                     sandbox) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![row_id, session, name, ws, cwd, owner, sandbox],
                )
            })
            .await?;
        // A live shell is returned as it is, even while a command runs in it.
        if !shell.is_running() {
            let _turn = shell.lock_run().await;
            let started = shell.ensure_running(host, start).await;
            if shell.is_user() {
                match &started {
                    Ok(()) => tracing::info!(
                        shell = %shell.id,
                        sandbox = %enum_text(shell.sandbox),
                        device = %device(params.by.as_ref()),
                        "user terminal opened"
                    ),
                    Err(e) => {
                        // Not reopened: a failed start leaves nothing behind.
                        shell.terminate().await;
                        self.forget(host, &shell, &format!("it didn't start: {}", e.message));
                    }
                }
            }
            started?;
        }
        Ok(shell.describe())
    }

    /// `shell.close`: ends the shell and forgets it. Idempotent.
    pub async fn close(&self, host: &Arc<Host>, params: ShellCloseParams) -> Result<(), RpcError> {
        let shell = self.lock().remove(&params.shell_id);
        let id = params.shell_id.to_string();
        host.store
            .run(move |c| c.execute("DELETE FROM shells WHERE id = ?1", [id]))
            .await?;
        if let Some(shell) = shell {
            shell.terminate().await;
            let mut gone = shell.describe();
            gone.shell.state = ShellState::Gone;
            if let Some(controller) = host.conns.controller() {
                controller
                    .notify::<notifications::ShellChanged>(ShellChangedParams { shell: gone });
            }
        }
        Ok(())
    }

    /// `shell.list`.
    pub fn list(&self, params: &ShellListParams) -> ShellListResult {
        let mut shells: Vec<HostShell> = self
            .lock()
            .values()
            .filter(|s| params.workspace_id.is_none_or(|w| s.workspace_id == w))
            .map(|s| s.describe())
            .collect();
        shells.sort_by(|a, b| a.shell_id.as_str().cmp(b.shell_id.as_str()));
        ShellListResult { shells }
    }

    /// Restarts the agent's shells of `workspace` under its current profile, all at once (after
    /// a level change; see [`Shell::restart`]). The user's terminals keep running under the
    /// profile they started with until they end.
    pub async fn restart_agent_shells(
        &self,
        host: &Arc<Host>,
        workspace: &WorkspaceId,
        reason: &str,
    ) {
        let shells: Vec<Arc<Shell>> = self
            .lock()
            .values()
            .filter(|s| s.workspace_id == *workspace && !s.is_user())
            .cloned()
            .collect();
        let mut restarts = tokio::task::JoinSet::new();
        for shell in shells {
            let (host, reason) = (host.clone(), reason.to_owned());
            restarts.spawn(async move {
                if let Err(e) = shell.restart(&host, &reason).await {
                    tracing::warn!(shell = %shell.id, "restarting a shell: {}", e.message);
                }
            });
        }
        restarts.join_all().await;
    }

    /// Ends every shell of a closing workspace.
    pub async fn close_workspace(&self, host: &Arc<Host>, workspace: &WorkspaceId) {
        let ids: Vec<ShellId> = self
            .lock()
            .values()
            .filter(|s| s.workspace_id == *workspace)
            .map(|s| s.id.clone())
            .collect();
        for shell_id in ids {
            if let Err(e) = self.close(host, ShellCloseParams { shell_id }).await {
                tracing::warn!("closing a shell: {}", e.message);
            }
        }
    }

    /// Loads the persisted shells and reattaches the ones whose supervisor survived. `resumes`
    /// are the agent commands that were running, by shell; the returned map holds the ones
    /// whose shell survived (the others are lost).
    pub async fn restore(
        &self,
        host: &Arc<Host>,
        mut resumes: HashMap<ShellId, Resume>,
    ) -> Result<HashMap<CallId, BashRun>, RpcError> {
        let rows = host
            .store
            .run(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, session_id, name, workspace_id, supervisor, cwd, reset_reason,
                     shell_pid, owner, sandbox FROM shells",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        (
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(8)?,
                            r.get::<_, String>(9)?,
                        ),
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<u32>>(7)?,
                    ))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await?;
        let mut adopted = HashMap::new();
        for ((id, session, name, workspace, owner, sandbox), supervisor, cwd, reset, shell_pid) in
            rows
        {
            let (Ok(session_id), Ok(workspace_id), Some(owner), Some(sandbox)) = (
                session.parse(),
                workspace.parse(),
                enum_parse(owner),
                enum_parse(sandbox),
            ) else {
                continue;
            };
            let id = ShellId::new(id);
            let key = Key {
                id: id.clone(),
                name,
                session_id,
                workspace_id,
                owner,
                sandbox,
            };
            let shell = Arc::new(Shell::new(key, cwd, reset));
            self.lock().insert(id.clone(), shell.clone());
            let resume = resumes.remove(&id);
            let attached = match &supervisor {
                Some(supervisor) => {
                    reattach(host, &shell, supervisor, shell_pid, resume.as_ref()).await
                }
                None => None,
            };
            if shell.is_user() {
                // Never respawned: without its supervisor it is gone for good.
                if attached.is_none() {
                    self.forget(host, &shell, "the host restarted, and its process with it");
                }
                continue;
            }
            match (attached, resume) {
                (Some(Some(run)), Some(resume)) => {
                    adopted.insert(resume.call_id, run);
                }
                (None, _) if supervisor.is_some() => {
                    // Its supervisor died with the host: gone until the next command.
                    shell.lock().reset_reason =
                        Some("the host restarted, and the shell's process with it".to_owned());
                    shell.persist(host);
                }
                _ => {}
            }
        }
        Ok(adopted)
    }
}

/// Reconnects to a surviving shell supervisor. `None` if it is gone; `Some(run)` with the
/// resumed command, if there was one.
async fn reattach(
    host: &Arc<Host>,
    shell: &Arc<Shell>,
    supervisor: &str,
    shell_pid: Option<u32>,
    resume: Option<&Resume>,
) -> Option<Option<BashRun>> {
    let socket = host.dirs.supervisor_socket(supervisor);
    let probe = Client::connect(socket.as_path(), false).await.ok()?;
    let status = probe.status().await.ok()?;
    if !status.alive {
        return None;
    }
    let from = resume.map_or(status.last_seq + 1, |r| r.start_seq);
    let (control, reader, stream) = connect(socket.as_path(), from).await.ok()?;
    let reading = Reading {
        _reader: reader,
        stream,
    };
    let mut run = None;
    let generation = {
        let mut inner = shell.lock();
        inner.generation += 1;
        inner.proc = Some(Proc {
            generation: inner.generation,
            child_pid: status.child_pid,
            shell_pid,
            control: Arc::new(control),
        });
        if let Some(resume) = resume {
            run = Some(run::adopt(host, shell, &mut inner, resume));
        }
        inner.generation
    };
    // An idle shell already showed its prompt.
    shell.ready.send_replace(resume.is_none());
    tokio::spawn(read_loop(host.clone(), shell.clone(), generation, reading));
    Some(run)
}
