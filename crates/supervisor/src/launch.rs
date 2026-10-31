//! Starting a detached supervisor from the daemon.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::paths::Paths;
use crate::proto::SupervisorInfo;
use crate::spec::SpawnSpec;

/// The `controlroomd` subcommand that runs a supervisor.
pub const SUBCOMMAND: &str = "supervise";

/// How long [`launch`] waits for the supervisor to become ready.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Everything needed to start a supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    /// The supervised process id (`[A-Za-z0-9_-]{1,64}`, typically a ULID).
    pub id: String,
    /// Where the supervisor keeps its files and its socket.
    pub paths: Paths,
    /// What to run.
    pub spec: SpawnSpec,
}

impl LaunchRequest {
    /// The arguments after the program name: `supervise <ID> --dir .. --socket ..
    /// --spec <JSON>`.
    pub fn args(&self) -> Vec<OsString> {
        let spec = serde_json::to_string(&self.spec).unwrap_or_default();
        vec![
            SUBCOMMAND.into(),
            self.id.clone().into(),
            "--dir".into(),
            self.paths.dir.clone().into(),
            "--socket".into(),
            self.paths.socket.clone().into(),
            "--spec".into(),
            spec.into(),
        ]
    }
}

/// Why a supervisor didn't start.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// Running the launcher failed.
    #[error("running {exe}: {source}")]
    Spawn {
        /// The binary.
        exe: String,
        /// The error.
        #[source]
        source: std::io::Error,
    },
    /// The supervisor wasn't ready within the launch timeout.
    #[error("supervisor launch timed out")]
    Timeout,
    /// The supervisor reported an error (the launcher's last stderr line).
    #[error("supervisor failed to start: {0}")]
    Failed(String),
}

/// Starts a detached supervisor and returns its info once it is ready.
///
/// Runs `exe supervise ...` as a launcher, which re-executes itself as a `setsid`
/// child and exits once that child is ready. The supervisor is therefore not a
/// child of the caller and survives it.
pub async fn launch(exe: &Path, request: &LaunchRequest) -> Result<SupervisorInfo, LaunchError> {
    let output = tokio::process::Command::new(exe)
        .args(request.args())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(LAUNCH_TIMEOUT, output)
        .await
        .map_err(|_| LaunchError::Timeout)?
        .map_err(|source| LaunchError::Spawn {
            exe: exe.display().to_string(),
            source,
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success()
        && let Ok(info) = serde_json::from_str::<SupervisorInfo>(stdout.trim())
    {
        return Ok(info);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map_or_else(
            || format!("launcher exited with {}", output.status),
            |l| l.trim().to_owned(),
        );
    Err(LaunchError::Failed(message))
}
