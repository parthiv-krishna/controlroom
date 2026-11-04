//! Job wakes from the hosts' job event logs (design §3.6): exit wakes (idle → a run, busy → a
//! steer), follow wakes after their buffer, merged wakes, kills, a lost job, and exactly once
//! across a link outage, a resubscribe and an agent daemon restart. While a host is away its
//! jobs keep counting as live work and one notice per outage follows the grace period.

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::AgentTodoAddParams;
use controlroom_protocol::ids::{OpId, SessionId};
use controlroom_protocol::session::{AutoContinueStatus, JobStatus, SessionState};
use controlroom_protocol::timeline::{InjectOrigin, RunTrigger};

use super::host::steered_text;
use super::jobs::{agent_op, follow, op, start, wait_job};
use super::{Harness, caps};

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
    h.machines
        .job_output(h.devbox, job, &["secret log content", "more", "done"]);
    h.machines.job_exit(h.devbox, job, 0);

    let wakes = wait_wakes(&mut h, id, 1).await;
    let log = format!("devbox:/home/alice/src/app/.controlroom/jobs/{job}.log");
    assert!(
        wakes[0].contains(&format!(
            "job {job} `build` exited with code 0 after 1s; 3 lines in {log}."
        )),
        "{wakes:?}"
    );
    assert!(
        !wakes[0].contains("secret log content"),
        "logs never reach the context"
    );
    assert_eq!(
        h.run_triggers(id).await,
        [job_trigger("job: build (exit 0)")]
    );
}

#[tokio::test]
async fn an_exit_wake_while_busy_is_steered_in() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    let job = start(&h, id, "tests").await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    h.machines.job_exit(h.devbox, job, 2);
    h.wait_command(id, "the steer", |c| {
        steered_text(c).is_some_and(|text| text.contains(WAKE))
    })
    .await;
    h.host.release(id, "work");
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(h.host.prompts(id), ["work on it"]);
}

#[tokio::test]
async fn a_follow_wakes_after_its_buffer_and_merges_with_other_jobs() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let e2e = start(&h, id, "e2e").await;
    let lint = start(&h, id, "lint").await;
    h.rt.job_follow(follow(id, e2e, Some("FAIL"), 10))
        .await
        .unwrap();
    h.rt.job_follow(follow(id, lint, None, 60)).await.unwrap();
    h.machines
        .job_output(devbox, e2e, &["ok", "FAIL a", "FAIL b"]);
    h.machines.job_output(devbox, lint, &["warning 1"]);
    wait_job(&mut h, id, e2e, "the matches", |j| {
        j.follow.as_ref().is_some_and(|f| f.next_wake_at.is_some())
    })
    .await;
    // Not before the buffer has passed (host time of the first match + 10 s).
    h.rt.tick();
    assert!(wake_prompts(&h, id).is_empty());
    h.clock.advance(10_000);
    h.rt.tick();
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(
        wakes[0].contains("2 lines matching /FAIL/ at lines 2-3"),
        "{wakes:?}"
    );
    assert!(
        wakes[0].contains("1 line new at line 1"),
        "lint's lines ride along: {wakes:?}"
    );
    assert_eq!(
        h.run_triggers(id).await,
        [job_trigger("jobs: e2e (follow), lint (follow)")]
    );
    // Later lines: the exit carries them, and the follow ends with it.
    h.machines.job_output(devbox, e2e, &["FAIL c"]);
    h.machines.job_exit(devbox, e2e, 1);
    let wakes = wait_wakes(&mut h, id, 2).await;
    assert!(
        wakes[1].contains("exited with code 1") && wakes[1].contains("not reported yet: line 4"),
        "{wakes:?}"
    );
}

#[tokio::test]
async fn kills_wake_unless_the_agent_killed_the_job_and_a_lost_job_says_so() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let mine = start(&h, id, "mine").await;
    let theirs = start(&h, id, "server").await;
    let rebooted = start(&h, id, "soak").await;
    h.rt.agent_job_kill(id, agent_op(mine)).await.unwrap();
    wait_job(&mut h, id, mine, "killed", |j| {
        j.status == JobStatus::Killed
    })
    .await;
    h.rt.job_kill(op(id, theirs)).await.unwrap();
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(
        wakes[0].contains("`server` was killed by the user"),
        "{wakes:?}"
    );
    assert!(!wakes.iter().any(|w| w.contains("`mine`")));
    h.machines.job_lost(h.devbox, rebooted);
    let wakes = wait_wakes(&mut h, id, 2).await;
    assert!(
        wakes[1].contains("`soak` was lost (host rebooted)"),
        "{wakes:?}"
    );
    assert_eq!(job_now_status(&h, id, rebooted), Some(JobStatus::Lost));
}

fn job_now_status(
    h: &Harness,
    id: SessionId,
    job: controlroom_protocol::ids::JobId,
) -> Option<JobStatus> {
    super::jobs::job_now(h, id, job).map(|j| j.status)
}

#[tokio::test]
async fn events_during_an_outage_wake_once_after_the_reconnect() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let job = start(&h, id, "build").await;
    h.rt.job_follow(follow(id, job, Some("ERROR"), 2))
        .await
        .unwrap();
    h.machines.drop_link(devbox);
    // The host keeps logging while the link is down.
    h.machines.job_output(devbox, job, &["ERROR one", "fine"]);
    h.clock.advance(5_000);
    h.machines.job_output(devbox, job, &["ERROR two"]);
    h.machines.job_exit(devbox, job, 0);
    h.rt.tick();
    assert!(wake_prompts(&h, id).is_empty(), "nothing is invented");
    assert_eq!(h.summary(id).live_work.jobs, 1);

    h.machines.reconnect(devbox);
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert_eq!(wakes.len(), 1, "one merged wake: {wakes:?}");
    assert!(
        wakes[0].contains("exited with code 0") && wakes[0].contains("lines 1, 3"),
        "{wakes:?}"
    );
    // A second connection resubscribes from the cursor: nothing is delivered again.
    h.machines.drop_link(devbox);
    h.machines.reconnect(devbox);
    h.machines
        .wait("the resubscribe", |m| {
            m.methods(devbox)
                .iter()
                .filter(|m| *m == "jobs.subscribe")
                .count()
                >= 3
        })
        .await;
    h.clock.advance(60_000);
    h.rt.tick();
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(wake_prompts(&h, id).len(), 1);
    // The consumed revision is acknowledged.
    h.machines
        .wait("the jobs ack", |m| m.acked_rev(devbox).is_some())
        .await;
}

#[tokio::test]
async fn a_job_that_exits_while_the_daemon_is_down_wakes_the_agent_once() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "migrate").await;
    h.machines.job_output(h.devbox, job, &["step 1"]);
    let machines = h.machines.clone();
    let devbox = h.devbox;
    let mut h = h
        .restart_after(move |_| machines.job_exit(devbox, job, 0))
        .await;
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(
        wakes[0].contains("`migrate` exited with code 0"),
        "{wakes:?}"
    );
    // Another restart doesn't deliver it again.
    let mut h = h.restart().await;
    h.wait_state(id, SessionState::Idle).await;
    h.machines
        .wait("the resubscribe", |m| {
            m.methods(devbox)
                .iter()
                .filter(|m| *m == "jobs.subscribe")
                .count()
                >= 3
        })
        .await;
    h.clock.advance(60_000);
    h.rt.tick();
    assert_eq!(wake_prompts(&h, id).len(), 1);
    assert_eq!(h.run_triggers(id).await.len(), 1);
}

#[tokio::test]
async fn an_unreachable_host_is_noticed_once_per_outage() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    start(&h, id, "soak").await;
    h.machines.drop_link(devbox);
    h.clock.advance(600_000);
    h.rt.tick();
    h.rt.tick();
    let s = h.summary(id);
    assert_eq!(s.live_work.jobs, 1);
    let injections: Vec<_> = h.rt.inner().with_core(|core| {
        core.sessions[&id]
            .data
            .injections
            .iter()
            .map(|i| (i.origin, i.wake, i.content.clone()))
            .collect()
    });
    assert_eq!(injections.len(), 1, "{injections:?}");
    assert_eq!(injections[0].0, InjectOrigin::Workspace);
    assert!(!injections[0].1, "a notice doesn't wake the agent");
    // The next run carries it.
    h.prompt(id, "status?").await;
    h.wait_state(id, SessionState::Idle).await;
    assert!(
        h.host
            .prompts(id)
            .iter()
            .any(|p| p.contains("devbox has been unreachable for 10 minutes"))
    );
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
    let s = h.wait_state(id, SessionState::Idle).await;
    assert_eq!(s.live_work.jobs, 1);
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(detail.status, AutoContinueStatus::On);
    assert_eq!(detail.reason.as_deref(), Some("waiting on job suite"));
    // While its host is away the reason says so.
    h.machines.drop_link(h.devbox);
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(
        detail.reason.as_deref(),
        Some("waiting on job suite (devbox unreachable)")
    );
    h.machines.reconnect(h.devbox);
    h.machines.job_exit(h.devbox, job, 0);
    let wakes = wait_wakes(&mut h, id, 1).await;
    assert!(wakes[0].contains("`suite` exited with code 0"), "{wakes:?}");
    assert_eq!(
        h.run_triggers(id).await[1],
        job_trigger("job: suite (exit 0)")
    );
}
