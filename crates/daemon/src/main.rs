//! controlroomd: serve, connect, supervise, doctor, and the host role (host, workspace-exec, pty).

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use controlroomd::{connect, doctor, host, serve, supervise};

/// The controlroom daemon.
#[derive(Parser)]
#[command(name = "controlroomd", version = controlroomd::version::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (or, with --detach, start it in the background).
    Serve(serve::Args),
    /// Bridge stdin/stdout to the daemon socket, starting the daemon if needed.
    Connect(connect::Args),
    /// Check this machine: persistence, boot start, sandbox, agents.
    Doctor(doctor::Args),
    /// Run one detached supervisor (a process and its transcript).
    Supervise(supervise::Args),
    /// The host role: serve the host API, or bridge to it.
    Host(host::Args),
    /// The per-workspace fs worker (started by the host daemon under the workspace sandbox).
    WorkspaceExec(host::worker::Args),
    /// Attach to or resize a shell's terminal.
    Pty(host::pty::Args),
}

fn main() -> ExitCode {
    strip_nono_environment();
    match Cli::parse().command {
        Command::Serve(args) => serve::run(args),
        Command::Connect(args) => connect::run(args),
        Command::Doctor(args) => doctor::run(args),
        Command::Supervise(args) => supervise::run(args),
        Command::Host(args) => host::run(args),
        Command::WorkspaceExec(args) => host::worker::run(args),
        Command::Pty(args) => host::pty::run(args),
    }
}

/// Removes inherited `NONO_*` variables before anything else runs: nono reads flags such as
/// `NONO_ALLOW` from its environment, and every process the daemon starts (supervisors, then
/// nono) inherits this one's.
fn strip_nono_environment() {
    let prefix = controlroom_sandbox::launch::NONO_ENV_PREFIX;
    let keys: Vec<_> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| k.to_string_lossy().starts_with(prefix))
        .collect();
    for key in keys {
        // SAFETY: this runs first thing in `main`, before any other thread exists, so nothing
        // can read the environment concurrently.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(key);
        }
    }
}
