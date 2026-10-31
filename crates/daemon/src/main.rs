//! controlroomd: serve, supervise, connect, ctl, doctor, credential.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod credential;

#[derive(Debug, Parser)]
#[command(name = "controlroomd", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a provider's API key for nono's credential capture.
    Credential(credential::Args),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Credential(args) => credential::run(args),
    }
}
