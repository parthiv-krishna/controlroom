//! The runtime's [`JobHost`] port: background jobs as detached supervisors (kind `job`).
//!
//! A job runs `$SHELL -lc '<command>'` in its cwd. At levels strict and relaxed it runs under
//! nono with the **session's** generated profile and launch env file (written by the agent's
//! last launch), so it can do exactly what the agent can. Its supervisor keeps its files in
//! `<state>/sessions/<session>/jobs/<job>/` (removed with the session), listens on
//! `<state>/run/<job>.sock`, and appends the plain-text log to
//! `<session cwd>/.controlroom/jobs/<job>.log`; `.controlroom/.gitignore` (`*`) keeps the logs
//! out of version control.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::ids::Seq;
use controlroom_protocol::session::SandboxLevel;
use controlroom_runtime::ports::{
    HostError, JobHost, JobLaunch, JobRef, RecordStream, SupervisorConn,
};
use controlroom_sandbox::launch::{LaunchCommand, Nono, NonoDirs};
use controlroom_sandbox::{AbsPath, SessionLayout};
use controlroom_supervisor::{Exec, LaunchRequest, Paths, SpawnSpec};
use controlroom_transcript::{ProcessKind, StdinMode};
use futures::future::BoxFuture;

use super::agents::{DaemonAgents, sandbox_unavailable};
use super::host::{connect_writer, host_error, read_file};
use crate::dirs::Dirs;

/// Longest output line kept in full; longer ones are cut (and marked in the transcript).
const JOB_MAX_LINE_BYTES: u64 = 64 << 10;

/// The jobs of this daemon.
pub struct DaemonJobs {
    dirs: Dirs,
    /// The `controlroomd` that runs supervisors.
    controlroomd: PathBuf,
    /// The agents' launch settings (shell, nono, sandbox.toml).
    agents: Arc<DaemonAgents>,
}

impl DaemonJobs {
    /// Jobs of `dirs`, supervised by `controlroomd`.
    pub fn new(dirs: Dirs, controlroomd: PathBuf, agents: Arc<DaemonAgents>) -> Self {
        Self {
            dirs,
            controlroomd,
            agents,
        }
    }

    fn layout(&self, job: JobRef) -> Result<SessionLayout, HostError> {
        let session =
            controlroom_sandbox::SessionId::new(&job.session.to_string()).map_err(host_error)?;
        Ok(SessionLayout::new(self.dirs.state(), &session))
    }

    /// Where the job's supervisor keeps its files and listens.
    fn paths(&self, job: JobRef) -> Result<Paths, HostError> {
        let id = job.job.to_string();
        Ok(Paths {
            dir: self
                .layout(job)?
                .root()
                .join("jobs")
                .join(&id)
                .as_path()
                .to_owned(),
            socket: self.dirs.supervisor_socket(&id).as_path().to_owned(),
        })
    }
}

fn abs(path: &str) -> Result<AbsPath, RpcError> {
    AbsPath::new(path).map_err(|e| sandbox_unavailable(e.to_string()))
}

/// Creates `<session cwd>/.controlroom/jobs/` and `.controlroom/.gitignore`.
fn prepare_log_dir(log_path: &AbsPath) -> Result<(), RpcError> {
    let fs_error = |path: &AbsPath, e: std::io::Error| {
        RpcError::new(
            controlroom_protocol::ErrorData::FsError {
                path: path.as_str().into(),
                detail: e.to_string(),
            },
            format!("{path}: {e}"),
        )
    };
    let jobs = log_path
        .parent()
        .ok_or_else(|| sandbox_unavailable(format!("{log_path} has no directory")))?;
    std::fs::create_dir_all(jobs.as_path()).map_err(|e| fs_error(&jobs, e))?;
    if let Some(controlroom) = jobs.parent() {
        let ignore = controlroom.join(".gitignore");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(ignore.as_path())
        {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, b"*\n").map_err(|e| fs_error(&ignore, e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(fs_error(&ignore, e)),
        }
    }
    Ok(())
}

impl JobHost for DaemonJobs {
    fn spawn_spec(&self, launch: &JobLaunch) -> Result<SpawnSpec, RpcError> {
        let cwd = abs(launch.cwd.as_str())?;
        let log_path = abs(launch.log_path.as_str())?;
        prepare_log_dir(&log_path)?;
        let sandboxed = launch.sandbox_level != SandboxLevel::Off;
        let host = self.agents.host(sandboxed)?;
        let layout = self
            .layout(launch.job)
            .map_err(|e| sandbox_unavailable(e.0))?;
        let (cmd, env_file) = if sandboxed {
            let profile = layout.profile();
            let env_file = layout.root().join("launch.env");
            if !profile.as_path().is_file() || !env_file.as_path().is_file() {
                return Err(sandbox_unavailable(
                    "the session's sandbox profile is missing (start its agent first)".into(),
                ));
            }
            let nono = Nono {
                binary: host
                    .nono
                    .clone()
                    .ok_or_else(|| sandbox_unavailable("nono is not available".into()))?,
                dirs: NonoDirs::under(self.dirs.state()),
            };
            let cmd =
                LaunchCommand::new(Some((&nono, &profile)), &host.shell, &cwd, &launch.command);
            (cmd, Some(env_file.as_path().to_owned()))
        } else {
            (
                LaunchCommand::new(None, &host.shell, &cwd, &launch.command),
                None,
            )
        };
        let labels = BTreeMap::from([
            ("session".to_owned(), launch.job.session.to_string()),
            ("job".to_owned(), launch.job.job.to_string()),
        ]);
        Ok(SpawnSpec {
            kind: ProcessKind::Job,
            exec: Exec::Argv(std::iter::once(cmd.program).chain(cmd.args).collect()),
            cwd: cwd.as_path().to_owned(),
            env_file,
            stdin: StdinMode::Null,
            timeout_ms: launch.timeout_ms,
            max_line_bytes: Some(JOB_MAX_LINE_BYTES),
            labels,
            plain_log: Some(log_path.as_path().to_owned()),
        })
    }

    fn launch(&self, job: JobRef, spec: SpawnSpec) -> BoxFuture<'_, Result<(), HostError>> {
        Box::pin(async move {
            let request = LaunchRequest {
                id: job.job.to_string(),
                paths: self.paths(job)?,
                spec,
            };
            controlroom_supervisor::launch(&self.controlroomd, &request)
                .await
                .map(|_| ())
                .map_err(host_error)
        })
    }

    fn connect(
        &self,
        job: JobRef,
    ) -> BoxFuture<'_, Result<Option<Arc<dyn SupervisorConn>>, HostError>> {
        Box::pin(async move { connect_writer(self.paths(job)?.socket).await })
    }

    fn read_transcript(
        &self,
        job: JobRef,
        from: Seq,
    ) -> BoxFuture<'_, Result<Box<dyn RecordStream>, HostError>> {
        Box::pin(async move {
            let path = self.paths(job)?.dir.join("transcript.jsonl");
            Ok(Box::new(read_file(path, from.0)) as Box<dyn RecordStream>)
        })
    }
}
