//! `controlroomd doctor`: environment checks (prototype plan §6 step 4, spec §6.6, §11.5).
//!
//! - persistence after logout: `KillUserProcesses`, linger, the systemd user manager, and the
//!   cgroup this process runs in;
//! - start at boot: linger plus a systemd user unit, else `crontab @reboot`, else lazily on the
//!   next connection;
//! - the sandbox: nono available and pinned to 0.80.0, the kernel's Landlock ABI;
//! - agent dependencies, resolved through the login shell, each in a `PATH` directory (nono);
//! - sftp availability (a placeholder for file browsing).
//!
//! The default output is the protocol's [`DoctorReport`] as JSON (what `doctor.report`
//! returns); `--human` prints a table with every underlying check. Checks never fail the
//! command: anything undeterminable is `unknown`, with the reason.

mod agents;
mod parse;
mod persistence;
mod render;
mod sandbox;

use std::path::PathBuf;
use std::process::ExitCode;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::daemon::{CheckStatus, DoctorCheck, DoctorReport};

pub use agents::check_agents;
pub use render::render_human;

use crate::config::{agents as agents_config, sandbox as sandbox_config};
use crate::dirs::Dirs;
use crate::shell::login_shell;
use crate::version::VERSION;

/// Arguments of `controlroomd doctor`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// State directory [default: ~/.local/state/controlroom].
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Print a human-readable table instead of JSON.
    #[arg(long)]
    human: bool,
}

/// The report plus the details behind its summary checks, for the human table.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnosis {
    /// What `doctor.report` returns.
    pub report: DoctorReport,
    /// The checks behind `report.persistence`.
    pub persistence: Vec<(&'static str, DoctorCheck)>,
    /// The checks behind `report.boot_start_check`.
    pub boot_start: Vec<(&'static str, DoctorCheck)>,
    /// The Landlock check behind `report.landlock_abi`.
    pub landlock: DoctorCheck,
    /// Whether the SSH server can serve sftp (placeholder).
    pub sftp: DoctorCheck,
    /// Why `sandbox.toml` could not be used, if it couldn't.
    pub sandbox_config_error: Option<String>,
}

/// Runs every check.
pub fn diagnose(dirs: &Dirs) -> Diagnosis {
    let shell = login_shell();
    // Sandboxed launches run nono with controlroomd's own PATH.
    let path_var = std::env::var("PATH").unwrap_or_default();
    let id = persistence::identity();
    let persistence = persistence::persistence(&id);
    let boot = persistence::boot_start(&persistence);
    let agents = agents_config::load(dirs);
    let sandbox = sandbox_config::load(dirs);
    let (landlock_abi, landlock) = sandbox::landlock();
    Diagnosis {
        report: DoctorReport {
            daemon_version: VERSION.to_owned(),
            user: id.user,
            persistence: persistence.summary(),
            boot_start: boot.method,
            boot_start_check: boot.summary,
            nono: sandbox::nono(dirs, &shell, &sandbox.value.nono),
            landlock_abi,
            agents_config: agents.path.as_ref().map(|p| p.as_str().into()),
            agents_config_error: agents.error,
            agents: check_agents(&shell, &path_var, &agents.value),
            checked_at: UnixMillis::now(),
        },
        persistence: persistence.details,
        boot_start: boot.details,
        landlock,
        sftp: sandbox::sftp(),
        sandbox_config_error: sandbox.error,
    }
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let dirs = match Dirs::from_env(args.state_dir.as_deref()) {
        Ok(dirs) => dirs,
        Err(e) => {
            eprintln!("controlroomd doctor: {e}");
            return ExitCode::FAILURE;
        }
    };
    let diagnosis = diagnose(&dirs);
    if args.human {
        print!("{}", render_human(&diagnosis));
    } else {
        match serde_json::to_string_pretty(&diagnosis.report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("controlroomd doctor: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// A check with an observed value.
fn check(status: CheckStatus, value: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        status,
        value: Some(value.to_owned()),
        detail: detail.into(),
    }
}

/// A check whose outcome couldn't be determined.
fn unknown(detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        status: CheckStatus::Unknown,
        value: None,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real checks; whatever the environment (even the Nix build sandbox, without
    /// systemd), it must produce a report that the protocol type reads back.
    #[test]
    fn diagnose_never_fails() {
        let t = tempfile::tempdir().unwrap();
        let dirs = Dirs::under_home(controlroom_sandbox::AbsPath::from_path(t.path()).unwrap());
        let diagnosis = diagnose(&dirs);
        let json = serde_json::to_value(&diagnosis.report).unwrap();
        let back: DoctorReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, diagnosis.report);
        assert_eq!(diagnosis.report.agents[0].agent, "pi");
        assert!(diagnosis.report.agents_config.is_none());
        assert!(render_human(&diagnosis).contains("Agents (defaults)"));
    }
}
