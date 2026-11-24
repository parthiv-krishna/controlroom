//! controlroom-agentd: serve, login, ctl, doctor, credential, supervise.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use controlroom_agentd::{credential, ctl, doctor, environment, login, serve, supervise};

/// The controlroom agent daemon: runs every pi session on the server.
#[derive(Parser)]
#[command(name = "controlroom-agentd", version = controlroom_agentd::version::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the agent daemon.
    Serve(serve::Args),
    /// Sign in to an OAuth subscription for the sessions (one time).
    Login(login::Args),
    /// Debug client: talk to the agent daemon, render a session's timeline.
    Ctl(ctl::Args),
    /// Check the server: sandbox, agents, configuration.
    Doctor(doctor::Args),
    /// Print a provider's credential for nono's credential capture.
    Credential(credential::Args),
    /// Run one detached supervisor (an agent and its transcript).
    Supervise(supervise::Args),
}

fn main() -> ExitCode {
    // `NONO_*` flags and provider keys never reach what the daemon starts.
    environment::strip_inherited();
    match Cli::parse().command {
        Command::Serve(args) => serve::run(args),
        Command::Login(args) => login::run(args),
        Command::Ctl(args) => ctl::run(args),
        Command::Doctor(args) => doctor::run(args),
        Command::Credential(args) => credential::run(args),
        Command::Supervise(args) => supervise::run(args),
    }
}
