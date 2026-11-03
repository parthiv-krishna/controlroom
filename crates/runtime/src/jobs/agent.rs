//! The agent's `job` tool through the agent socket: `run`, `follow`, `unfollow`, `status`,
//! `kill`. Every mutating call is deduplicated by the pi tool call id.

use controlroom_protocol::RpcError;
use controlroom_protocol::agent::{
    AgentJobFollowParams, AgentJobParams, AgentJobs, JobRunParams, JobRunResult, JobStatusParams,
};
use controlroom_protocol::daemon::JobFollowParams;
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::session::Job;

use super::commands::job_cwd;
use super::io;
use super::model::KilledBy;
use crate::Runtime;
use crate::agent::op_id;
use crate::commands::params_hash;

impl Runtime {
    /// `job.run`: starts a background job in the session and returns once its supervisor
    /// runs. A replayed call returns the first result without starting anything.
    pub async fn agent_job_run(
        &self,
        id: SessionId,
        params: JobRunParams,
    ) -> Result<JobRunResult, RpcError> {
        const METHOD: &str = "agent.job.run";
        self.recovered().await;
        let op = op_id(id, &params.call_id);
        let hash = params_hash(&params);
        let inner = self.inner();
        if let Some(started) = inner.with_core(|core| core.stored_op(METHOD, &op, &hash))? {
            return Ok(started);
        }
        let session_cwd =
            inner.with_core(|core| Ok::<_, RpcError>(core.session(id)?.data.cwd.clone()))?;
        let cwd = job_cwd(&session_cwd, params.cwd.as_ref())?;
        let cwd = inner.host.resolve_cwd(&cwd).await?;
        let mut launch = None;
        let started = inner.transact(|core| {
            core.with_op(METHOD, &op, &params, |core| {
                let (started, job) = core.job_start(id, &params, cwd)?;
                launch = Some(job);
                Ok(started)
            })
        })?;
        if let Some(launch) = launch {
            io::launch(inner, launch).await?;
        }
        Ok(started)
    }

    /// `job.follow`.
    pub async fn agent_job_follow(
        &self,
        id: SessionId,
        params: AgentJobFollowParams,
    ) -> Result<Job, RpcError> {
        let follow = JobFollowParams {
            op_id: op_id(id, &params.call_id),
            id,
            job_id: params.job_id,
            buffer_seconds: params.buffer_seconds,
            pattern: params.pattern,
            from: params.from,
        };
        self.follow_job("agent.job.follow", follow).await
    }

    /// `job.unfollow`.
    pub async fn agent_job_unfollow(
        &self,
        id: SessionId,
        params: AgentJobParams,
    ) -> Result<Job, RpcError> {
        let op = op_id(id, &params.call_id);
        self.command("agent.job.unfollow", &op, &params, |core| {
            core.job_unfollow(id, &params.job_id)
        })
        .await
    }

    /// `job.kill`: the agent knows, so no exit wake follows.
    pub async fn agent_job_kill(
        &self,
        id: SessionId,
        params: AgentJobParams,
    ) -> Result<Job, RpcError> {
        let op = op_id(id, &params.call_id);
        self.command("agent.job.kill", &op, &params, |core| {
            core.job_kill(id, &params.job_id, KilledBy::Agent)
        })
        .await
    }

    /// `job.status`: one job, or all of them, newest first.
    pub async fn agent_job_status(
        &self,
        id: SessionId,
        params: JobStatusParams,
    ) -> Result<AgentJobs, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            let jobs = match params.job_id {
                Some(job) => vec![core.job_view(id, &job)?],
                None => core.job_views(core.session(id)?),
            };
            Ok(AgentJobs { jobs })
        })
    }
}
