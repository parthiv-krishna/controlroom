//! `controlroomd supervise`, implemented by `controlroom-supervisor`.

use std::process::ExitCode;

pub use controlroom_supervisor::cli::Args;

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    controlroom_supervisor::cli::run(args)
}
