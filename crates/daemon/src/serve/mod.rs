//! `controlroomd serve`: the per-user daemon (spec §3, §6).
//!
//! Startup order: claim the instance (lock, bind the socket; [`crate::instance`]), open the
//! runtime (`daemon.sqlite`), report ready (when detached), then accept connections while
//! [`Runtime::recover`] re-folds transcripts and reattaches supervisors concurrently;
//! commands wait for it. The socket is bound before recovery, so a slow recovery never makes
//! `connect` time out.
//!
//! Running: one task per connection ([`conn`]), `tick()` once a second, leaked `/tmp/nono-*`
//! directories cleaned once at start. SIGTERM or SIGINT stop accepting, drop every supervisor
//! connection and exit 0; agents keep running under their supervisors. SIGHUP is ignored.
//!
//! - [`host`], [`agents`]: the runtime's ports on this machine;
//! - [`conn`], [`controller`], [`dispatch`], [`feeds`], [`fs_list`]: the daemon API.

mod agents;
mod conn;
mod controller;
mod dispatch;
mod feeds;
mod fs_list;
mod host;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use controlroom_runtime::ports::SystemClock;
use controlroom_runtime::{Runtime, RuntimeConfig};
use controlroom_sandbox::AbsPath;
use tokio::signal::unix::{SignalKind, signal};

use crate::config::sandbox as sandbox_config;
use crate::dirs::Dirs;
use crate::instance::{self, Claim, ReadyLine, Started};

/// Leaked nono temp dirs older than this are removed at start.
const NONO_TMP_MIN_AGE: Duration = Duration::from_secs(3600);

/// Arguments of `controlroomd serve`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Start a detached daemon and exit once its socket is ready (exit 0 if one is running).
    #[arg(long, conflicts_with = "detached_child")]
    detach: bool,
    /// Internal: the detached daemon started by `--detach` or `connect`.
    /// (Its flag is [`instance::DETACHED_CHILD_FLAG`].)
    #[arg(long = "detached-child", hide = true)]
    detached_child: bool,
}

/// Daemon-wide state shared by connections.
pub struct Shared {
    /// The session runtime.
    pub runtime: Runtime,
    /// The daemon's directories.
    pub dirs: Dirs,
    /// The connected controller.
    pub controllers: controller::Controllers,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let result = Dirs::from_env(args.state_dir.as_deref())
        .map_err(anyhow::Error::from)
        .and_then(|dirs| {
            if args.detach {
                start_detached(&dirs)
            } else {
                serve(dirs, args.detached_child)
            }
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if args.detached_child {
                ReadyLine::Error(format!("{e:#}")).report();
            }
            eprintln!("controlroomd serve: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn start_detached(dirs: &Dirs) -> anyhow::Result<()> {
    let exe = instance::serve_binary()?;
    match instance::spawn_detached(&exe, dirs)? {
        Started::New => eprintln!("controlroomd serve: started"),
        Started::AlreadyRunning => eprintln!("controlroomd serve: already running"),
    }
    Ok(())
}

fn serve(dirs: Dirs, detached: bool) -> anyhow::Result<()> {
    if detached {
        instance::detach_from_launcher()?;
    }
    init_logging();
    let instance = match instance::claim(&dirs)? {
        Claim::Acquired(instance) => instance,
        Claim::AlreadyRunning => {
            if detached {
                ReadyLine::AlreadyRunning.report();
                return Ok(());
            }
            anyhow::bail!("another daemon is running ({})", dirs.daemon_lock());
        }
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(serve_async(dirs, instance, detached))
}

async fn serve_async(
    dirs: Dirs,
    instance: instance::Instance,
    detached: bool,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("resolving the controlroomd path")?;
    let controlroomd = AbsPath::from_path(&exe).context("the controlroomd path")?;
    let runtime = Runtime::open(RuntimeConfig {
        db_path: dirs.state().join("daemon.sqlite").as_path().to_owned(),
        host: Arc::new(host::DaemonHost::new(dirs.clone(), exe)),
        agents: Arc::new(agents::DaemonAgents::new(dirs.clone(), controlroomd)),
        clock: Arc::new(SystemClock),
    })?;
    instance.listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(instance.listener)?;
    if detached {
        ReadyLine::Ready.report();
    }
    tracing::info!("controlroomd {} serving", crate::version::VERSION);

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
        dirs,
        controllers: controller::Controllers::default(),
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
    let filter = tracing_subscriber::EnvFilter::try_from_env("CONTROLROOMD_LOG")
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
    let Ok(tmp) = crate::dirs::expand_home(&config.nono_tmp, dirs.home()) else {
        return;
    };
    let removed =
        controlroom_sandbox::tempdirs::clean_leaked_nono_dirs(tmp.as_path(), NONO_TMP_MIN_AGE);
    if !removed.is_empty() {
        tracing::info!("removed {} leaked nono temp dirs", removed.len());
    }
}
