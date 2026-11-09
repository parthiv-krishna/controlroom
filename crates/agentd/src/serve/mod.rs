//! `controlroom-agentd serve`: the agent daemon (design §1, spec §3, §6).
//!
//! Startup order: claim the instance (lock, bind the socket; [`crate::instance`]), open the
//! runtime (`agent.sqlite`), bind the agent sockets of open sessions, then accept connections
//! while [`Runtime::recover`] re-folds transcripts and reattaches supervisors concurrently;
//! commands wait for it.
//!
//! Running: one task per connection ([`conn`]), `tick()` once a second, leaked `/tmp/nono-*`
//! directories cleaned once at start. SIGTERM or SIGINT stop accepting, drop every supervisor
//! connection and exit 0; agents keep running under their supervisors (systemd's
//! `KillMode=process`). SIGHUP is ignored.
//!
//! - [`host`], [`agents`]: the runtime's ports on the server;
//! - [`agent`]: the per-session agent sockets @controlroom/pi talks to;
//! - [`conn`], [`controller`], [`dispatch`], [`feeds`]: the agent daemon API, including the
//!   host bridge the controller carries.

mod agent;
mod agents;
mod conn;
mod controller;
mod dispatch;
mod feeds;
mod host;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use controlroom_protocol::Capability;
use controlroom_runtime::ports::SystemClock;
use controlroom_runtime::{Runtime, RuntimeConfig};
use controlroom_sandbox::AbsPath;
use tokio::signal::unix::{SignalKind, signal};

use crate::config::sandbox as sandbox_config;
use crate::dirs::{Dirs, Overrides};
use crate::instance;

/// Leaked nono temp dirs older than this are removed at start.
const NONO_TMP_MIN_AGE: Duration = Duration::from_secs(3600);

/// Where the agent daemon keeps its files: the options of `serve` and of every subcommand
/// that finds a running daemon's files.
#[derive(Debug, clap::Args)]
pub struct Location {
    /// State directory [default: $STATE_DIRECTORY, else ~/.local/state/controlroom-agent].
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Configuration directory [default: $CONFIGURATION_DIRECTORY, else <state>/config with
    /// --state-dir, else ~/.config/controlroom-agent].
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// The agent daemon API socket [default: $RUNTIME_DIRECTORY/agentd.sock, else
    /// <state>/run/agentd.sock].
    #[arg(long)]
    socket: Option<PathBuf>,
}

/// Arguments of `controlroom-agentd serve`.
#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(flatten)]
    location: Location,
}

impl Location {
    /// The directories these arguments name.
    pub fn dirs(&self) -> Result<Dirs, crate::dirs::DirsError> {
        Dirs::from_env(Overrides {
            state: self.state_dir.as_deref(),
            config: self.config_dir.as_deref(),
            socket: self.socket.as_deref(),
        })
    }
}

/// Daemon-wide state shared by connections.
pub struct Shared {
    /// The session runtime.
    pub runtime: Runtime,
    /// The daemon's directories.
    pub dirs: Dirs,
    /// The connected controller.
    pub controllers: controller::Controllers,
    /// Settings › Models.
    pub models: crate::models::Models,
    /// Optional features this daemon serves (`hello.capabilities`).
    pub capabilities: Vec<Capability>,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let result = args
        .location
        .dirs()
        .map_err(anyhow::Error::from)
        .and_then(serve);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroom-agentd serve: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn serve(dirs: Dirs) -> anyhow::Result<()> {
    init_logging();
    let instance = instance::claim(&dirs)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(serve_async(dirs, instance))
}

async fn serve_async(dirs: Dirs, instance: instance::Instance) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("resolving the controlroom-agentd path")?;
    let agentd = AbsPath::from_path(&exe).context("the controlroom-agentd path")?;
    let sockets = agent::AgentSockets::new(dirs.clone(), tokio::runtime::Handle::current());
    let runtime = Runtime::open(RuntimeConfig {
        db_path: dirs.database().as_path().to_owned(),
        host: Arc::new(host::ServerHost::new(dirs.clone(), exe, sockets.clone())),
        agents: Arc::new(agents::ServerAgents::new(
            dirs.clone(),
            agentd,
            sockets.clone(),
        )),
        clock: Arc::new(SystemClock),
    })?;
    // Agents that outlived the previous daemon reconnect to their sockets.
    let open = runtime
        .subscribe_sessions(None)
        .map_err(|e| anyhow::anyhow!("listing sessions: {e}"))?
        .snapshot
        .map(|s| s.sessions)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.closed_at.is_none())
        .map(|s| s.id);
    sockets.serve(runtime.clone(), open);
    instance.listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(instance.listener)?;
    tracing::info!(
        "controlroom-agentd {} serving on {}",
        crate::version::VERSION,
        dirs.socket()
    );

    tokio::task::spawn_blocking({
        let dirs = dirs.clone();
        move || clean_nono_tmp(&dirs)
    });
    tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime.recover().await;
            tracing::info!("recovery finished");
        }
    });
    let ticker = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            let mut every = tokio::time::interval(Duration::from_secs(1));
            loop {
                every.tick().await;
                runtime.tick();
            }
        }
    });

    let shared = Arc::new(Shared {
        runtime: runtime.clone(),
        dirs: dirs.clone(),
        controllers: controller::Controllers::default(),
        models: crate::models::Models::new(dirs.clone()),
        capabilities: vec![
            Capability::TODOS,
            Capability::JOBS,
            Capability::SANDBOX_APPROVALS,
        ],
    });
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut next_connection = 0u64;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    next_connection += 1;
                    tokio::spawn(conn::serve(shared.clone(), next_connection, stream));
                }
                Err(e) => tracing::warn!("accept: {e}"),
            },
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = hangup.recv() => tracing::info!("ignoring SIGHUP"),
        }
    }
    tracing::info!("shutting down; agents keep running");
    ticker.abort();
    runtime.shutdown().await;
    Ok(())
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("CONTROLROOM_AGENTD_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

/// Removes `nono-*` dirs a crashed nono left in its TMPDIR (spec §11.5).
fn clean_nono_tmp(dirs: &Dirs) {
    let config = sandbox_config::load(dirs).value;
    let Ok(tmp) = crate::dirs::expand_home(&config.nono_tmp, crate::dirs::home().as_ref()) else {
        return;
    };
    let removed =
        controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(tmp.as_path(), NONO_TMP_MIN_AGE);
    if !removed.is_empty() {
        tracing::info!("removed {} leaked nono temp dirs", removed.len());
    }
}
