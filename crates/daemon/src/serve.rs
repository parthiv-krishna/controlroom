//! `controlroomd serve`: the per-user host daemon (spec §3, §5.3b, §6).
//!
//! Startup order: claim the instance (lock, bind the socket; [`crate::instance`]), then
//! [`host::start`] (open the state, recover), report ready (when detached), and serve
//! ([`host::run_server`]). Connections queue on the bound socket while recovery runs. Logs go
//! to stderr (`daemon.log` when detached), filtered by `$CONTROLROOMD_LOG`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;

use crate::dirs::Dirs;
use crate::host;
use crate::instance::{self, Claim, ReadyLine, Started};

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
        .block_on(async move {
            let host = host::start(dirs).await?;
            if detached {
                ReadyLine::Ready.report();
            }
            tracing::info!("controlroomd {} serving", crate::version::VERSION);
            host::run_server(host, instance).await
        })
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
