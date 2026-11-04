//! `controlroom-agentd doctor`: the server's checks (spec §11.5, §14): the sandbox (nono pinned
//! to 0.80.0, the kernel's Landlock ABI), the agents' dependencies (resolved through the login
//! shell, each in a `PATH` directory), and the state and configuration directories. The agent
//! daemon runs as a system service, so the hosts' persistence and boot-start checks don't
//! apply (they report `unknown` with that reason).
//!
//! The default output is the protocol's [`DoctorReport`] as JSON (what `doctor.report`
//! returns); `--human` prints a short table. Checks never fail the command.

mod agents;
mod sandbox;

use std::process::ExitCode;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::host::{BootStartMethod, CheckStatus, DoctorCheck, DoctorReport};

pub use agents::check_agents;

use crate::config::{agents as agents_config, sandbox as sandbox_config};
use crate::dirs::Dirs;
use crate::serve::Location;
use crate::shell::login_shell;
use crate::version::VERSION;

/// Arguments of `controlroom-agentd doctor`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Where the agent daemon keeps its files (as for `serve`).
    #[command(flatten)]
    location: Location,
    /// Print a human-readable table instead of JSON.
    #[arg(long)]
    human: bool,
}

/// Runs every check.
pub fn diagnose(dirs: &Dirs) -> DoctorReport {
    let shell = login_shell();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let agents = agents_config::load(dirs);
    let sandbox = sandbox_config::load(dirs);
    let (landlock_abi, _) = sandbox::landlock();
    let service = || unknown("not applicable: the agent daemon runs as a system service");
    DoctorReport {
        daemon_version: VERSION.to_owned(),
        user: std::env::var("USER").ok(),
        persistence: service(),
        boot_start: BootStartMethod::Unknown,
        boot_start_check: service(),
        nono: sandbox::nono(dirs, &shell, &sandbox.value.nono),
        landlock_abi,
        agents_config: agents.path.as_ref().map(|p| p.as_str().into()),
        agents_config_error: agents.error.or(sandbox.error),
        agents: check_agents(&shell, &path_var, &agents.value),
        checked_at: UnixMillis::now(),
    }
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let dirs = match args.location.dirs() {
        Ok(dirs) => dirs,
        Err(e) => {
            eprintln!("controlroom-agentd doctor: {e}");
            return ExitCode::FAILURE;
        }
    };
    let report = diagnose(&dirs);
    if args.human {
        print!("{}", render_human(&report));
        return ExitCode::SUCCESS;
    }
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("controlroom-agentd doctor: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// A short table of the report.
pub fn render_human(report: &DoctorReport) -> String {
    let mark = |status: CheckStatus| match status {
        CheckStatus::Ok => "ok",
        CheckStatus::Warn => "warn",
        CheckStatus::Fail => "FAIL",
        CheckStatus::Unknown => "?",
    };
    let mut out = format!("controlroom-agentd {} doctor\n", report.daemon_version);
    let nono = &report.nono;
    out.push_str(&format!(
        "  {:5} nono: {}\n",
        mark(nono.status),
        nono.detail
    ));
    let landlock = report
        .landlock_abi
        .map_or_else(|| "unavailable".to_owned(), |abi| format!("ABI {abi}"));
    out.push_str(&format!("        landlock: {landlock}\n"));
    for agent in &report.agents {
        let ok = matches!(agent.status, controlroom_protocol::agentd::AgentStatus::Ok);
        let status = if ok { "ok" } else { "FAIL" };
        out.push_str(&format!(
            "  {status:5} agent {}: {}\n",
            agent.agent, agent.detail
        ));
    }
    if let Some(error) = &report.agents_config_error {
        out.push_str(&format!("  FAIL  config: {error}\n"));
    }
    out
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

    /// Runs the real checks; whatever the environment (even the Nix build sandbox), it must
    /// produce a report that the protocol type reads back.
    #[test]
    fn diagnose_never_fails() {
        let t = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(controlroom_sandbox::AbsPath::from_path(t.path()).unwrap());
        let report = diagnose(&dirs);
        let json = serde_json::to_value(&report).unwrap();
        let back: DoctorReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
        assert_eq!(report.agents[0].agent, "pi");
        assert!(report.agents_config.is_none());
        assert!(render_human(&report).contains("agent pi"));
    }
}
