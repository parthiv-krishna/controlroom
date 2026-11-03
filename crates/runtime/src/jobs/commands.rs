//! Job commands under the runtime's lock: start, kill, follow, unfollow.

use std::time::Duration;

use controlroom_protocol::daemon::{FollowFrom, JobFollowParams};
use controlroom_protocol::ids::{JobId, RemotePath, SessionId};
use controlroom_protocol::session::{Job, SessionState};
use controlroom_protocol::{Resource, RpcError};
use regex::{Regex, RegexBuilder};

use super::JobLive;
use super::api::{JobRun, JobStarted};
use super::model::{FollowSpec, JobEnd, JobRecord, KilledBy};
use super::progress::Matcher;
use crate::core::{Core, Task};
use crate::errors::{invalid_params, invalid_state, not_found};
use crate::ports::{JobLaunch, JobRef};

/// SIGTERM to SIGKILL for a killed job.
const KILL_GRACE: Duration = Duration::from_secs(5);
/// The follow batching window when none is given.
const DEFAULT_BUFFER_SECONDS: u32 = 15;
/// The longest batching window.
const MAX_BUFFER_SECONDS: u32 = 3600;
/// The longest follow pattern, and the compiled size limit.
const MAX_PATTERN_LEN: usize = 1000;
const PATTERN_SIZE_LIMIT: usize = 1 << 20;
/// Display names are cut to this many characters.
const NAME_CHARS: usize = 40;

/// Lines a rescan must match against a new follow pattern: the ones folded before it.
pub(crate) struct Rescan {
    pub job: JobRef,
    pub generation: u64,
    pub from_line: u64,
    pub to_line: u64,
    pub pattern: Regex,
}

impl Core {
    /// Records a new running job and its timeline item; returns what to launch.
    pub(crate) fn job_start(
        &mut self,
        id: SessionId,
        run: &JobRun,
        cwd: RemotePath,
    ) -> Result<(JobStarted, JobLaunch), RpcError> {
        if run.command.trim().is_empty() {
            return Err(invalid_params("the command is empty"));
        }
        if run.timeout_s == Some(0) {
            return Err(invalid_params("timeout_s must be positive"));
        }
        let now = self.clock.now();
        let head = self.head(id);
        let session = self.session_mut(id)?;
        if session.data.closed() {
            return Err(invalid_state(SessionState::Closed, "job run"));
        }
        let job = JobId::generate();
        let log_path = log_path(&session.data.cwd, &job);
        let record = JobRecord {
            id: job,
            name: display_name(run.name.as_deref().unwrap_or(&run.command)),
            command: run.command.clone(),
            cwd: cwd.clone(),
            log_path: log_path.clone(),
            timeout_s: run.timeout_s,
            started_at: now,
            seq: head,
            follow: None,
            notified_line: 0,
            kill: None,
            end: None,
            exit_woken: false,
        };
        session.data.jobs.insert(job, record);
        let launch = JobLaunch {
            job: JobRef { session: id, job },
            command: run.command.clone(),
            cwd,
            session_cwd: session.data.cwd.clone(),
            log_path: log_path.clone(),
            timeout_ms: run.timeout_s.map(|s| u64::from(s) * 1000),
            sandbox_level: session.data.sandbox_level(),
        };
        self.publish_job(id, &job)?;
        Ok((
            JobStarted {
                job_id: job,
                log_path,
            },
            launch,
        ))
    }

    /// The job's supervisor didn't start: it ends as failed, without a wake (the caller of
    /// `job run` got the error).
    pub(crate) fn job_launch_failed(&mut self, job: JobRef, error: &str) -> Result<(), RpcError> {
        let now = self.clock.now();
        let Ok(session) = self.session_mut(job.session) else {
            return Ok(());
        };
        let Some(record) = session.data.jobs.get_mut(&job.job) else {
            return Ok(());
        };
        record.end = Some(JobEnd {
            status: controlroom_protocol::session::JobStatus::Failed,
            exit: None,
            signal: None,
            ended_at: now,
            lines: 0,
            error: Some(error.to_owned()),
        });
        record.exit_woken = true;
        self.jobs.remove(&job.job);
        self.publish_job(job.session, &job.job)
    }

    /// `job.kill`: SIGTERM, then SIGKILL after a grace period. The exit record says the rest.
    pub(crate) fn job_kill(
        &mut self,
        id: SessionId,
        job: &JobId,
        by: KilledBy,
    ) -> Result<Job, RpcError> {
        let record = self.job_record_mut(id, job)?;
        if record.running() && record.kill.is_none() {
            record.kill = Some(by);
            let conn = self.jobs.get(job).and_then(|j| j.conn.clone());
            if let Some(conn) = conn {
                self.task(Task::Terminate(conn, KILL_GRACE));
            }
        }
        self.job_view(id, job)
    }

    /// Kills every running job of a session that is closing or being deleted.
    pub(crate) fn kill_session_jobs(&mut self, id: SessionId) -> Result<(), RpcError> {
        let running: Vec<JobId> = self
            .session(id)?
            .data
            .jobs
            .values()
            .filter(|j| j.running())
            .map(|j| j.id)
            .collect();
        for job in running {
            self.job_kill(id, &job, KilledBy::User)?;
        }
        Ok(())
    }

    /// `job.follow`: starts or updates the follow subscription. Returns a rescan when lines
    /// folded before it must be matched against the new pattern.
    pub(crate) fn job_follow(
        &mut self,
        params: &JobFollowParams,
    ) -> Result<(Job, Option<Rescan>), RpcError> {
        let buffer_seconds = params
            .buffer_seconds
            .unwrap_or(DEFAULT_BUFFER_SECONDS)
            .min(MAX_BUFFER_SECONDS);
        let pattern = params
            .pattern
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(compile)
            .transpose()?;
        let (id, job) = (params.id, params.job_id);
        let lines = self.jobs.get(&job).map_or(0, |j| j.progress.lines);
        let record = self.job_record_mut(id, &job)?;
        if !record.running() {
            return Ok((self.job_view(id, &job)?, None));
        }
        let since_line = match params.from {
            FollowFrom::Now => lines,
            FollowFrom::Unseen => record.notified_line,
        };
        record.follow = Some(FollowSpec {
            buffer_seconds,
            pattern: params.pattern.clone().filter(|p| !p.is_empty()),
            since_line,
        });
        let baseline = record.follow_baseline();
        let live = self.jobs.entry(job).or_insert_with(|| JobLive::new(id));
        live.follow_generation += 1;
        let generation = live.follow_generation;
        let rescan = match pattern {
            Some(pattern) => {
                live.progress.follow(Matcher::Pattern(pattern.clone()));
                (lines > baseline).then(|| Rescan {
                    job: JobRef { session: id, job },
                    generation,
                    from_line: baseline + 1,
                    to_line: lines,
                    pattern,
                })
            }
            None => {
                live.progress.follow(Matcher::Any);
                None
            }
        };
        self.schedule_follow_wake(id, &job);
        self.publish_job(id, &job)?;
        Ok((self.job_view(id, &job)?, rescan))
    }

    /// `job.unfollow`: no more follow wakes; the exit wake stays on.
    pub(crate) fn job_unfollow(&mut self, id: SessionId, job: &JobId) -> Result<Job, RpcError> {
        let record = self.job_record_mut(id, job)?;
        if record.follow.take().is_some() {
            if let Some(live) = self.jobs.get_mut(job) {
                live.follow_generation += 1;
                live.progress.unfollow();
            }
            self.publish_job(id, job)?;
        }
        self.job_view(id, job)
    }

    /// Matches a rescan found among lines folded before the follow started.
    pub(crate) fn job_rescanned(
        &mut self,
        job: JobRef,
        generation: u64,
        matches: &[u64],
    ) -> Result<(), RpcError> {
        let Some(live) = self.jobs.get_mut(&job.job) else {
            return Ok(());
        };
        if live.follow_generation != generation {
            return Ok(());
        }
        live.progress.add_matches(matches);
        self.schedule_follow_wake(job.session, &job.job);
        Ok(())
    }

    /// The job's record, for a change (`not_found` if the session has no such job).
    pub(crate) fn job_record_mut(
        &mut self,
        id: SessionId,
        job: &JobId,
    ) -> Result<&mut JobRecord, RpcError> {
        self.session_mut(id)?
            .data
            .jobs
            .get_mut(job)
            .ok_or_else(|| not_found(Resource::Job, job))
    }

    /// The job as the wire shows it.
    pub(crate) fn job_view(&self, id: SessionId, job: &JobId) -> Result<Job, RpcError> {
        let record = self
            .session(id)?
            .data
            .jobs
            .get(job)
            .ok_or_else(|| not_found(Resource::Job, job))?;
        Ok(record.to_job(self.jobs.get(job).map(|j| &j.progress)))
    }
}

/// `<session cwd>/.controlroom/jobs/<id>.log`.
pub(crate) fn log_path(session_cwd: &RemotePath, job: &JobId) -> RemotePath {
    let cwd = session_cwd.as_str().trim_end_matches('/');
    RemotePath::new(format!("{cwd}/.controlroom/jobs/{job}.log"))
}

/// The job's working directory: `cwd` (absolute, or relative to the session's) normalized, and
/// inside the session's working directory.
pub(crate) fn job_cwd(
    session_cwd: &RemotePath,
    cwd: Option<&RemotePath>,
) -> Result<RemotePath, RpcError> {
    let base = session_cwd.as_str();
    let joined = match cwd.map(RemotePath::as_str) {
        None | Some("") => return Ok(session_cwd.clone()),
        Some(path) if path.starts_with('/') => path.to_owned(),
        Some(path) => format!("{base}/{path}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let normalized = format!("/{}", parts.join("/"));
    let root = base.trim_end_matches('/');
    let inside =
        normalized == root || normalized.starts_with(&format!("{root}/")) || root.is_empty();
    if !inside {
        return Err(invalid_params(format!(
            "the job's cwd {normalized} is outside the session's working directory {base}"
        )));
    }
    Ok(RemotePath::new(normalized))
}

fn compile(pattern: &str) -> Result<Regex, RpcError> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(invalid_params("the follow pattern is too long"));
    }
    RegexBuilder::new(pattern)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|e| invalid_params(format!("invalid follow pattern: {e}")))
}

/// A one-line display name of at most [`NAME_CHARS`] characters.
fn display_name(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("job");
    if line.chars().count() <= NAME_CHARS {
        return line.to_owned();
    }
    let mut name: String = line.chars().take(NAME_CHARS - 1).collect();
    name.push('…');
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_cwds_stay_inside_the_session() {
        let session = RemotePath::new("/home/alice/app");
        let cwd = |p: &str| job_cwd(&session, Some(&RemotePath::new(p)));
        assert_eq!(job_cwd(&session, None).unwrap(), session);
        assert_eq!(cwd("web/./src/..").unwrap().as_str(), "/home/alice/app/web");
        assert_eq!(
            cwd("/home/alice/app/x").unwrap().as_str(),
            "/home/alice/app/x"
        );
        assert!(cwd("..").is_err());
        assert!(cwd("/home/alice/application").is_err());
        assert!(cwd("/etc").is_err());
    }

    #[test]
    fn names_and_log_paths() {
        assert_eq!(display_name("  \n cargo test --all \n"), "cargo test --all");
        let long = display_name(&"x".repeat(100));
        assert_eq!(long.chars().count(), NAME_CHARS);
        assert!(long.ends_with('…'));
        let job: JobId = "01J00000000000000000000000".parse().unwrap();
        assert_eq!(
            log_path(&RemotePath::new("/srv/app/"), &job).as_str(),
            "/srv/app/.controlroom/jobs/01J00000000000000000000000.log"
        );
        assert!(compile("(").is_err());
        assert!(compile("FAIL|panicked").is_ok());
    }
}
