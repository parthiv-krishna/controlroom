//! The supervisor runtime: child process, transcript, socket server, signals.
//!
//! Threads do the blocking I/O (two output pumps, the stdin logger and writer, the
//! reaper; for a terminal one pump and the input writer); a current-thread tokio
//! runtime serves the socket, timers and signals, and runs fsyncs and transcript
//! reads on its blocking pool.

mod child;
mod conn;
mod log;
mod pty;
mod pump;
mod stdin;

use crate::diag::diag;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, bail};
use controlroom_transcript::{
    Body, ExitInfo, FORMAT_VERSION, Meta, ProcessKind, StdinMode, StopCause, Writer, unix_ms_now,
};
use rustix::process::Signal;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot, watch};

use self::child::Group;
use self::log::{AppendError, Log};
use self::stdin::LogOp;
use crate::paths::Paths;
use crate::proto::{PROTOCOL_VERSION, ReadyLine, SupervisorInfo, VERSION};
use crate::spec::{SpawnSpec, read_env_file};

/// Grace period between SIGTERM and SIGKILL when none is given.
pub(crate) const DEFAULT_GRACE: Duration = Duration::from_secs(5);
/// How long to keep draining stdout/stderr after the child exits (processes that
/// left the group may still hold the pipes open).
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long active readers get to receive the exit record before shutdown.
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
/// Backstop fsync interval for output records.
const SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// How long to retry writing the exit record on a failing disk.
const EXIT_RECORD_ATTEMPTS: u32 = 60;
/// Queued write and barrier operations.
const OPS_QUEUE: usize = 256;
/// Lines handed to the stdin writer but not yet written.
const STDIN_QUEUE: usize = 256;

/// What to supervise and where.
pub(crate) struct Config {
    pub id: String,
    pub spec: SpawnSpec,
    pub paths: Paths,
}

/// Runs the supervisor until the child exits. Prints the [`ReadyLine`] to stdout.
pub(crate) fn run(config: Config) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let running = match start(config).await {
            Ok(running) => running,
            Err(e) => {
                print_ready_line(&ReadyLine::Error(format!("{e:#}")));
                return Err(e);
            }
        };
        print_ready_line(&ReadyLine::Ready(running.info.clone()));
        running.wait().await;
        Ok(())
    })
}

/// Prints the one line a supervisor ever writes to stdout. After it, stdout may be
/// a closed pipe (the launcher has exited), so nothing else is printed there.
fn print_ready_line(line: &ReadyLine) {
    if let Ok(json) = serde_json::to_string(line) {
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "{json}").and_then(|()| stdout.flush());
    }
}

/// State shared by the socket server and the lifecycle tasks.
pub(crate) struct Shared {
    pub info: SupervisorInfo,
    pub log: Arc<Log>,
    pub group: Arc<Group>,
    pub stdin: StdinMode,
    pub max_line_bytes: u64,
    pub ops: mpsc::Sender<LogOp>,
    /// Connection id holding the writer role.
    pub writer_conn: Mutex<Option<u64>>,
    /// Set once the exit record is written.
    pub exit: watch::Sender<Option<ExitInfo>>,
    /// Seq of this run's exit record (0 until written).
    pub final_seq: AtomicU64,
    /// Set when the supervisor is shutting down.
    pub shutdown: watch::Sender<bool>,
    /// Why the supervisor asked the child to stop (first request wins).
    stop_cause: Mutex<Option<StopCause>>,
    /// The terminal of a `pty` child.
    pub pty: Option<PtyHandle>,
}

/// The master side of a `pty` child's terminal.
pub(crate) struct PtyHandle {
    /// For resizing.
    pub master: OwnedFd,
    /// Raw input, written in order by the pty writer thread.
    pub input: SyncSender<pty::Input>,
}

impl Shared {
    /// Asks the child's group to stop (SIGTERM, or SIGHUP for a terminal, which is what an
    /// interactive shell honors), and SIGKILLs it after `grace` if it still runs.
    pub(crate) fn terminate(self: &Arc<Self>, grace: Duration, cause: StopCause) {
        self.stop_cause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(cause);
        let signal = if self.pty.is_some() {
            Signal::HUP
        } else {
            Signal::TERM
        };
        self.group.signal(signal);
        let shared = self.clone();
        tokio::spawn(async move {
            let mut exit = shared.exit.subscribe();
            tokio::select! {
                () = tokio::time::sleep(grace) => shared.group.signal(Signal::KILL),
                () = until_exited(&mut exit) => {}
            }
        });
    }

    pub(crate) fn exited(&self) -> Option<ExitInfo> {
        *self.exit.borrow()
    }

    fn stop_cause(&self) -> Option<StopCause> {
        *self
            .stop_cause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Undoes a partially completed start: kills the child and removes the socket.
struct StartGuard {
    socket: PathBuf,
    group: Option<Arc<Group>>,
    armed: bool,
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Some(group) = &self.group {
                group.signal(Signal::KILL);
            }
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

struct Running {
    info: SupervisorInfo,
    paths: Paths,
    shared: Arc<Shared>,
    reaped: oneshot::Receiver<std::io::Result<std::process::ExitStatus>>,
    pumps_done: Vec<oneshot::Receiver<()>>,
    conns_done: mpsc::Receiver<()>,
    conns_done_tx: mpsc::Sender<()>,
}

async fn start(config: Config) -> anyhow::Result<Running> {
    let Config { id, spec, paths } = config;
    spec.validate()?;
    paths.validate()?;
    let pid = std::process::id();
    let argv = spec.argv(std::env::var("SHELL").ok().as_deref());

    // Before anything can send us one.
    let signals = Signals {
        term: signal(SignalKind::terminate())?,
        int: signal(SignalKind::interrupt())?,
        hup: signal(SignalKind::hangup())?,
    };

    paths
        .create_dir()
        .with_context(|| format!("creating {}", paths.dir.display()))?;
    let transcript = paths.transcript();
    let (writer, report) = match Writer::open(&transcript) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            bail!(
                "{} already has a live supervisor ({e})",
                paths.dir.display()
            )
        }
        Err(e) => return Err(e).with_context(|| format!("opening {}", transcript.display())),
    };
    if report.truncated_bytes > 0 {
        diag!(
            "dropped {} bytes of an incomplete final transcript line",
            report.truncated_bytes
        );
    }

    let listener = bind(&paths).await?;
    let mut guard = StartGuard {
        socket: paths.socket.clone(),
        group: None,
        armed: true,
    };

    let env = match &spec.env_file {
        Some(path) => read_env_file(path)?,
        None => Vec::new(),
    };
    let spawned = if spec.kind == ProcessKind::Pty {
        pty::spawn(&argv, &spec.cwd, env).map(Spawned::Pty)
    } else {
        child::spawn(&argv, &spec.cwd, env, spec.stdin).map(Spawned::Pipes)
    }
    .with_context(|| format!("spawning {argv:?} in {}", spec.cwd.display()))?;
    let child_pid = spawned.child().id();
    let group = Arc::new(Group::new(child_pid));
    guard.group = Some(group.clone());

    let plain = match &spec.plain_log {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o644)
                .open(path)
                .with_context(|| format!("opening the plain log {}", path.display()))?,
        ),
        None => None,
    };
    let log = Arc::new(Log::new(writer, plain)?);
    let max_line_bytes = spec.line_cap();
    log.append(Body::Meta(Box::new(Meta {
        format: FORMAT_VERSION,
        id: id.clone(),
        kind: spec.kind,
        argv,
        cwd: spec.cwd.to_string_lossy().into_owned(),
        stdin: spec.stdin,
        timeout_ms: spec.timeout_ms,
        max_line_bytes,
        labels: spec.labels.clone(),
        child_pid,
        supervisor_pid: pid,
        protocol_version: PROTOCOL_VERSION,
        supervisor_version: VERSION.to_owned(),
    })))?;
    let sync_log = log.clone();
    tokio::task::spawn_blocking(move || sync_log.sync()).await??;

    let info = SupervisorInfo {
        id,
        kind: spec.kind,
        pid,
        child_pid,
        socket: paths.socket.clone(),
        transcript,
        protocol_version: PROTOCOL_VERSION,
        version: VERSION.to_owned(),
        started_at: unix_ms_now(),
    };
    let (ops_tx, ops_rx) = mpsc::channel(OPS_QUEUE);
    let (mut child, started) = start_io(spawned, &log, usize::try_from(max_line_bytes)?)?;
    let shared = Arc::new(Shared {
        info: info.clone(),
        log: log.clone(),
        group: group.clone(),
        stdin: spec.stdin,
        max_line_bytes,
        ops: ops_tx,
        writer_conn: Mutex::new(None),
        exit: watch::Sender::new(None),
        final_seq: AtomicU64::new(0),
        shutdown: watch::Sender::new(false),
        stop_cause: Mutex::new(None),
        pty: started.pty,
    });

    let pumps_done = started.pumps_done;
    let stdin_tx = match started.stdin {
        Some(pipe) => {
            let (tx, rx) = std::sync::mpsc::sync_channel(STDIN_QUEUE);
            spawn_thread("stdin", move || stdin::stdin_writer(pipe, rx))?;
            Some(tx)
        }
        None => None,
    };
    let logger_log = log.clone();
    spawn_thread("logger", move || {
        stdin::logger(logger_log, ops_rx, stdin_tx)
    })?;
    let (reaped_tx, reaped) = oneshot::channel();
    let reaper_log = log.clone();
    spawn_thread("reaper", move || {
        let status = group.wait_and_reap(&mut child);
        reaper_log.mark_child_reaped();
        let _ = reaped_tx.send(status);
    })?;

    tokio::spawn(handle_signals(shared.clone(), signals));
    tokio::spawn(periodic_sync(shared.clone()));
    if let Some(timeout_ms) = spec.timeout_ms {
        tokio::spawn(enforce_timeout(
            shared.clone(),
            Duration::from_millis(timeout_ms),
        ));
    }
    let (conns_done_tx, conns_done) = mpsc::channel(1);
    tokio::spawn(conn::accept_loop(
        shared.clone(),
        listener,
        conns_done_tx.clone(),
    ));

    write_info_file(&paths, &info)?;
    guard.armed = false;
    Ok(Running {
        info,
        paths,
        shared,
        reaped,
        pumps_done,
        conns_done,
        conns_done_tx,
    })
}

/// Binds the socket, replacing a stale one but never a live one.
async fn bind(paths: &Paths) -> anyhow::Result<UnixListener> {
    let socket = &paths.socket;
    if tokio::net::UnixStream::connect(socket).await.is_ok() {
        bail!("socket {} is in use by a live supervisor", socket.display());
    }
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("removing stale socket"),
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name(format!("supervise-{name}"))
        .spawn(f)
        .with_context(|| format!("spawning {name} thread"))?;
    Ok(())
}

/// A spawned child with its stdio: pipes, or a terminal.
enum Spawned {
    Pipes(child::Spawned),
    Pty(pty::PtySpawned),
}

impl Spawned {
    fn child(&self) -> &std::process::Child {
        match self {
            Spawned::Pipes(s) => &s.child,
            Spawned::Pty(s) => &s.child,
        }
    }
}

/// The child's I/O, once its threads run.
struct StartedIo {
    /// Resolve when an output pump has logged everything.
    pumps_done: Vec<oneshot::Receiver<()>>,
    /// The stdin pipe, for the stdin writer (pipes only).
    stdin: Option<std::process::ChildStdin>,
    /// The terminal (pty only).
    pty: Option<PtyHandle>,
}

/// Starts the output pumps (and, for a terminal, the input writer).
fn start_io(
    spawned: Spawned,
    log: &Arc<Log>,
    cap: usize,
) -> anyhow::Result<(std::process::Child, StartedIo)> {
    match spawned {
        Spawned::Pipes(s) => {
            let pumps_done = vec![
                spawn_pump("stdout", log, move |log| {
                    pump::pump(s.stdout, log, cap, Body::Out);
                })?,
                spawn_pump("stderr", log, move |log| {
                    pump::pump(s.stderr, log, cap, Body::Stderr);
                })?,
            ];
            let started = StartedIo {
                pumps_done,
                stdin: s.stdin,
                pty: None,
            };
            Ok((s.child, started))
        }
        Spawned::Pty(s) => {
            let reader = File::from(s.master.try_clone()?);
            let writer = File::from(s.master.try_clone()?);
            let (input, input_rx) = std::sync::mpsc::sync_channel(STDIN_QUEUE);
            spawn_thread("pty-input", move || pty::writer(writer, &input_rx))?;
            let pumps_done = vec![spawn_pump("pty", log, move |log| {
                pty::pump(reader, &log);
            })?];
            let started = StartedIo {
                pumps_done,
                stdin: None,
                pty: Some(PtyHandle {
                    master: s.master,
                    input,
                }),
            };
            Ok((s.child, started))
        }
    }
}

/// Runs an output pump on its own thread; the receiver resolves when it is done.
fn spawn_pump(
    name: &str,
    log: &Arc<Log>,
    pump: impl FnOnce(Arc<Log>) + Send + 'static,
) -> anyhow::Result<oneshot::Receiver<()>> {
    let (done_tx, done) = oneshot::channel();
    let log = log.clone();
    spawn_thread(name, move || {
        pump(log);
        let _ = done_tx.send(());
    })?;
    Ok(done)
}

fn write_info_file(paths: &Paths, info: &SupervisorInfo) -> anyhow::Result<()> {
    let path = paths.info_file();
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(info)?)?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

impl Running {
    /// Waits for the child to exit, records it, and shuts down.
    async fn wait(mut self) {
        let status = match (&mut self.reaped).await {
            Ok(Ok(status)) => Some(status),
            Ok(Err(e)) => {
                diag!("waiting for child: {e}");
                None
            }
            Err(_) => {
                diag!("reaper thread died");
                None
            }
        };
        let exit = ExitInfo {
            code: status.and_then(|s| s.code()),
            signal: status.and_then(|s| s.signal()),
            cause: self.shared.stop_cause(),
        };

        // Log everything the child wrote before the exit record.
        let pumps = std::mem::take(&mut self.pumps_done);
        let drained = async {
            for done in pumps {
                let _ = done.await;
            }
        };
        if tokio::time::timeout(PIPE_DRAIN_TIMEOUT, drained)
            .await
            .is_err()
        {
            diag!("output pipes still open after the child exited; closing");
        }

        let shared = &self.shared;
        if let Some(record) = append_exit(&shared.log, exit).await {
            shared.final_seq.store(record, Ordering::SeqCst);
        }
        let log = shared.log.clone();
        match tokio::task::spawn_blocking(move || log.sync()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => diag!("fsync on exit: {e}"),
            Err(e) => diag!("fsync on exit: {e}"),
        }
        shared.exit.send_replace(Some(exit));

        if let Err(e) = std::fs::remove_file(&self.info.socket) {
            diag!("removing socket: {e}");
        }
        shared.shutdown.send_replace(true);

        // Every connection task holds a sender; wait for them to finish streaming.
        drop(self.conns_done_tx);
        let _ = tokio::time::timeout(READER_DRAIN_TIMEOUT, self.conns_done.recv()).await;
        remove_own_info_file(&self.paths, &self.info);
    }
}

/// Appends the exit record (closing the log), retrying on a failing disk for a
/// while. Returns its seq.
async fn append_exit(log: &Arc<Log>, exit: ExitInfo) -> Option<u64> {
    for attempt in 1..=EXIT_RECORD_ATTEMPTS {
        match log.append_final(Body::Exit(exit)) {
            Ok(record) => return Some(record.seq),
            Err(AppendError::Closed) => return None,
            Err(e) => {
                if attempt == 1 {
                    diag!("writing exit record failed, retrying: {e}");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    diag!("giving up on the exit record");
    None
}

/// Removes `supervisor.json` unless a newer supervisor already replaced it.
fn remove_own_info_file(paths: &Paths, info: &SupervisorInfo) {
    let ours = crate::paths::read_info(&paths.dir).is_some_and(|on_disk| on_disk.pid == info.pid);
    if ours {
        let _ = std::fs::remove_file(paths.info_file());
    }
}

struct Signals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

/// SIGHUP is ignored (pi exits on HUP, so it is never forwarded). SIGTERM/SIGINT
/// terminate the child with the default grace; a second one kills its group.
async fn handle_signals(shared: Arc<Shared>, mut signals: Signals) {
    let mut terminating = false;
    loop {
        tokio::select! {
            _ = signals.term.recv() => {}
            _ = signals.int.recv() => {}
            _ = signals.hup.recv() => {
                diag!("ignoring SIGHUP");
                continue;
            }
        }
        if terminating {
            diag!("second termination signal, killing the child");
            shared.group.signal(Signal::KILL);
        } else {
            diag!("termination signal, terminating the child");
            terminating = true;
            shared.terminate(DEFAULT_GRACE, StopCause::Signal);
        }
    }
}

/// Backstop: output records are fsynced within [`SYNC_INTERVAL`].
async fn periodic_sync(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(SYNC_INTERVAL);
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            () = until_shutdown(&mut shutdown) => return,
        }
        if shared.log.last_seq() > shared.log.durable_seq() {
            let log = shared.log.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || log.sync()).await {
                diag!("fsync: {e}");
            }
        }
    }
}

async fn enforce_timeout(shared: Arc<Shared>, timeout: Duration) {
    let mut exit = shared.exit.subscribe();
    tokio::select! {
        () = tokio::time::sleep(timeout) => {
            diag!("timeout of {timeout:?} reached, terminating the child");
            shared.terminate(DEFAULT_GRACE, StopCause::Timeout);
        }
        () = until_exited(&mut exit) => {}
    }
}

/// Resolves once shutdown starts (or the sender is gone).
pub(crate) async fn until_shutdown(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|&s| s).await;
}

/// Resolves once the exit record is written (or the sender is gone).
pub(crate) async fn until_exited(rx: &mut watch::Receiver<Option<ExitInfo>>) {
    let _ = rx.wait_for(Option::is_some).await;
}
