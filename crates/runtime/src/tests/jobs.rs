//! Background jobs: run, logs, follow, kill, timeout, lost supervisors, restarts, live work.

use controlroom_drivers::fake::FakeScript;
use controlroom_protocol::daemon::{
    FollowFrom, JobFollowParams, JobListParams, JobLogsParams, JobOpParams, SettingsSetParams,
};
use controlroom_protocol::ids::{JobId, OpId, RemotePath, Seq, SessionId};
use controlroom_protocol::session::{Job, JobStatus, SandboxLevel, SessionState};
use controlroom_protocol::timeline::ItemBody;
use controlroom_protocol::{ErrorData, Resource};
use controlroom_transcript::StopCause;

use super::{Harness, TIMEOUT};
use crate::ports::Clock;
use crate::{JobRun, JobStarted, KilledBy};

const HOUR_MS: i64 = 3600 * 1000;

pub(super) fn run(command: &str) -> JobRun {
    JobRun {
        op_id: OpId::generate(),
        command: command.into(),
        cwd: None,
        timeout_s: None,
        name: None,
    }
}

pub(super) fn op(id: SessionId, job: JobId) -> JobOpParams {
    JobOpParams {
        op_id: OpId::generate(),
        id,
        job_id: job,
    }
}

pub(super) fn follow(
    id: SessionId,
    job: JobId,
    pattern: Option<&str>,
    from: FollowFrom,
) -> JobFollowParams {
    JobFollowParams {
        op_id: OpId::generate(),
        id,
        job_id: job,
        buffer_seconds: Some(10),
        pattern: pattern.map(str::to_owned),
        from,
    }
}

pub(super) async fn jobs(h: &Harness, id: SessionId) -> Vec<Job> {
    h.rt.job_list(JobListParams { id }).await.unwrap().jobs
}

pub(super) async fn job(h: &Harness, id: SessionId, job: JobId) -> Job {
    jobs(h, id).await.into_iter().find(|j| j.id == job).unwrap()
}

/// Waits until the job satisfies `pred`; wakes on the session's feed.
pub(super) async fn wait_job(
    h: &Harness,
    id: SessionId,
    job_id: JobId,
    pred: impl Fn(&Job) -> bool,
) -> Job {
    let mut updates =
        h.rt.subscribe_session(id, Some(Seq(u64::MAX)), None)
            .await
            .unwrap()
            .updates;
    let wait = async {
        loop {
            let current = job(h, id, job_id).await;
            if pred(&current) {
                return current;
            }
            let _ = updates.recv().await;
        }
    };
    match tokio::time::timeout(TIMEOUT, wait).await {
        Ok(job) => job,
        Err(_) => panic!(
            "timed out waiting for job {job_id}: {:#?}",
            job(h, id, job_id).await
        ),
    }
}

/// Waits until the runtime has folded every record the job wrote, then ticks (which publishes
/// the line count). The runtime's folding has no event of its own, so this yields until it
/// caught up, bounded by the timeout.
pub(super) async fn settle(h: &Harness, job_id: JobId) {
    let target = h.jobs.last_seq(job_id);
    let caught_up = || {
        h.rt.inner().with_core(|core| {
            core.jobs
                .get(&job_id)
                .is_none_or(|j| j.progress.seq >= target)
        })
    };
    let wait = async {
        while !caught_up() {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(TIMEOUT, wait)
        .await
        .expect("the runtime folds the job's records");
    h.rt.tick();
}

pub(super) async fn ended(h: &Harness, id: SessionId, job_id: JobId) -> Job {
    wait_job(h, id, job_id, |j| j.ended_at.is_some()).await
}

/// The job's timeline item, from a full resubscribe.
async fn job_item(h: &Harness, id: SessionId, job_id: JobId) -> Job {
    let backlog =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap()
            .backlog;
    backlog
        .into_iter()
        .find_map(|item| match item.body {
            ItemBody::Job { job } if job.id == job_id => Some(job),
            _ => None,
        })
        .expect("a job item")
}

#[tokio::test]
async fn a_job_runs_writes_its_log_and_exits() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let JobStarted { job_id, log_path } =
        h.rt.job_run(
            id,
            JobRun {
                cwd: Some(RemotePath::new("web")),
                timeout_s: Some(600),
                name: Some("tests".into()),
                ..run("npm test")
            },
        )
        .await
        .unwrap();
    assert_eq!(
        log_path.as_str(),
        format!("/home/alice/src/app/.controlroom/jobs/{job_id}.log")
    );
    let launch = h.jobs.launches().pop().unwrap();
    assert_eq!(launch.cwd.as_str(), "/home/alice/src/app/web");
    assert_eq!(launch.session_cwd.as_str(), "/home/alice/src/app");
    assert_eq!(launch.timeout_ms, Some(600_000));
    assert_eq!(launch.sandbox_level, SandboxLevel::Strict);
    assert_eq!(h.summary(id).live_work.jobs, 1);
    let running = job(&h, id, job_id).await;
    assert_eq!(
        (running.name.as_str(), running.status, running.timeout_s),
        ("tests", JobStatus::Running, Some(600))
    );

    h.jobs
        .output(job_id, &["\u{1b}[32mok\u{1b}[0m 1", "10%\r100%", "ok 2"]);
    h.clock.advance(1500);
    settle(&h, job_id).await;
    assert_eq!(job(&h, id, job_id).await.lines, 3);
    let logs =
        h.rt.job_logs(JobLogsParams {
            id,
            job_id,
            from_line: Some(2),
            max_lines: None,
        })
        .await
        .unwrap();
    assert_eq!((logs.from_line, logs.total_lines), (2, 3));
    assert_eq!(logs.lines, ["100%", "ok 2"]);
    let tail =
        h.rt.job_logs(JobLogsParams {
            id,
            job_id,
            from_line: None,
            max_lines: Some(1),
        })
        .await
        .unwrap();
    assert_eq!((tail.from_line, tail.lines), (3, vec!["ok 2".to_owned()]));

    h.jobs.exit(job_id, 0);
    let done = ended(&h, id, job_id).await;
    assert_eq!(
        (done.status, done.exit, done.lines),
        (JobStatus::Succeeded, Some(0), 3)
    );
    assert_eq!(done.ended_at.unwrap().0 - done.started_at.0, 1500);
    h.wait(id, "no live work", |s| s.live_work.jobs == 0).await;
    assert_eq!(job_item(&h, id, job_id).await.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn bad_runs_are_refused() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let err = |e: controlroom_protocol::RpcError| e.data.unwrap();
    let outside =
        h.rt.job_run(
            id,
            JobRun {
                cwd: Some(RemotePath::new("../other")),
                ..run("make")
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err(outside), ErrorData::InvalidParams);
    let empty = h.rt.job_run(id, run("  ")).await.unwrap_err();
    assert_eq!(err(empty), ErrorData::InvalidParams);
    let unknown =
        h.rt.job_kill(op(id, JobId::generate()), KilledBy::User)
            .await
            .unwrap_err();
    assert!(matches!(
        unknown.data.unwrap(),
        ErrorData::NotFound {
            resource: Resource::Job,
            ..
        }
    ));

    // A launch that fails ends the job at once and reports the error.
    h.jobs.fail_spec(Some("nono is missing"));
    let failed = h.rt.job_run(id, run("make")).await.unwrap_err();
    assert!(matches!(
        failed.data.unwrap(),
        ErrorData::SandboxUnavailable { .. }
    ));
    let listed = jobs(&h, id).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, JobStatus::Failed);
    assert_eq!(h.summary(id).live_work.jobs, 0);

    // Closed sessions run nothing.
    h.jobs.fail_spec(None);
    h.rt.close(h.op(id)).await.unwrap();
    let closed = h.rt.job_run(id, run("make")).await.unwrap_err();
    assert!(matches!(
        closed.data.unwrap(),
        ErrorData::InvalidState { .. }
    ));
}

#[tokio::test]
async fn a_replayed_run_starts_one_job() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let params = run("make");
    let first = h.rt.job_run(id, params.clone()).await.unwrap();
    let again = h.rt.job_run(id, params).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(h.jobs.launches().len(), 1);
    assert_eq!(jobs(&h, id).await.len(), 1);
}

#[tokio::test]
async fn kill_timeout_and_a_lost_supervisor() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;

    let killed = h.rt.job_run(id, run("sleep 100")).await.unwrap().job_id;
    let job = h.rt.job_kill(op(id, killed), KilledBy::User).await.unwrap();
    assert_eq!(
        job.status,
        JobStatus::Running,
        "the exit record says the rest"
    );
    let job = ended(&h, id, killed).await;
    assert_eq!(
        (job.status, job.signal.as_deref()),
        (JobStatus::Killed, Some("SIGTERM"))
    );

    let slow = h.rt.job_run(id, run("sleep 100")).await.unwrap().job_id;
    h.jobs.stop(slow, None, Some(15), Some(StopCause::Timeout));
    assert_eq!(ended(&h, id, slow).await.status, JobStatus::TimedOut);

    let lost = h.rt.job_run(id, run("sleep 100")).await.unwrap().job_id;
    h.jobs.output(lost, &["one line"]);
    h.jobs.kill_supervisor(lost);
    let job = ended(&h, id, lost).await;
    assert_eq!(
        (job.status, job.exit, job.lines),
        (JobStatus::Failed, None, 1)
    );

    let crashed = h.rt.job_run(id, run("./crash")).await.unwrap().job_id;
    h.jobs.stop(crashed, None, Some(11), None);
    let job = ended(&h, id, crashed).await;
    assert_eq!(
        (job.status, job.signal.as_deref()),
        (JobStatus::Failed, Some("SIGSEGV"))
    );
    h.wait(id, "no live work", |s| s.live_work.jobs == 0).await;
}

#[tokio::test]
async fn following_buffers_matching_lines_until_unfollowed() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let job_id = h.rt.job_run(id, run("e2e")).await.unwrap().job_id;
    h.jobs.output(job_id, &["ok", "FAIL one"]);
    h.clock.advance(1000);
    settle(&h, job_id).await;
    assert_eq!(job(&h, id, job_id).await.lines, 2);

    // `from: unseen` covers the lines printed before following (a rescan finds the match).
    let followed =
        h.rt.job_follow(follow(
            id,
            job_id,
            Some("FAIL|panicked"),
            FollowFrom::Unseen,
        ))
        .await
        .unwrap();
    let follow_info = followed.follow.unwrap();
    assert_eq!(follow_info.pattern.as_deref(), Some("FAIL|panicked"));
    assert_eq!(follow_info.buffer_seconds, 10);
    let now = h.clock.now().0;
    let job = wait_job(&h, id, job_id, |j| {
        j.follow.as_ref().is_some_and(|f| f.next_wake_at.is_some())
    })
    .await;
    assert_eq!(job.follow.unwrap().next_wake_at.unwrap().0, now + 10_000);

    // `from: now` ignores what came before; non-matching lines don't schedule a wake.
    let job =
        h.rt.job_follow(follow(id, job_id, Some("panicked"), FollowFrom::Now))
            .await
            .unwrap();
    assert_eq!(job.follow.unwrap().next_wake_at, None);
    h.jobs.output(job_id, &["FAIL two"]);
    h.jobs.output(job_id, &["thread panicked"]);
    wait_job(&h, id, job_id, |j| {
        j.follow.as_ref().is_some_and(|f| f.next_wake_at.is_some())
    })
    .await;

    let job = h.rt.job_unfollow(op(id, job_id)).await.unwrap();
    assert_eq!(job.follow, None);
    let bad =
        h.rt.job_follow(follow(id, job_id, Some("("), FollowFrom::Now))
            .await
            .unwrap_err();
    assert_eq!(bad.data.unwrap(), ErrorData::InvalidParams);

    // Following ends with the job.
    h.rt.job_follow(follow(id, job_id, None, FollowFrom::Now))
        .await
        .unwrap();
    h.jobs.exit(job_id, 1);
    let job = ended(&h, id, job_id).await;
    assert_eq!(
        (job.status, job.exit, job.follow),
        (JobStatus::Failed, Some(1), None)
    );
    let late =
        h.rt.job_follow(follow(id, job_id, None, FollowFrom::Now))
            .await
            .unwrap();
    assert_eq!(late.follow, None, "an ended job can't be followed");
}

#[tokio::test]
async fn a_daemon_restart_reattaches_a_running_job() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let job_id = h.rt.job_run(id, run("cargo build")).await.unwrap().job_id;
    h.rt.job_follow(follow(id, job_id, Some("error"), FollowFrom::Unseen))
        .await
        .unwrap();
    h.jobs.output(job_id, &["compiling a"]);

    // Output arrives while no daemon runs, and after the restart.
    let mut h = h.restart_after(|_| {}).await;
    h.jobs.output(job_id, &["compiling b"]);
    let s = h
        .wait(id, "the job still runs", |s| s.live_work.jobs == 1)
        .await;
    assert_eq!(s.state, SessionState::Idle);
    h.jobs.output(job_id, &["error: oops"]);
    let job = wait_job(&h, id, job_id, |j| {
        j.follow.as_ref().is_some_and(|f| f.next_wake_at.is_some())
    })
    .await;
    assert_eq!(job.follow.unwrap().pattern.as_deref(), Some("error"));
    h.rt.job_kill(op(id, job_id), KilledBy::Agent)
        .await
        .unwrap();
    let job = ended(&h, id, job_id).await;
    assert_eq!((job.status, job.lines), (JobStatus::Killed, 3));

    // A job that ended while the daemon was down is seen at recovery; one whose supervisor
    // vanished (a reboot) ends as failed.
    let exited = h.rt.job_run(id, run("true")).await.unwrap().job_id;
    let rebooted = h.rt.job_run(id, run("sleep 100")).await.unwrap().job_id;
    let h = h.restart_after(|_| {}).await;
    h.jobs.exit(exited, 0);
    h.jobs.kill_supervisor(rebooted);
    let h = h.restart_after(|_| {}).await;
    assert_eq!(ended(&h, id, exited).await.status, JobStatus::Succeeded);
    assert_eq!(ended(&h, id, rebooted).await.status, JobStatus::Failed);
}

#[tokio::test]
async fn idle_stop_waits_for_running_jobs() {
    let mut h = Harness::new(FakeScript::default()).await;
    h.rt.set_settings(SettingsSetParams {
        op_id: OpId::generate(),
        idle_timeout_h: Some(1),
        retention: None,
    })
    .await
    .unwrap();
    let id = h.create().await;
    let job_id = h.rt.job_run(id, run("make watch")).await.unwrap().job_id;
    h.clock.advance(3 * HOUR_MS);
    h.rt.tick();
    assert!(h.host.supervisor_running(id), "a running job is live work");
    assert_eq!(h.summary(id).state, SessionState::Idle);

    // The exit wakes the agent; idle-stop waits for that run to be old enough too.
    h.jobs.exit(job_id, 0);
    ended(&h, id, job_id).await;
    h.wait(id, "the exit wake's run", |s| {
        s.state == SessionState::Idle && s.last_turn_end_seq > Seq(0)
    })
    .await;
    h.clock.advance(2 * HOUR_MS);
    h.rt.tick();
    h.wait_state(id, SessionState::Dormant).await;
}

#[tokio::test]
async fn closing_or_deleting_a_session_kills_its_jobs() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let job_id = h.rt.job_run(id, run("serve")).await.unwrap().job_id;
    h.rt.close(h.op(id)).await.unwrap();
    assert_eq!(ended(&h, id, job_id).await.status, JobStatus::Killed);

    let other = h.create().await;
    let job_id = h.rt.job_run(other, run("serve")).await.unwrap().job_id;
    h.rt.delete(h.op(other)).await.unwrap();
    h.jobs.wait("the job killed", |j| !j.running(job_id)).await;
}
