//! Jobs on hosts: `job run` goes to the workspace's host once per call id, the host's job event
//! log drives the jobs' state, the user's commands go to the host too, `job.logs` reads the log
//! there, and closing a session kills its jobs.

use controlroom_drivers::fake::FakeScript;
use controlroom_protocol::agent::{AgentJobParams, JobRunParams, JobStatusParams};
use controlroom_protocol::agentd::{
    FollowFrom, JobFollowParams, JobListParams, JobLogsParams, JobOpParams,
};
use controlroom_protocol::ids::{JobId, OpId, RemotePath, SessionId};
use controlroom_protocol::session::{Job, JobStatus};
use controlroom_protocol::timeline::ItemBody;
use controlroom_protocol::{ErrorData, Resource};

use super::Harness;

pub(super) fn run(command: &str) -> JobRunParams {
    JobRunParams {
        call_id: OpId::generate().to_string(),
        command: command.into(),
        machine: None,
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

pub(super) fn agent_op(job: JobId) -> AgentJobParams {
    AgentJobParams {
        call_id: OpId::generate().to_string(),
        job_id: job,
    }
}

pub(super) fn follow(
    id: SessionId,
    job: JobId,
    pattern: Option<&str>,
    buffer_seconds: u32,
) -> JobFollowParams {
    JobFollowParams {
        op_id: OpId::generate(),
        id,
        job_id: job,
        buffer_seconds: Some(buffer_seconds),
        pattern: pattern.map(str::to_owned),
        from: FollowFrom::Unseen,
    }
}

/// Starts a job called `name` on the session's primary workspace.
pub(super) async fn start(h: &Harness, id: SessionId, name: &str) -> JobId {
    h.rt.agent_job_run(
        id,
        JobRunParams {
            name: Some(name.into()),
            ..run(&format!("./{name}.sh"))
        },
    )
    .await
    .unwrap()
    .job_id
}

/// The job as the runtime knows it now.
pub(super) fn job_now(h: &Harness, id: SessionId, job: JobId) -> Option<Job> {
    h.rt.inner().with_core(|core| {
        core.sessions
            .get(&id)
            .and_then(|s| s.data.jobs.get(&job))
            .map(crate::jobs::JobRecord::to_job)
    })
}

/// Waits until the job satisfies `pred`.
pub(super) async fn wait_job(
    h: &mut Harness,
    id: SessionId,
    job: JobId,
    what: &str,
    pred: impl Fn(&Job) -> bool,
) -> Job {
    let rt = h.rt.clone();
    let check = move || {
        rt.inner().with_core(|core| {
            core.sessions
                .get(&id)
                .and_then(|s| s.data.jobs.get(&job))
                .map(crate::jobs::JobRecord::to_job)
        })
    };
    h.wait(id, what, |_| check().is_some_and(|j| pred(&j)))
        .await;
    job_now(h, id, job).unwrap()
}

#[tokio::test]
async fn a_job_runs_on_its_host_and_follows_its_event_log() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let started =
        h.rt.agent_job_run(
            id,
            JobRunParams {
                name: Some("build".into()),
                ..run("make all")
            },
        )
        .await
        .unwrap();
    let job = started.job_id;
    assert_eq!(h.machines.job_ids(devbox), [job]);
    assert_eq!(
        started.log_path,
        RemotePath::new(format!("/home/alice/src/app/.controlroom/jobs/{job}.log"))
    );
    let view = job_now(&h, id, job).unwrap();
    assert_eq!(view.machine_id, Some(devbox));
    assert_eq!(view.status, JobStatus::Running);
    assert_eq!(h.summary(id).live_work.jobs, 1);

    h.machines.job_output(devbox, job, &["one", "two", "three"]);
    wait_job(&mut h, id, job, "the lines", |j| j.lines == 3).await;
    let logs =
        h.rt.job_logs(JobLogsParams {
            id,
            job_id: job,
            from_line: Some(2),
            max_lines: Some(5),
        })
        .await
        .unwrap();
    assert_eq!((logs.from_line, logs.total_lines), (1, 3));
    assert_eq!(logs.lines, ["one", "two", "three"]);

    h.machines.job_exit(devbox, job, 0);
    let ended = wait_job(&mut h, id, job, "the exit", |j| {
        j.status == JobStatus::Succeeded
    })
    .await;
    assert_eq!((ended.exit, ended.lines), (Some(0), 3));
    assert_eq!(h.summary(id).live_work.jobs, 0);
    let listed = h.rt.job_list(JobListParams { id }).await.unwrap().jobs;
    assert_eq!(listed, std::slice::from_ref(&ended));
    let item = h.timeline(id).await.into_iter().find_map(|i| match i.body {
        ItemBody::Job { job } => Some(job),
        _ => None,
    });
    assert_eq!(item, Some(ended));
}

#[tokio::test]
async fn bad_runs_are_refused_and_replays_start_one_job() {
    let h0 = Harness::new(FakeScript::default()).await;
    let mut h = h0;
    let id = h.create().await;
    for (params, kind) in [
        (run("  "), "invalid_params"),
        (
            JobRunParams {
                timeout_s: Some(0),
                ..run("x")
            },
            "invalid_params",
        ),
        (
            JobRunParams {
                machine: Some("nowhere".into()),
                ..run("x")
            },
            "not_found",
        ),
    ] {
        let err = h.rt.agent_job_run(id, params).await.unwrap_err();
        let got = match err.data {
            Some(ErrorData::InvalidParams) => "invalid_params",
            Some(ErrorData::NotFound {
                resource: Resource::Workspace,
                ..
            }) => "not_found",
            other => panic!("{other:?}"),
        };
        assert_eq!(got, kind);
    }
    let params = run("./once.sh");
    let first = h.rt.agent_job_run(id, params.clone()).await.unwrap();
    let again = h.rt.agent_job_run(id, params).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(h.machines.job_ids(h.devbox).len(), 1);
    let err =
        h.rt.agent_job_status(
            id,
            JobStatusParams {
                job_id: Some(JobId::generate()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err.data,
        Some(ErrorData::NotFound {
            resource: Resource::Job,
            ..
        })
    ));
    h.rt.close(h.op(id)).await.unwrap();
    assert!(h.rt.agent_job_run(id, run("late")).await.is_err());
}

#[tokio::test]
async fn user_commands_go_to_the_host_and_closing_kills_jobs() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let a = start(&h, id, "a").await;
    let b = start(&h, id, "b").await;
    let followed =
        h.rt.job_follow(follow(id, a, Some("ERR"), 5))
            .await
            .unwrap();
    let spec = followed.follow.unwrap();
    assert_eq!(
        (spec.pattern.as_deref(), spec.buffer_seconds),
        (Some("ERR"), 5)
    );
    let unfollowed = h.rt.job_unfollow(op(id, a)).await.unwrap();
    assert!(unfollowed.follow.is_none());
    let killed = h.rt.agent_job_kill(id, agent_op(b)).await.unwrap();
    assert_eq!(killed.id, b);
    wait_job(&mut h, id, b, "b killed", |j| j.status == JobStatus::Killed).await;

    h.rt.close(h.op(id)).await.unwrap();
    wait_job(&mut h, id, a, "a killed", |j| j.status == JobStatus::Killed).await;
    let methods = h.machines.methods(devbox);
    assert!(methods.iter().filter(|m| *m == "tool.run").count() >= 6);
}
