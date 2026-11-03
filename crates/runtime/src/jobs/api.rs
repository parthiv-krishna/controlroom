//! The [`Runtime`]'s `job.*` methods of the daemon API (the user's side).

use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::{
    JobFollowParams, JobListParams, JobListResult, JobLogs, JobLogsParams, JobOpParams,
};
use controlroom_protocol::ids::Seq;
use controlroom_protocol::session::Job;
use controlroom_supervisor::plain::plain_line;
use controlroom_transcript::Body;

use super::io;
use super::model::KilledBy;
use super::progress::line_seq;
use crate::Runtime;
use crate::errors::internal;
use crate::ports::JobRef;

/// Lines `job.logs` returns when the caller doesn't say.
const DEFAULT_LOG_LINES: u32 = 200;
/// The most lines one `job.logs` returns.
const MAX_LOG_LINES: u32 = 2000;

impl Runtime {
    /// `job.list`: every job of the session, newest first.
    pub async fn job_list(&self, params: JobListParams) -> Result<JobListResult, RpcError> {
        self.recovered().await;
        self.inner().with_core(|core| {
            let session = core.session(params.id)?;
            Ok(JobListResult {
                jobs: core.job_views(session),
            })
        })
    }

    /// `job.logs`: a range of the job's log, read from its transcript (the log file is the
    /// agent's to read and may be gone).
    pub async fn job_logs(&self, params: JobLogsParams) -> Result<JobLogs, RpcError> {
        self.recovered().await;
        let job = JobRef {
            session: params.id,
            job: params.job_id,
        };
        let total = self.inner().with_core(|core| {
            let ended = core.job_view(params.id, &params.job_id)?.lines;
            Ok::<_, RpcError>(core.jobs.get(&job.job).map_or(ended, |j| j.progress.lines))
        })?;
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
        let mut lines = Vec::new();
        if from_line <= total {
            let mut stream = self
                .inner()
                .job_host
                .read_transcript(job, Seq(line_seq(from_line)))
                .await
                .map_err(internal)?;
            while let Some(record) = stream.next().await {
                if let Body::Out(line) | Body::Stderr(line) = &record.body {
                    lines.push(plain_line(&line.raw).into_owned());
                    if lines.len() as u64 >= max.min(total - from_line + 1) {
                        break;
                    }
                }
            }
        }
        Ok(JobLogs {
            from_line,
            lines,
            total_lines: total,
        })
    }

    /// `job.kill` by the user: the exit wakes the agent.
    pub async fn job_kill(&self, params: JobOpParams) -> Result<Job, RpcError> {
        self.command("job.kill", &params.op_id, &params, |core| {
            core.job_kill(params.id, &params.job_id, KilledBy::User)
        })
        .await
    }

    /// `job.follow`: wake the agent on the job's new or matching lines.
    pub async fn job_follow(&self, params: JobFollowParams) -> Result<Job, RpcError> {
        self.follow_job("job.follow", params).await
    }

    pub(super) async fn follow_job(
        &self,
        method: &str,
        params: JobFollowParams,
    ) -> Result<Job, RpcError> {
        let mut rescan = None;
        let job = self
            .command(method, &params.op_id, &params, |core| {
                let (job, scan) = core.job_follow(&params)?;
                rescan = scan;
                Ok(job)
            })
            .await?;
        if let Some(rescan) = rescan {
            io::rescan(self.inner(), rescan);
        }
        Ok(job)
    }

    /// `job.unfollow`: only the exit wake stays.
    pub async fn job_unfollow(&self, params: JobOpParams) -> Result<Job, RpcError> {
        self.command("job.unfollow", &params.op_id, &params, |core| {
            core.job_unfollow(params.id, &params.job_id)
        })
        .await
    }
}
