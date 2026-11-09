//! `controlroom-agentd login <provider>`: a one-time sign-in to an OAuth subscription on the
//! server from a terminal (design §9); Settings › Models does the same from the UI. It runs the
//! session agent's pi interactively on the agent daemon's own agent dir (`sandbox.toml`'s
//! `pi_agent_dir`, by default `<state>/agent`; `PI_CODING_AGENT_DIR`), where the user signs in with pi's
//! `/login <provider>`; pi stores the credential in that dir's `auth.json`. Afterwards it
//! checks that an OAuth credential for the provider is there. From then on the agent daemon is
//! the only refresher (sessions get synthetic tokens, `credential` serves real ones).
//!
//! Run it as the agent daemon's user, e.g. `sudo -u controlroom-agent -i controlroom-agentd login
//! <provider>`: it finds the daemon's directories like every subcommand (see [`crate::dirs`]).
//! No credential is ever printed.

use std::process::{Command, ExitCode};

use anyhow::{Context, bail};
use serde_json::Value;

use crate::config::agents::{self as agents_config, SESSION_AGENT};
use crate::config::sandbox as sandbox_config;
use crate::dirs::{Dirs, create_private_dir, home};
use crate::serve::Location;
use crate::shell::login_shell;

/// Arguments of `controlroom-agentd login`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Where the agent daemon keeps its files (as for `serve`).
    #[command(flatten)]
    location: Location,
    /// The pi provider id, e.g. the subscription's provider.
    provider: String,
}

/// Runs the subcommand.
pub fn run(args: Args) -> ExitCode {
    let result = args
        .location
        .dirs()
        .map_err(anyhow::Error::from)
        .and_then(|dirs| login(&dirs, &args.provider));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("controlroom-agentd login: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn login(dirs: &Dirs, provider: &str) -> anyhow::Result<()> {
    let sandbox = sandbox_config::load(dirs);
    if let Some(e) = &sandbox.error {
        bail!("{e}");
    }
    let agent_dir = sandbox
        .value
        .resolve(home().as_ref(), &dirs.pi_agent_dir())?
        .agent_dir
        .user_agent_dir
        .unwrap_or_else(|| dirs.pi_agent_dir());
    create_private_dir(&agent_dir).with_context(|| format!("creating {agent_dir}"))?;
    let scratch = dirs.state().join("login");
    create_private_dir(&scratch).with_context(|| format!("creating {scratch}"))?;
    let agents = agents_config::load(dirs);
    let Some(agent) = agents.value.0.get(SESSION_AGENT) else {
        bail!("no pi agent in agents.toml");
    };
    eprintln!(
        "Starting pi on {agent_dir}. Sign in with `/login {provider}`, then quit pi (/quit)."
    );
    let shell = login_shell();
    let status = Command::new(shell.as_path())
        .arg("-lc")
        .arg(format!("exec {}", agent.command.trim()))
        .current_dir(scratch.as_path())
        .env("PI_CODING_AGENT_DIR", agent_dir.as_path())
        .status()
        .with_context(|| format!("starting {}", agent.command))?;
    if !status.success() {
        eprintln!("pi exited with {status}");
    }
    let auth = std::fs::read_to_string(agent_dir.join("auth.json").as_path()).unwrap_or_default();
    if has_oauth(&auth, provider) {
        eprintln!("{provider}: signed in; sessions started from now on can use it");
        Ok(())
    } else {
        bail!("no OAuth credential for {provider} in {agent_dir}/auth.json")
    }
}

/// Whether `auth.json` holds an OAuth credential for `provider`.
fn has_oauth(auth: &str, provider: &str) -> bool {
    serde_json::from_str::<Value>(auth)
        .ok()
        .and_then(|auth| {
            auth.get(provider)?
                .get("type")?
                .as_str()
                .map(|t| t == "oauth")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_oauth_credentials_count() {
        let auth = r#"{"sub": {"type": "oauth", "access": "a"}, "key": {"type": "api_key"}}"#;
        assert!(has_oauth(auth, "sub"));
        assert!(!has_oauth(auth, "key"));
        assert!(!has_oauth(auth, "other"));
        assert!(!has_oauth("not json", "sub"));
    }
}
