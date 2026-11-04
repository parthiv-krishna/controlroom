//! The host role of `controlroomd` (spec §5.3b, §6.5b, §22; design `docs/design/remote-tools.md`):
//! the host API server the controller reaches over SSH (`controlroomd host connect`).
//!
//! A host keeps no sessions. It knows **workspaces** (one session's directory, each with its
//! own nono profile), named persistent **shells** (PTY supervisors), a **journal** of tool
//! calls keyed by call id, **jobs** with their event log, and pending sandbox **approvals**.
//! Everything the agent triggers runs under its workspace's profile: shells, jobs, and the fs
//! worker (`controlroomd workspace-exec`) that serves the file and search ops.
//!
//! Startup: claim the instance (`run/host.lock`, `run/host.sock`), open `host.sqlite`, bind
//! the approval webhook, clean nono temp dirs that killed sandboxes leaked, restore
//! workspaces, recover the journal (reattaching shells whose supervisors survived), resume
//! following jobs, report ready, then serve. A new `epoch` marks every start. SIGTERM/SIGINT
//! stop the daemon (shells and jobs keep running under their supervisors); SIGHUP is ignored.
//!
//! - [`conn`], [`conns`], [`dispatch`]: the API surface;
//! - [`workspaces`], [`context`], [`sandbox`]: workspaces, their instruction files and skills,
//!   and their sandbox;
//! - [`shells`]: named shells and agent commands; [`tools`]: tool calls and the journal;
//! - [`fsops`], [`worker`]: the file ops and the fs worker that runs them;
//! - [`jobs`]: jobs and their event log; [`approvals`]: the webhook and approvals;
//! - [`fs_list`], [`pty`]: the user's own file listing, and terminal attach;
//! - [`paths`], [`store`]: files and the database.

pub mod approvals;
pub mod conn;
pub mod conns;
pub mod context;
pub mod dispatch;
pub mod fs_list;
pub mod fsops;
pub mod jobs;
pub mod paths;
pub mod pty;
pub mod sandbox;
pub mod shells;
pub mod store;
pub mod tools;
pub mod worker;
pub mod workspaces;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use controlroom_protocol::ids::{DaemonId, Epoch};
use controlroom_sandbox::AbsPath;
use tokio::signal::unix::{SignalKind, signal};

use self::approvals::Approvals;
use self::conns::Conns;
use self::jobs::Jobs;
use self::paths::HostPaths;
use self::sandbox::SandboxEnv;
use self::shells::Shells;
use self::store::{Store, meta_get, meta_set};
use self::tools::Tools;
use self::workspaces::Workspaces;
use crate::dirs::{Dirs, create_private_dir};
use crate::instance::{self, Claim, ReadyLine, Started};
use crate::shell::{login_shell, resolve_program};

/// How often the journal is pruned.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);
/// Leaked nono temp dirs older than this are removed at start (spec §11.5).
const NONO_TMP_MIN_AGE: Duration = Duration::from_secs(3600);

/// The host daemon's shared state.
pub struct Host {
    /// Its files.
    pub paths: HostPaths,
    /// `host.sqlite`.
    pub store: Store,
    /// This installation's stable id.
    pub daemon_id: DaemonId,
    /// This process's lifetime.
    pub epoch: Epoch,
    /// This `controlroomd` (runs supervisors and fs workers).
    pub exe: AbsPath,
    /// nono and `sandbox.toml`.
    pub sandbox: SandboxEnv,
    /// Open connections.
    pub conns: Arc<Conns>,
    /// Open workspaces.
    pub workspaces: Workspaces,
    /// Named shells.
    pub shells: Shells,
    /// Running tool calls.
    pub tools: Tools,
    /// Jobs and their event log.
    pub jobs: Jobs,
    /// Sandbox approvals.
    pub approvals: Approvals,
}

/// `controlroomd host …`.
#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Run the host daemon (or, with --detach, start it in the background).
    Serve(ServeArgs),
    /// Bridge stdin/stdout to the host daemon, starting it if needed (what the controller runs).
    Connect(StateArgs),
}

/// Arguments of `controlroomd host serve`.
#[derive(Debug, clap::Args)]
struct ServeArgs {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Start a detached daemon and exit once its socket is ready (exit 0 if one is running).
    #[arg(long, conflicts_with = "detached_child")]
    detach: bool,
    /// Internal: the detached daemon started by `--detach` or `connect`.
    #[arg(long = "detached-child", hide = true)]
    detached_child: bool,
}

/// A state directory override.
#[derive(Debug, clap::Args)]
pub struct StateArgs {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
}

/// Runs `controlroomd host …`.
pub fn run(args: Args) -> ExitCode {
    let result = match args.command {
        Command::Serve(args) => serve(&args),
        Command::Connect(args) => connect(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroomd host: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn connect(args: &StateArgs) -> anyhow::Result<()> {
    let paths = HostPaths::new(Dirs::from_env(args.state_dir.as_deref())?);
    let stream = crate::connect::connect_or_start_with(&paths.socket(), || {
        let exe = instance::serve_binary()?;
        instance::spawn_detached_with(&exe, paths.dirs(), &["host", "serve"], &paths.log())
            .map(drop)
    })?;
    crate::connect::bridge(stream)
}

fn serve(args: &ServeArgs) -> anyhow::Result<()> {
    let paths = HostPaths::new(Dirs::from_env(args.state_dir.as_deref())?);
    if args.detach {
        let exe = instance::serve_binary()?;
        match instance::spawn_detached_with(&exe, paths.dirs(), &["host", "serve"], &paths.log())? {
            Started::New => eprintln!("controlroomd host: started"),
            Started::AlreadyRunning => eprintln!("controlroomd host: already running"),
        }
        return Ok(());
    }
    if args.detached_child {
        instance::detach_from_launcher()?;
    }
    init_tracing();
    let detached = args.detached_child;
    let instance = match instance::claim_at(paths.dirs(), &paths.lock(), &paths.socket()) {
        Ok(Claim::Acquired(instance)) => instance,
        Ok(Claim::AlreadyRunning) => {
            if detached {
                ReadyLine::AlreadyRunning.report();
            }
            return Ok(());
        }
        Err(e) => {
            if detached {
                ReadyLine::Error(format!("{e:#}")).report();
            }
            return Err(e);
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let started = start(paths).await;
        let host = match started {
            Ok(host) => host,
            Err(e) => {
                if detached {
                    ReadyLine::Error(format!("{e:#}")).report();
                }
                return Err(e);
            }
        };
        if detached {
            ReadyLine::Ready.report();
        }
        run_server(host, instance).await
    })
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("CONTROLROOMD_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

/// Opens the state and recovers; see the module docs.
async fn start(paths: HostPaths) -> anyhow::Result<Arc<Host>> {
    create_private_dir(&paths.root()).with_context(|| format!("creating {}", paths.root()))?;
    let store = Store::open(paths.database().as_path())?;
    let daemon_id = store
        .run_blocking(|c| {
            if let Some(id) = meta_get(c, "daemon_id")?.and_then(|s| s.parse().ok()) {
                return Ok(id);
            }
            let id = DaemonId::generate();
            meta_set(c, "daemon_id", &id.to_string())?;
            Ok(id)
        })
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .context("resolving the controlroomd path")?;
    let exe = AbsPath::from_path(&exe)?;
    let sandbox = {
        let (paths, exe) = (paths.clone(), exe.clone());
        tokio::task::spawn_blocking(move || SandboxEnv::load(&paths, exe)).await?
    };
    let bash = tokio::task::spawn_blocking(find_bash).await?;
    let nono_tmp = sandbox.settings.nono_tmp.clone();
    tokio::task::spawn_blocking(move || {
        let removed = controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(
            nono_tmp.as_path(),
            NONO_TMP_MIN_AGE,
        );
        if !removed.is_empty() {
            tracing::info!("removed {} leaked nono temp dirs", removed.len());
        }
    });
    let conns = Arc::new(Conns::default());
    let host = Arc::new(Host {
        paths,
        store,
        daemon_id,
        epoch: Epoch::generate(),
        exe,
        sandbox,
        conns: conns.clone(),
        workspaces: Workspaces::default(),
        shells: Shells::new(bash),
        tools: Tools::new(conns),
        jobs: Jobs::default(),
        approvals: Approvals::default(),
    });
    match approvals::bind(&host).await {
        Ok(listener) => {
            tokio::spawn(approvals::serve(listener, host.clone()));
        }
        Err(e) => tracing::error!("the approval webhook can't bind ({e}); approvals deny"),
    }
    {
        let host = host.clone();
        tokio::task::spawn_blocking(move || workspaces::restore(&host))
            .await?
            .map_err(|e| anyhow::anyhow!(e.message))?;
    }
    tools::recover(&host)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    host.jobs
        .restore(&host)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    tracing::info!("host daemon {} up (epoch {})", host.daemon_id, host.epoch);
    Ok(host)
}

/// The bash agent shells run: the login shell if it is bash, else `bash` on its `PATH`.
fn find_bash() -> AbsPath {
    let shell = login_shell();
    if shell.file_name() == Some("bash") {
        return shell;
    }
    resolve_program(&shell, "bash")
        .ok()
        .flatten()
        .and_then(|p| AbsPath::new(&p).ok())
        .unwrap_or_else(|| AbsPath::new("/bin/bash").expect("static path"))
}

async fn run_server(host: Arc<Host>, instance: instance::Instance) -> anyhow::Result<()> {
    instance.listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(instance.listener)?;
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut prune = tokio::time::interval(PRUNE_EVERY);
    let mut next_id = 0u64;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    next_id += 1;
                    tokio::spawn(conn::serve(host.clone(), next_id, stream));
                }
                Err(e) => {
                    tracing::warn!("accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = prune.tick() => {
                let host = host.clone();
                tokio::spawn(async move { tools::prune(&host).await });
            }
            _ = hup.recv() => tracing::info!("ignoring SIGHUP"),
            _ = term.recv() => break,
            _ = int.recv() => break,
        }
    }
    tracing::info!("host daemon stopping; shells and jobs keep running");
    Ok(())
}
