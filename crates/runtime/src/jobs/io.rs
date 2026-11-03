//! The async half of jobs: launching supervisors, following their transcripts, rescanning for
//! a new follow pattern, and recovery.

use std::sync::Arc;

use controlroom_protocol::ids::Seq;
use controlroom_protocol::{ErrorData, RpcError};
use controlroom_supervisor::plain::plain_line;
use controlroom_transcript::Body;

use super::commands::Rescan;
use super::progress::line_seq;
use crate::errors::internal;
use crate::ports::{JobLaunch, JobRef, SupervisorConn};
use crate::runtime::Inner;

/// Starts the job's supervisor and follows it. A failure ends the job as failed.
pub(crate) async fn launch(inner: &Arc<Inner>, launch: JobLaunch) -> Result<(), RpcError> {
    let job = launch.job;
    match start(inner, launch).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("job {}: launch failed: {}", job.job, e.message);
            let _ = inner.transact(|core| core.job_launch_failed(job, &e.message));
            Err(e)
        }
    }
}

async fn start(inner: &Arc<Inner>, launch: JobLaunch) -> Result<(), RpcError> {
    let job = launch.job;
    let host = inner.job_host.clone();
    let spec = tokio::task::spawn_blocking(move || host.spawn_spec(&launch))
        .await
        .map_err(internal)??;
    inner
        .job_host
        .launch(job, spec)
        .await
        .map_err(|e| RpcError::new(ErrorData::InternalError, format!("starting the job: {e}")))?;
    let conn = inner.job_host.connect(job).await.map_err(internal)?;
    follow(inner, job, conn);
    Ok(())
}

/// Attaches the job's supervisor (when `conn` is given) and follows it, then (or else) its
/// transcript file to the end.
fn follow(inner: &Arc<Inner>, job: JobRef, conn: Option<Arc<dyn SupervisorConn>>) {
    let tracked = inner.transact(|core| core.job_attach(job, conn.clone()));
    if !matches!(tracked, Ok(true)) {
        return;
    }
    let inner2 = inner.clone();
    inner.spawn(async move {
        if let Some(conn) = conn {
            read_stream(&inner2, job, conn).await;
        }
        catch_up(&inner2, job).await;
        let _ = inner2.transact(|core| core.job_detached(job));
    });
}

async fn read_stream(inner: &Arc<Inner>, job: JobRef, conn: Arc<dyn SupervisorConn>) {
    let Some(from) = inner.with_core(|core| core.jobs.get(&job.job).map(|j| j.progress.seq + 1))
    else {
        return;
    };
    match conn.read(Seq(from)).await {
        Ok(mut stream) => {
            while let Some(record) = stream.next().await {
                let _ = inner.transact(|core| core.job_record(job, &record));
            }
        }
        Err(e) => tracing::warn!("job {}: reading its supervisor: {e}", job.job),
    }
}

/// Folds the job's transcript file from where the job's progress is to its end.
async fn catch_up(inner: &Arc<Inner>, job: JobRef) {
    let Some(from) = inner.with_core(|core| core.jobs.get(&job.job).map(|j| j.progress.seq + 1))
    else {
        return;
    };
    let mut stream = match inner.job_host.read_transcript(job, Seq(from)).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::warn!("job {}: reading its transcript: {e}", job.job);
            return;
        }
    };
    while let Some(record) = stream.next().await {
        let _ = inner.transact(|core| core.job_record(job, &record));
    }
}

/// Matches lines folded before a new follow pattern, then hands the matches to the core.
pub(crate) fn rescan(inner: &Arc<Inner>, rescan: Rescan) {
    let inner2 = inner.clone();
    inner.spawn(async move {
        let Rescan {
            job,
            generation,
            from_line,
            to_line,
            pattern,
        } = rescan;
        let mut stream = match inner2
            .job_host
            .read_transcript(job, Seq(line_seq(from_line)))
            .await
        {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!("job {}: rescanning its transcript: {e}", job.job);
                return;
            }
        };
        let mut matches = Vec::new();
        while let Some(record) = stream.next().await {
            let line = record.seq.saturating_sub(1);
            if line > to_line {
                break;
            }
            if let Body::Out(text) | Body::Stderr(text) = &record.body
                && pattern.is_match(&plain_line(&text.raw))
            {
                matches.push(line);
            }
        }
        let _ = inner2.transact(|core| core.job_rescanned(job, generation, &matches));
    });
}

/// Recovery: rebuilds every running job's progress from its transcript and reattaches its
/// supervisor; a job whose supervisor is gone ends there (exited, or lost).
pub(crate) async fn recover_jobs(inner: &Arc<Inner>) {
    let running: Vec<JobRef> = inner.with_core(|core| {
        core.sessions
            .values()
            .flat_map(|s| {
                s.data
                    .jobs
                    .values()
                    .filter(|j| j.running())
                    .map(|j| JobRef {
                        session: s.data.id,
                        job: j.id,
                    })
            })
            .collect()
    });
    let recoveries = running.into_iter().map(|job| recover_job(inner, job));
    futures::future::join_all(recoveries).await;
}

async fn recover_job(inner: &Arc<Inner>, job: JobRef) {
    if inner.with_core(|core| core.job_tracked(job)).is_none() {
        return;
    }
    catch_up(inner, job).await;
    let conn = match inner.job_host.connect(job).await {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!("job {}: connecting to its supervisor: {e}", job.job);
            None
        }
    };
    follow(inner, job, conn);
}
