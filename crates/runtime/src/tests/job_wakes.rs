//! Job wakes through injection: exit wakes (idle → a new run, busy → a steer), follow wakes
//! with a pattern after the buffer, merged wakes, kills and timeouts, exactly once across a
//! daemon restart, and auto-continue waiting for a job whose exit wake then continues the work
//! (the P5 done-when "a job-triggered continuation works").

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::AgentTodoAddParams;
use controlroom_protocol::daemon::FollowFrom;
use controlroom_protocol::ids::{JobId, OpId, SessionId};
use controlroom_protocol::session::{AutoContinueStatus, JobStatus, SessionState};
use controlroom_protocol::timeline::{InjectOrigin, RunTrigger};
use controlroom_transcript::StopCause;

use super::jobs::{agent_op, ended, follow, op, run, settle};
use super::{Harness, caps};
use controlroom_protocol::agent::JobRunParams;

const WAKE: &str = "Background job update";

fn steering() -> Capabilities {
    Capabilities {
        steer: true,
        ..caps()
    }
}

/// "work" runs hold at gate `work`; everything else answers "ok".
fn gated() -> FakeScript {
    FakeScript::default()
        .turn(
            "work",
            [
                FakeStep::Text("working".into()),
                FakeStep::Gate("work".into()),
            ],
        )
        .default_turn([FakeStep::Text("ok".into())])
}

async fn start(h: &Harness, id: SessionId, name: &str) -> JobId {
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

/// The wake prompts the agent got, in order.
fn wake_prompts(h: &Harness, id: SessionId) -> Vec<String> {
    h.host
        .prompts(id)
        .into_iter()
        .filter(|p| p.contains(WAKE))
        .collect()
}

/// Waits until the agent got `n` wake prompts and is idle again.
async fn wait_wakes(h: &mut Harness, id: SessionId, n: usize) -> Vec<String> {
    h.host
        .wait("a wake prompt", |host| {
            host.prompts(id).iter().filter(|p| p.contains(WAKE)).count() >= n
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
    wake_prompts(h, id)
}

fn job_trigger(label: &str) -> RunTrigger {
    RunTrigger::Inject {
        origin: InjectOrigin::Job,
        label: label.into(),
    }
}

#[tokio::test]
async fn an_exit_wake_while_idle_starts_a_run_with_metadata_only() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    h.jobs.output(job, &["secret log content", "more", "done"]);
    h.clock.advance(65_000);
    h.jobs.exit(job, 0);

    let wakes = wait_wakes(&mut h, id, 1).await;
    let log = format!("/home/alice/src/app/.controlroom/jobs/{job}.log");
    assert!(
        wakes[0].contains(&format!(
            "job {job} `build` exited with code 0 after 1m 05s; 3 lines in {log}."
        )),
        "{wakes:?}"
    );
    assert!(
        !wakes[0].contains("secret log content"),
        "logs never reach the context"
    );
    assert!(wakes[0].contains("tail -n 50"), "{wakes:?}");
    assert_eq!(
        h.run_triggers(id).await,
        [job_trigger("job: build (exit 0)")]
    );
    assert_eq!(h.summary(id).live_work.jobs, 0);
}

#[tokio::test]
async fn an_exit_wake_while_busy_is_steered_in() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    let job = start(&h, id, "tests").await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    h.jobs.exit(job, 2);
    h.wait_command(
        id,
        "the steer",
        |c| matches!(c, Command::Steer { text } if text.contains(WAKE)),
    )
    .await;
    h.host.release(id, "work");
    h.wait_state(id, SessionState::Idle).await;
    // The wake joined the run in progress: no run of its own.
    assert_eq!(h.host.prompts(id), ["work on it"]);
    assert_eq!(h.run_triggers(id).await.len(), 1);
}

#[tokio::test]
async fn a_pattern_follow_wakes_after_its_buffer_and_ends_with_the_exit() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "e2e").await;
    h.rt.job_follow(follow(id, job, Some("FAIL"), FollowFrom::Unseen))
        .await
        .unwrap();
    h.jobs
        .output(job, &["ok 1", "FAIL login", "ok 2", "FAIL logout"]);
    settle(&h, job).await;
    h.clock.advance(9_000);
    h.rt.tick();
    assert!(wake_prompts(&h, id).is_empty(), "the buffer hasn't elapsed");
    h.clock.advance(1_000);
    h.rt.tick();
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(
        wakes[0].contains("is still running: 2 lines matching /FAIL/ at lines 2, 4 of"),
        "{wakes:?}"
    );
    assert_eq!(h.run_triggers(id).await, [job_trigger("job: e2e (follow)")]);

    // Lines that don't match don't wake; reported lines are not reported again.
    h.jobs.output(job, &["ok 3"]);
    settle(&h, job).await;
    h.clock.advance(60_000);
    h.rt.tick();
    h.jobs.output(job, &["FAIL again"]);
    h.jobs.exit(job, 1);
    let wakes = wait_wakes(&mut h, id, 2).await;
    assert!(wakes[1].contains("exited with code 1"), "{wakes:?}");
    assert!(
        wakes[1].contains("Followed lines not reported yet: line 6."),
        "{wakes:?}"
    );
    assert_eq!(ended(&h, id, job).await.follow, None);
}

#[tokio::test]
async fn wakes_due_together_are_merged() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let lint = start(&h, id, "lint").await;
    let types = start(&h, id, "typecheck").await;
    for job in [lint, types] {
        h.rt.job_follow(follow(id, job, None, FollowFrom::Unseen))
            .await
            .unwrap();
    }
    h.jobs.output(lint, &["warning: unused"]);
    h.jobs.output(types, &["error TS2322", "error TS2345"]);
    settle(&h, lint).await;
    settle(&h, types).await;
    h.clock.advance(10_000);
    h.rt.tick();
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(wakes[0].contains(&format!("job {lint} `lint` is still running: 1 line new")));
    assert!(wakes[0].contains(&format!(
        "job {types} `typecheck` is still running: 2 lines new"
    )));
    assert_eq!(
        h.run_triggers(id).await,
        [job_trigger("jobs: lint (follow), typecheck (follow)")]
    );

    // An exit takes the other job's buffered lines along at once.
    h.jobs.output(types, &["error TS1005"]);
    settle(&h, types).await;
    h.jobs.exit(lint, 0);
    let wakes = wait_wakes(&mut h, id, 2).await;
    assert!(wakes[1].contains("`lint` exited with code 0"), "{wakes:?}");
    assert!(
        wakes[1].contains("`typecheck` is still running: 1 line new at line 3"),
        "{wakes:?}"
    );
    // Nothing is left for the timer.
    h.clock.advance(60_000);
    h.rt.tick();
    assert_eq!(wake_prompts(&h, id).len(), 2);
}

#[tokio::test]
async fn kills_and_timeouts_wake_unless_the_agent_killed_the_job() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let own = start(&h, id, "watch").await;
    h.rt.agent_job_kill(id, agent_op(own)).await.unwrap();
    assert_eq!(ended(&h, id, own).await.status, JobStatus::Killed);

    let users = start(&h, id, "server").await;
    h.rt.job_kill(op(id, users)).await.unwrap();
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(
        wakes[0].contains("`server` was killed by the user"),
        "{wakes:?}"
    );

    let slow = start(&h, id, "bench").await;
    h.jobs.stop(slow, None, Some(15), Some(StopCause::Timeout));
    let wakes = wait_wakes(&mut h, id, 2).await;
    assert!(
        wakes[1].contains("`bench` timed out and was stopped"),
        "{wakes:?}"
    );
    assert_eq!(
        h.run_triggers(id).await,
        [
            job_trigger("job: server (killed)"),
            job_trigger("job: bench (timed out)")
        ]
    );
}

#[tokio::test]
async fn a_job_that_exits_while_the_daemon_is_down_wakes_the_agent_once() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "migrate").await;
    h.jobs.output(job, &["step 1"]);
    let jobs = h.jobs.clone();
    let mut h = h.restart_after(move |_| jobs.exit(job, 0)).await;
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(
        wakes[0].contains("`migrate` exited with code 0"),
        "{wakes:?}"
    );
    // Another restart doesn't deliver it again.
    let mut h = h.restart().await;
    h.wait_state(id, SessionState::Idle).await;
    h.clock.advance(60_000);
    h.rt.tick();
    assert_eq!(wake_prompts(&h, id).len(), 1);
    assert_eq!(h.run_triggers(id).await.len(), 1);
}

#[tokio::test]
async fn auto_continue_waits_for_a_job_whose_exit_wake_continues_the_work() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    h.rt.agent_todo_add(
        id,
        AgentTodoAddParams {
            call_id: OpId::generate().to_string(),
            texts: vec!["run the suite".into(), "fix failures".into()],
        },
    )
    .await
    .unwrap();
    let job = start(&h, id, "suite").await;
    h.host.release(id, "work");

    // The turn ended with open items, but the running job will wake the agent instead.
    let s = h.wait_state(id, SessionState::Idle).await;
    assert_eq!(s.live_work.jobs, 1);
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(detail.status, AutoContinueStatus::On);
    assert_eq!(detail.reason.as_deref(), Some("waiting on job suite"));
    assert_eq!(h.host.prompts(id), ["work on it"]);

    // The job's exit wake starts the next run.
    h.jobs.exit(job, 0);
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(wakes[0].contains("`suite` exited with code 0"), "{wakes:?}");
    let triggers = h.run_triggers(id).await;
    assert_eq!(
        triggers[1],
        job_trigger("job: suite (exit 0)"),
        "{triggers:?}"
    );
}
