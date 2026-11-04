//! Jobs and the job event log across client disconnects and daemon restarts.

use controlroom_protocol::Rev;
use controlroom_protocol::host::{
    HostOp, HostResult, JobEvent, JobEventKind, JobFollowSpec, JobsAckParams, JobsSubscribeParams,
    KilledBy, methods,
};
use controlroom_protocol::ids::JobId;
use controlroom_protocol::session::{Job, JobStatus, SandboxLevel};

use crate::harness::{HostEnv, RpcClient, Session, fifo, release, wait_until};

fn job_run(command: &str, follow: Option<&str>) -> HostOp {
    HostOp::JobRun {
        command: command.into(),
        cwd: Some("sub".into()),
        name: Some("suite".into()),
        timeout_s: None,
        follow: follow.map(|p| JobFollowSpec {
            pattern: Some(p.into()),
            buffer_seconds: 2,
        }),
    }
}

async fn run_job(session: &Session, client: &RpcClient, call: &str, op: HostOp) -> Job {
    match session.run(client, call, op).await.unwrap() {
        HostResult::JobRun { job } => *job,
        other => panic!("{other:?}"),
    }
}

/// Events from a subscription until (and including) `job`'s exit.
async fn events_until_exit(
    client: &RpcClient,
    mut events: Vec<JobEvent>,
    job: JobId,
) -> Vec<JobEvent> {
    while !events
        .iter()
        .any(|e| e.job_id == job && matches!(e.kind, JobEventKind::Exit { .. }))
    {
        let note = client.note("jobs.event").await;
        let more: Vec<JobEvent> = serde_json::from_value(note["events"].clone()).unwrap();
        events.extend(more);
    }
    events
}

fn contiguous(events: &[JobEvent]) {
    for pair in events.windows(2) {
        assert_eq!(pair[1].rev.0, pair[0].rev.0 + 1, "{events:#?}");
    }
}

#[tokio::test]
async fn a_job_logs_its_events_and_a_resubscribe_gets_exactly_the_missed_ones() {
    let env = HostEnv::start().await;
    let first = env.controller().await;
    let session = Session::open(&env, &first, SandboxLevel::Off).await;
    let subscribed = first
        .call::<methods::JobsSubscribe>(JobsSubscribeParams { since: None })
        .await
        .unwrap();
    assert!(subscribed.events.is_empty());
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let ran = env.dir.path().join("ran.txt");
    let command = format!(
        "echo x >> {}; echo one; echo ERROR two; cat {} > /dev/null; echo three; echo ERROR four; exit 3",
        ran.display(),
        gate.display()
    );
    let job = run_job(&session, &first, "j1", job_run(&command, Some("^ERROR"))).await;
    assert_eq!(job.status, JobStatus::Running);
    assert!(
        job.log_path
            .as_str()
            .ends_with(&format!("/project/.controlroom/jobs/{}.log", job.id))
    );
    // A resend returns the same job and starts nothing.
    let again = run_job(&session, &first, "j1", job_run(&command, Some("^ERROR"))).await;
    assert_eq!(again.id, job.id);

    // Live events up to the first two lines.
    let mut seen: Vec<JobEvent> = Vec::new();
    while !seen
        .iter()
        .any(|e| matches!(e.kind, JobEventKind::Progress { lines: 2, .. }))
    {
        let note = first.note("jobs.event").await;
        seen.extend(serde_json::from_value::<Vec<JobEvent>>(note["events"].clone()).unwrap());
    }
    assert!(matches!(seen[0].kind, JobEventKind::Started { .. }));
    let cursor = seen.last().unwrap().rev;
    drop(first);

    // While nobody listens the job finishes.
    release(&gate).await;
    let log = std::path::PathBuf::from(job.log_path.as_str());
    wait_until("the job's log is complete", || {
        std::fs::read_to_string(&log).is_ok_and(|l| l.contains("ERROR four"))
    })
    .await;

    let second = env.controller().await;
    let backlog = second
        .call::<methods::JobsSubscribe>(JobsSubscribeParams {
            since: Some(cursor),
        })
        .await
        .unwrap();
    let events = events_until_exit(&second, backlog.events, job.id).await;
    assert_eq!(events[0].rev.0, cursor.0 + 1, "no gap after the cursor");
    contiguous(&events);
    let progress: Vec<(u64, Vec<u64>)> = seen
        .iter()
        .chain(&events)
        .filter_map(|e| match &e.kind {
            JobEventKind::Progress { lines, matches, .. } => Some((*lines, matches.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress.last().unwrap().0,
        4,
        "{:?}",
        std::fs::read_to_string(&log)
    );
    let matched: Vec<u64> = progress.iter().flat_map(|(_, m)| m.clone()).collect();
    assert_eq!(matched, [2, 4]);
    let exits: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.kind, JobEventKind::Exit { .. }))
        .collect();
    assert_eq!(exits.len(), 1);
    let JobEventKind::Exit {
        exit_code,
        lines,
        lost,
        ..
    } = &exits[0].kind
    else {
        unreachable!()
    };
    assert_eq!((*exit_code, *lines, lost.clone()), (Some(3), 4, None));
    assert_eq!(std::fs::read_to_string(&ran).unwrap().lines().count(), 1);

    let HostResult::JobStatus { jobs } = session
        .run(&second, "s1", HostOp::JobStatus { job_id: None })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(jobs[0].status, JobStatus::Failed);
    assert_eq!(jobs[0].exit, Some(3));

    // After an ack the ended job's events are pruned.
    second
        .call::<methods::JobsAck>(JobsAckParams {
            rev: events.last().unwrap().rev,
        })
        .await
        .unwrap();
    let after = second
        .call::<methods::JobsSubscribe>(JobsSubscribeParams { since: None })
        .await
        .unwrap();
    assert!(after.events.is_empty(), "{:?}", after.events);
    assert_eq!(after.rev, events.last().unwrap().rev);
}

#[tokio::test]
async fn a_job_survives_a_daemon_restart_and_ends_once() {
    let mut env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let command = format!("echo one; cat {} > /dev/null; echo two", gate.display());
    let job = run_job(&session, &client, "j1", job_run(&command, None)).await;
    let log = std::path::PathBuf::from(job.log_path.as_str());
    wait_until("the first line", || {
        std::fs::read_to_string(&log).is_ok_and(|l| l.contains("one"))
    })
    .await;
    drop(client);
    env.kill_daemon();
    // The job ends while the daemon is down.
    release(&gate).await;
    wait_until("the job's log is complete", || {
        std::fs::read_to_string(&log).is_ok_and(|l| l.contains("two"))
    })
    .await;
    env.start_daemon().await;
    let client = env.controller().await;
    let backlog = client
        .call::<methods::JobsSubscribe>(JobsSubscribeParams { since: None })
        .await
        .unwrap();
    let events = events_until_exit(&client, backlog.events, job.id).await;
    contiguous(&events);
    let exits: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JobEventKind::Exit {
                exit_code, lines, ..
            } => Some((*exit_code, *lines)),
            _ => None,
        })
        .collect();
    assert_eq!(exits, [(Some(0), 2)]);
}

#[tokio::test]
async fn a_job_whose_supervisor_died_with_the_host_is_lost_and_kills_are_logged() {
    let mut env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let subscribed = client
        .call::<methods::JobsSubscribe>(JobsSubscribeParams { since: None })
        .await
        .unwrap();
    let job = run_job(&session, &client, "j1", job_run("sleep 1000", None)).await;
    let killed = run_job(
        &session,
        &client,
        "j2",
        job_run("echo hi; sleep 1000", None),
    )
    .await;
    let result = session
        .run(
            &client,
            "k1",
            HostOp::JobKill {
                job_id: killed.id,
                by: KilledBy::Agent,
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, HostResult::JobKill { .. }));
    let before = events_until_exit(&client, subscribed.events, killed.id).await;
    assert!(before.iter().any(|e| e.job_id == killed.id
        && matches!(
            e.kind,
            JobEventKind::Killed {
                by: KilledBy::Agent
            }
        )));
    drop(client);
    env.kill_daemon();
    // The "reboot" takes the first job's supervisor and command with it.
    crate::harness::kill_everything_under(&env.state());
    crate::harness::kill_everything_under(&env.project());
    env.start_daemon().await;
    let client = env.controller().await;
    let backlog = client
        .call::<methods::JobsSubscribe>(JobsSubscribeParams {
            since: Some(Rev(0)),
        })
        .await
        .unwrap();
    let events = events_until_exit(&client, backlog.events, job.id).await;
    let lost = events.iter().find_map(|e| match &e.kind {
        JobEventKind::Exit { lost, .. } if e.job_id == job.id => Some(lost.clone()),
        _ => None,
    });
    assert!(lost.flatten().is_some_and(|l| l.contains("supervisor")));
    let HostResult::JobStatus { jobs } = session
        .run(
            &client,
            "s1",
            HostOp::JobStatus {
                job_id: Some(killed.id),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(jobs[0].status, JobStatus::Killed);
}

#[tokio::test]
async fn following_a_running_job_rescans_its_lines_so_far() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let subscribed = client
        .call::<methods::JobsSubscribe>(JobsSubscribeParams { since: None })
        .await
        .unwrap();
    let gate = env.dir.path().join("gate");
    fifo(&gate);
    let command = format!(
        "echo WARN a; echo ok; echo WARN b; cat {} > /dev/null; echo WARN c",
        gate.display()
    );
    let job = run_job(&session, &client, "j1", job_run(&command, None)).await;
    let mut seen = subscribed.events;
    while !seen
        .iter()
        .any(|e| matches!(e.kind, JobEventKind::Progress { lines: 3, .. }))
    {
        let note = client.note("jobs.event").await;
        seen.extend(serde_json::from_value::<Vec<JobEvent>>(note["events"].clone()).unwrap());
    }
    let followed = session
        .run(
            &client,
            "f1",
            HostOp::JobFollow {
                job_id: job.id,
                pattern: Some("^WARN".into()),
                buffer_seconds: Some(3),
                from: Default::default(),
            },
        )
        .await
        .unwrap();
    let HostResult::JobFollow { job: after } = followed else {
        panic!()
    };
    assert_eq!(after.follow.as_ref().map(|f| f.buffer_seconds), Some(3));
    release(&gate).await;
    let events = events_until_exit(&client, seen, job.id).await;
    let follow = events.iter().find_map(|e| match &e.kind {
        JobEventKind::Follow { matches, .. } => Some(matches.clone()),
        _ => None,
    });
    assert_eq!(follow, Some(vec![1, 3]));
    let later: Vec<u64> = events
        .iter()
        .skip_while(|e| !matches!(e.kind, JobEventKind::Follow { .. }))
        .filter_map(|e| match &e.kind {
            JobEventKind::Progress { matches, .. } => Some(matches.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(later, [4]);
}
