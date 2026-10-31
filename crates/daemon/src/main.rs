//! controlroomd: serve, supervise, connect, ctl, doctor, credential.

mod credential;
mod supervise;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The controlroom daemon.
#[derive(Parser)]
#[command(name = "controlroomd", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print a provider's API key for nono's credential capture.
    Credential(credential::Args),
    /// Run one detached supervisor (an agent or job and its transcript).
    Supervise(supervise::Args),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Credential(args) => credential::run(args),
        Command::Supervise(args) => supervise::run(args),
    }
}
