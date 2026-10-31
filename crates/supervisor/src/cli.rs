//! `controlroomd supervise`: the command-line entry point.
//!
//! ```text
//! controlroomd supervise <ID> --dir <DIR> --socket <PATH> --spec <JSON> [--foreground]
//! ```
//!
//! Without `--foreground` the command is a launcher: it re-executes the same binary
//! (`/proc/self/exe`, i.e. the immutable versioned path) as a detached child, waits
//! for its ready line, prints the [`SupervisorInfo`](crate::SupervisorInfo) JSON and
//! exits 0, or prints the error to stderr and exits 1. The child calls `setsid()`
//! first, so it has no controlling terminal and is in neither the caller's session
//! nor its process group; once the launcher exits it is reparented to init (or the
//! nearest subreaper). Its cwd is `/`, stdin is `/dev/null` and stderr is appended
//! to `<dir>/supervisor.log`.
//!
//! With `--foreground` the supervisor runs in the calling process (tests,
//! debugging) and prints `{"ready":{..}}` or `{"error":".."}` as its only stdout line.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, bail};

use crate::launch::LaunchRequest;
use crate::paths::{Paths, validate_id};
use crate::proto::ReadyLine;
use crate::server;
use crate::spec::SpawnSpec;

/// Arguments of `controlroomd supervise`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Supervised process id ([A-Za-z0-9_-], at most 64 characters).
    id: String,
    /// Process directory (transcript, supervisor.json, supervisor.log).
    #[arg(long)]
    dir: PathBuf,
    /// Socket path (at most 107 bytes).
    #[arg(long)]
    socket: PathBuf,
    /// The spawn spec as JSON.
    #[arg(long)]
    spec: String,
    /// Run in this process instead of detaching (tests, debugging).
    #[arg(long)]
    foreground: bool,
    /// Internal: set by the launcher on the detached child, which calls setsid().
    #[arg(long, hide = true)]
    detached_child: bool,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: Args) -> anyhow::Result<()> {
    let request = parse(&args);
    if args.detached_child {
        rustix::process::setsid().context("setsid")?;
    }
    if args.foreground || args.detached_child {
        let request = match request {
            Ok(request) => request,
            Err(e) => {
                let line = ReadyLine::Error(format!("{e:#}"));
                println!("{}", serde_json::to_string(&line)?);
                return Err(e);
            }
        };
        server::run(server::Config {
            id: request.id,
            spec: request.spec,
            paths: request.paths,
        })
    } else {
        detach(&request?)
    }
}

fn parse(args: &Args) -> anyhow::Result<LaunchRequest> {
    validate_id(&args.id)?;
    let paths = Paths {
        dir: std::path::absolute(&args.dir)?,
        socket: std::path::absolute(&args.socket)?,
    };
    paths.validate()?;
    let spec: SpawnSpec = serde_json::from_str(&args.spec).context("parsing --spec")?;
    spec.validate()?;
    Ok(LaunchRequest {
        id: args.id.clone(),
        paths,
        spec,
    })
}

/// Starts the detached supervisor and relays its ready line.
fn detach(request: &LaunchRequest) -> anyhow::Result<()> {
    request
        .paths
        .create_dir()
        .with_context(|| format!("creating {}", request.paths.dir.display()))?;
    let log_path = request.paths.log_file();
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let exe = std::env::current_exe().context("resolving the controlroomd path")?;
    let mut child = Command::new(exe)
        .args(request.args())
        .arg("--detached-child")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .context("spawning the detached supervisor")?;

    let mut line = String::new();
    if let Some(stdout) = child.stdout.take() {
        BufReader::new(stdout).read_line(&mut line)?;
    }
    match serde_json::from_str::<ReadyLine>(&line) {
        Ok(ReadyLine::Ready(info)) => {
            println!("{}", serde_json::to_string(&info)?);
            Ok(())
        }
        Ok(ReadyLine::Error(message)) => {
            let _ = child.wait();
            bail!("{message}")
        }
        Err(_) => {
            let status = child.wait()?;
            bail!(
                "supervisor exited ({status}) before becoming ready; see {}",
                log_path.display()
            )
        }
    }
}
