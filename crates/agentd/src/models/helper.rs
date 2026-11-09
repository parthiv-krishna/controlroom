//! The controlroom-pi bundle's models helper (`controlroom-pi-models`, `nix/pi/models.mjs`):
//! asks pi itself which providers and models a config offers, signs in and out with pi's own
//! login flows, so the agent daemon never reimplements pi's provider logic.
//!
//! ```text
//! controlroom-pi-models list   --agent-dir DIR               JSON: providers and models
//! controlroom-pi-models logout --agent-dir DIR --provider ID
//! controlroom-pi-models login  --agent-dir DIR --provider ID  JSON lines (see `super::login`)
//! ```

use std::process::Stdio;
use std::time::Duration;

use controlroom_protocol::agentd::ModelChoice;
use controlroom_sandbox::AbsPath;
use serde::Deserialize;
use tokio::process::Command;

/// How long `list` and `logout` may take (pi loads its catalog; no network).
const TIMEOUT: Duration = Duration::from_secs(60);

/// Longest helper error message passed on.
const MAX_ERROR_LEN: usize = 500;

/// A provider pi knows.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogProvider {
    /// pi's provider id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// pi can sign in to it.
    pub oauth: bool,
}

/// What pi offers with a config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Catalog {
    /// Every provider pi knows.
    pub providers: Vec<CatalogProvider>,
    /// The models of providers that have credentials.
    pub models: Vec<ModelChoice>,
}

/// The models helper.
#[derive(Debug, Clone)]
pub struct Helper {
    program: AbsPath,
}

impl Helper {
    /// The helper at `program`.
    pub fn new(program: AbsPath) -> Self {
        Self { program }
    }

    /// A helper command on `agent_dir`.
    pub fn command(&self, mode: &str, agent_dir: &AbsPath) -> Command {
        let mut command = Command::new(self.program.as_path());
        command
            .arg(mode)
            .arg("--agent-dir")
            .arg(agent_dir.as_str())
            .env("PI_CODING_AGENT_DIR", agent_dir.as_str())
            .env("PI_OFFLINE", "1")
            .env("PI_TELEMETRY", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// The providers and models of the config in `agent_dir`.
    pub async fn list(&self, agent_dir: &AbsPath) -> Result<Catalog, String> {
        let out = run(self.command("list", agent_dir)).await?;
        serde_json::from_str(&out).map_err(|e| format!("the models helper's list: {e}"))
    }

    /// Signs out of `provider` in `agent_dir`'s `auth.json` (under pi's lock).
    pub async fn logout(&self, agent_dir: &AbsPath, provider: &str) -> Result<(), String> {
        let mut command = self.command("logout", agent_dir);
        command.arg("--provider").arg(provider);
        run(command).await.map(drop)
    }
}

/// Runs `command`, returning stdout, or its stderr as the error.
async fn run(mut command: Command) -> Result<String, String> {
    let out = tokio::time::timeout(TIMEOUT, command.output())
        .await
        .map_err(|_| "the models helper timed out".to_owned())?
        .map_err(|e| format!("the models helper: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let message: String = stderr.trim().chars().take(MAX_ERROR_LEN).collect();
    Err(if message.is_empty() {
        format!("the models helper exited with {}", out.status)
    } else {
        message
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper script.
    fn helper(dir: &std::path::Path, body: &str) -> Helper {
        let path = dir.join("helper");
        crate::models::test_support::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        Helper::new(AbsPath::from_path(&path).unwrap())
    }

    #[tokio::test]
    async fn lists_and_reports_errors() {
        let t = tempfile::tempdir().unwrap();
        let dir = AbsPath::from_path(t.path()).unwrap();
        let ok = helper(
            t.path(),
            r#"[ "$1 $2 $3" = "list --agent-dir $PI_CODING_AGENT_DIR" ] || exit 9
echo '{"providers": [{"id": "anthropic", "name": "Anthropic", "oauth": true}],
       "models": [{"provider": "anthropic", "id": "m1", "name": "M1", "reasoning": true}]}'"#,
        );
        let catalog = ok.list(&dir).await.unwrap();
        assert!(catalog.providers[0].oauth);
        assert_eq!(catalog.models[0].id, "m1");
        let failing = helper(t.path(), "echo 'no pi here' >&2; exit 1");
        assert_eq!(failing.list(&dir).await.unwrap_err(), "no pi here");
        assert_eq!(failing.logout(&dir, "x").await.unwrap_err(), "no pi here");
    }
}
