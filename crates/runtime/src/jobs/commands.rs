//! Job commands. Starting, following, unfollowing and killing a job go to its host as `tool.run`
//! ops, at most once by call id (the agent's tool call id, or `op:<op id>` for the user's
//! commands); the outcome then also shows in the job event log. Status and lists come from the
//! jobs as the event log last described them (a job on an unreachable host keeps its last known
//! state), and `job.logs` reads the log on its host.

use controlroom_protocol::agent::{
    AgentJobFollowParams, AgentJobParams, AgentJobs, JobRunParams, JobRunResult, JobStatusParams,
};
use controlroom_protocol::agentd::{
    FollowFrom, JobFollowParams, JobListParams, JobListResult, JobLogs, JobLogsParams, JobOpParams,
};
use controlroom_protocol::host::{
    HostOp, HostRequest, HostResult, KilledBy, ReadContent, ToolRunParams,
};
use controlroom_protocol::ids::{CallId, HostOpId, JobId, MachineId, OpId, SessionId, WorkspaceId};
use controlroom_protocol::session::{Job, SessionState};
use controlroom_protocol::{ErrorData, Resource, RpcError};
use regex::RegexBuilder;
use serde::Serialize;

use super::model::{FollowSpec, JobRecord, Pending};
use crate::agent::op_id;
use crate::commands::params_hash;
use crate::core::Core;
use crate::errors::{invalid_params, invalid_state, not_found};
use crate::hosts::{parse, session_request};
use crate::runtime::Runtime;

/// The follow batching window when none is given.
const DEFAULT_BUFFER_SECONDS: u32 = 15;
/// The longest batching window.
const MAX_BUFFER_SECONDS: u32 = 3600;
/// The longest follow pattern, and its compiled size limit.
const MAX_PATTERN_LEN: usize = 1000;
const PATTERN_SIZE_LIMIT: usize = 1 << 20;
/// Lines `job.logs` returns when the caller doesn't say, and at most.
const DEFAULT_LOG_LINES: u32 = 200;
const MAX_LOG_LINES: u32 = 2000;

/// A follow, unfollow or kill of one job.
struct JobOp<'a, P> {
    method: &'a str,
    op_id: OpId,
    params: &'a P,
    session: SessionId,
    job: JobId,
    call_id: CallId,
    op: HostOp,
}

impl Runtime {
    /// The agent's `job.run`: starts a background job on a workspace's host and returns once
    /// the host has it. A replayed call returns the first result without starting anything.
    pub async fn agent_job_run(
        &self,
        id: SessionId,
        params: JobRunParams,
    ) -> Result<JobRunResult, RpcError> {
        const METHOD: &str = "agent.job.run";
        self.recovered().await;
        let op = op_id(id, &params.call_id);
        let inner = self.inner();
        if let Some(started) =
            inner.with_core(|core| core.stored_op(METHOD, &op, &params_hash(&params)))?
        {
            return Ok(started);
        }
        if params.command.trim().is_empty() {
            return Err(invalid_params("the command is empty"));
        }
        if params.timeout_s == Some(0) {
            return Err(invalid_params("timeout_s must be positive"));
        }
        let (machine, workspace) = inner.with_core(|core| core.job_place(id, &params))?;
        let request = HostRequest::ToolRun(ToolRunParams {
            call_id: CallId::new(params.call_id.as_str()),
            workspace_id: workspace,
            op: HostOp::JobRun {
                command: params.command.clone(),
                cwd: params.cwd.clone(),
                name: params.name.clone(),
                timeout_s: params.timeout_s,
                follow: None,
            },
        });
        let key = HostOpId::new(params.call_id.as_str());
        let (value, _) = session_request(inner, id, machine, key, request, None).await?;
        let HostResult::JobRun { job } = parse(value)? else {
            return Err(unexpected());
        };
        self.command(METHOD, &op, &params, |core| {
            core.job_started(id, machine, workspace, &job)?;
            Ok(JobRunResult {
                job_id: job.id,
                log_path: job.log_path.clone(),
            })
        })
        .await
    }

    /// The agent's `job.follow`.
    pub async fn agent_job_follow(
        &self,
        id: SessionId,
        params: AgentJobFollowParams,
    ) -> Result<Job, RpcError> {
        let op = follow_op(
            &params.pattern,
            params.buffer_seconds,
            params.from,
            params.job_id,
        )?;
        self.job_op(JobOp {
            method: "agent.job.follow",
            op_id: op_id(id, &params.call_id),
            params: &params,
            session: id,
            job: params.job_id,
            call_id: CallId::new(params.call_id.as_str()),
            op,
        })
        .await
    }

    /// The agent's `job.unfollow`.
    pub async fn agent_job_unfollow(
        &self,
        id: SessionId,
        params: AgentJobParams,
    ) -> Result<Job, RpcError> {
        self.job_op(JobOp {
            method: "agent.job.unfollow",
            op_id: op_id(id, &params.call_id),
            params: &params,
            session: id,
            job: params.job_id,
            call_id: CallId::new(params.call_id.as_str()),
            op: HostOp::JobUnfollow {
                job_id: params.job_id,
            },
        })
        .await
    }

    /// The agent's `job.kill`: the agent knows, so no exit wake follows.
    pub async fn agent_job_kill(
        &self,
        id: SessionId,
        params: AgentJobParams,
    ) -> Result<Job, RpcError> {
        self.inner()
            .transact(|core| core.job_kill_requested(id, &params.job_id, KilledBy::Agent))?;
        self.job_op(JobOp {
            method: "agent.job.kill",
            op_id: op_id(id, &params.call_id),
            params: &params,
            session: id,
            job: params.job_id,
            call_id: CallId::new(params.call_id.as_str()),
            op: HostOp::JobKill {
                job_id: params.job_id,
                by: KilledBy::Agent,
            },
        })
        .await
    }

    /// The agent's `job.status`: one job, or all of them, newest first, as last known.
    pub async fn agent_job_status(
        &self,
        id: SessionId,
        params: JobStatusParams,
    ) -> Result<AgentJobs, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            let jobs = match params.job_id {
                Some(job) => vec![core.job_view(id, &job)?],
                None => core.job_views(id)?,
            };
            Ok(AgentJobs { jobs })
        })
    }

    /// `job.list`: every job of the session, newest first.
    pub async fn job_list(&self, params: JobListParams) -> Result<JobListResult, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            Ok(JobListResult {
                jobs: core.job_views(params.id)?,
            })
        })
    }

    /// `job.logs`: a range of the job's log, read on its host.
    pub async fn job_logs(&self, params: JobLogsParams) -> Result<JobLogs, RpcError> {
        self.recovered().await;
        let inner = self.inner();
        let record = inner.with_core(|core| core.job_record(params.id, &params.job_id))?;
        let total = record.end.as_ref().map_or(record.lines, |e| e.lines);
        let max = u64::from(
            params
                .max_lines
                .unwrap_or(DEFAULT_LOG_LINES)
                .clamp(1, MAX_LOG_LINES),
        );
        let from_line = params
            .from_line
            .unwrap_or_else(|| total.saturating_sub(max) + 1)
            .max(1);
        let call_id = CallId::new(format!("logs:{}", ulid::Ulid::generate()));
        let request = HostRequest::ToolRun(ToolRunParams {
            call_id: call_id.clone(),
            workspace_id: record.workspace_id,
            op: HostOp::Read {
                path: record.log_path.clone(),
                offset: Some(from_line),
                limit: Some(max),
            },
        });
        let key = HostOpId::new(call_id.as_str());
        let (value, _) =
            session_request(inner, params.id, record.machine_id, key, request, None).await?;
        inner.with_core(|core| {
            core.acks
                .entry(record.machine_id)
                .or_default()
                .push(call_id)
        });
        let HostResult::Read { content, .. } = parse(value)? else {
            return Err(unexpected());
        };
        let (lines, start_line) = match content {
            ReadContent::Text {
                lines, start_line, ..
            } => (lines, start_line),
            _ => (Vec::new(), from_line),
        };
        Ok(JobLogs {
            from_line: start_line,
            lines,
            total_lines: total,
        })
    }

    /// The user's `job.kill`: the exit wakes the agent.
    pub async fn job_kill(&self, params: JobOpParams) -> Result<Job, RpcError> {
        self.inner()
            .transact(|core| core.job_kill_requested(params.id, &params.job_id, KilledBy::User))?;
        self.job_op(JobOp {
            method: "job.kill",
            op_id: params.op_id.clone(),
            params: &params,
            session: params.id,
            job: params.job_id,
            call_id: user_call(&params.op_id),
            op: HostOp::JobKill {
                job_id: params.job_id,
                by: KilledBy::User,
            },
        })
        .await
    }

    /// The user's `job.follow`: wake the agent on the job's new or matching lines.
    pub async fn job_follow(&self, params: JobFollowParams) -> Result<Job, RpcError> {
        let op = follow_op(
            &params.pattern,
            params.buffer_seconds,
            params.from,
            params.job_id,
        )?;
        self.job_op(JobOp {
            method: "job.follow",
            op_id: params.op_id.clone(),
            params: &params,
            session: params.id,
            job: params.job_id,
            call_id: user_call(&params.op_id),
            op,
        })
        .await
    }

    /// The user's `job.unfollow`: only the exit wake stays.
    pub async fn job_unfollow(&self, params: JobOpParams) -> Result<Job, RpcError> {
        self.job_op(JobOp {
            method: "job.unfollow",
            op_id: params.op_id.clone(),
            params: &params,
            session: params.id,
            job: params.job_id,
            call_id: user_call(&params.op_id),
            op: HostOp::JobUnfollow {
                job_id: params.job_id,
            },
        })
        .await
    }

    /// Runs a job op on the job's host once per op id and returns the job after it.
    async fn job_op<P: Serialize>(&self, op: JobOp<'_, P>) -> Result<Job, RpcError> {
        self.recovered().await;
        let inner = self.inner();
        let hash = params_hash(op.params);
        if let Some(job) = inner.with_core(|core| core.stored_op(op.method, &op.op_id, &hash))? {
            return Ok(job);
        }
        let record = inner.with_core(|core| core.job_record(op.session, &op.job))?;
        let request = HostRequest::ToolRun(ToolRunParams {
            call_id: op.call_id.clone(),
            workspace_id: record.workspace_id,
            op: op.op,
        });
        let key = HostOpId::new(op.call_id.as_str());
        let (value, _) =
            session_request(inner, op.session, record.machine_id, key, request, None).await?;
        let job = match parse(value)? {
            HostResult::JobFollow { job }
            | HostResult::JobUnfollow { job }
            | HostResult::JobKill { job } => job,
            _ => return Err(unexpected()),
        };
        self.command(op.method, &op.op_id, op.params, |core| {
            core.job_updated(op.session, &job)?;
            core.job_view(op.session, &job.id)
        })
        .await
    }
}

/// The host op of a follow, validated.
fn follow_op(
    pattern: &Option<String>,
    buffer_seconds: Option<u32>,
    from: FollowFrom,
    job_id: JobId,
) -> Result<HostOp, RpcError> {
    let pattern = pattern.clone().filter(|p| !p.is_empty());
    if let Some(pattern) = &pattern {
        check_pattern(pattern)?;
    }
    Ok(HostOp::JobFollow {
        job_id,
        pattern,
        buffer_seconds: Some(
            buffer_seconds
                .unwrap_or(DEFAULT_BUFFER_SECONDS)
                .min(MAX_BUFFER_SECONDS),
        ),
        from,
    })
}

fn check_pattern(pattern: &str) -> Result<(), RpcError> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(invalid_params("the follow pattern is too long"));
    }
    RegexBuilder::new(pattern)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map(|_| ())
        .map_err(|e| invalid_params(format!("invalid follow pattern: {e}")))
}

/// The call id of a user's job command.
fn user_call(op_id: &OpId) -> CallId {
    CallId::new(format!("op:{op_id}"))
}

fn unexpected() -> RpcError {
    RpcError::new(
        ErrorData::InvalidResponse,
        "the host answered with an unexpected result",
    )
}

impl Core {
    /// Where the agent's job runs: the named machine's (or the primary) workspace, the one
    /// holding an absolute `cwd` if several are there.
    fn job_place(
        &self,
        id: SessionId,
        params: &JobRunParams,
    ) -> Result<(MachineId, WorkspaceId), RpcError> {
        let data = &self.session(id)?.data;
        if data.closed() {
            return Err(invalid_state(SessionState::Closed, "job run"));
        }
        let machine = match &params.machine {
            Some(name) => self
                .machines
                .by_name(name)
                .filter(|m| data.workspaces.iter().any(|w| w.machine_id == *m))
                .ok_or_else(|| not_found(Resource::Workspace, name))?,
            None => data
                .workspaces
                .first()
                .map(|w| w.machine_id)
                .ok_or_else(|| not_found(Resource::Workspace, "primary"))?,
        };
        let on_machine = || data.workspaces.iter().filter(|w| w.machine_id == machine);
        let workspace = params
            .cwd
            .as_ref()
            .and_then(|cwd| on_machine().find(|w| w.contains(cwd.as_str())))
            .or_else(|| on_machine().next())
            .map(|w| w.id)
            .ok_or_else(|| not_found(Resource::Workspace, machine))?;
        Ok((machine, workspace))
    }

    /// Records a job the host started (unless its `started` event did already).
    fn job_started(
        &mut self,
        id: SessionId,
        machine: MachineId,
        workspace: WorkspaceId,
        job: &Job,
    ) -> Result<(), RpcError> {
        let head = self.head(id);
        let session = self.session_mut(id)?;
        if session.data.jobs.contains_key(&job.id) {
            return Ok(());
        }
        session.data.jobs.insert(
            job.id,
            JobRecord {
                id: job.id,
                machine_id: machine,
                workspace_id: workspace,
                name: job.name.clone(),
                command: job.command.clone(),
                cwd: job.cwd.clone(),
                log_path: job.log_path.clone(),
                timeout_s: job.timeout_s,
                started_at: job.started_at,
                seq: head,
                lines: job.lines,
                follow: None,
                notified_line: 0,
                pending: Pending::default(),
                first_match_at: None,
                kill: None,
                end: None,
                exit_woken: false,
                wakes: Vec::new(),
            },
        );
        self.publish_job(id, &job.id)
    }

    /// Takes the host's view of a job after a command: its follow subscription and line count.
    fn job_updated(&mut self, id: SessionId, job: &Job) -> Result<(), RpcError> {
        let record = self.job_record_mut(id, &job.id)?;
        if record.running() {
            record.follow = job.follow.as_ref().map(|f| FollowSpec {
                buffer_seconds: f.buffer_seconds,
                pattern: f.pattern.clone(),
            });
            if record.follow.is_none() {
                record.pending = Pending::default();
                record.first_match_at = None;
            }
        }
        record.lines = record.lines.max(job.lines);
        self.publish_job(id, &job.id)
    }

    /// Records who asked to kill a job, before the kill reaches its host (so its exit is
    /// judged right whichever arrives first).
    fn job_kill_requested(
        &mut self,
        id: SessionId,
        job: &JobId,
        by: KilledBy,
    ) -> Result<(), RpcError> {
        let record = self.job_record_mut(id, job)?;
        if record.running() {
            record.kill.get_or_insert(by);
        }
        Ok(())
    }

    /// Kills every running job of a session that is closing.
    pub(crate) fn kill_session_jobs(&mut self, id: SessionId) -> Result<(), RpcError> {
        let running: Vec<(JobId, MachineId, WorkspaceId)> = self
            .session(id)?
            .data
            .jobs
            .values()
            .filter(|j| j.running())
            .map(|j| (j.id, j.machine_id, j.workspace_id))
            .collect();
        for (job, machine, workspace) in running {
            self.job_kill_requested(id, &job, KilledBy::User)?;
            let call_id = CallId::new(format!("kill:{job}"));
            self.host_send(
                machine,
                Some(id),
                HostOpId::new(call_id.as_str()),
                HostRequest::ToolRun(ToolRunParams {
                    call_id,
                    workspace_id: workspace,
                    op: HostOp::JobKill {
                        job_id: job,
                        by: KilledBy::User,
                    },
                }),
            );
        }
        Ok(())
    }

    /// A copy of the job's record (`not_found` if the session has no such job).
    fn job_record(&self, id: SessionId, job: &JobId) -> Result<JobRecord, RpcError> {
        self.session(id)?
            .data
            .jobs
            .get(job)
            .cloned()
            .ok_or_else(|| not_found(Resource::Job, job))
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
        Ok(self.job_record(id, job)?.to_job())
    }

    /// The session's jobs, newest first.
    fn job_views(&self, id: SessionId) -> Result<Vec<Job>, RpcError> {
        Ok(self
            .session(id)?
            .data
            .jobs
            .values()
            .rev()
            .map(JobRecord::to_job)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_are_validated() {
        let job: JobId = "01J00000000000000000000000".parse().unwrap();
        assert!(follow_op(&Some("(".into()), None, FollowFrom::Unseen, job).is_err());
        let HostOp::JobFollow {
            pattern,
            buffer_seconds,
            ..
        } = follow_op(&Some(String::new()), Some(99_999), FollowFrom::Now, job).unwrap()
        else {
            panic!("a follow op");
        };
        assert_eq!((pattern, buffer_seconds), (None, Some(MAX_BUFFER_SECONDS)));
        assert!(check_pattern("FAIL|panicked").is_ok());
    }
}
