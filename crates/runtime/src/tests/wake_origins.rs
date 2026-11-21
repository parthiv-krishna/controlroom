//! Every job wake's message names its job (`origin: inject job` with the job as its source), on
//! every path a wake takes into the agent: a waking prompt while idle, a steer into a run, a
//! steer racing the end of the run, a follow wake, a wake that waits behind a queued prompt, a
//! wake that starts a stopped agent, a wake produced while the agent daemon was down, and a
//! re-fold. A wake merged from several jobs names them in its label (no single source).

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ids::{JobId, SessionId};
use controlroom_protocol::session::{ContentPart, RunOutcome, SessionState};
use controlroom_protocol::timeline::{InjectOrigin, ItemBody, MessageOrigin, MessageRole};

use super::jobs::{follow, start};
use super::{Harness, caps};

/// The start of every wake's text.
const WAKE: &str = "[controlroom] Job";

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

/// The origin a single job's wake carries.
fn from_job(job: JobId, label: &str) -> Option<MessageOrigin> {
    Some(MessageOrigin::Inject {
        origin: InjectOrigin::Job,
        label: label.into(),
        source: Some(job.to_string()),
    })
}

/// The origins of the session's wake messages (user messages starting with [`WAKE`], merged
/// ones included), in timeline order.
async fn wake_origins(h: &Harness, id: SessionId) -> Vec<Option<MessageOrigin>> {
    h.timeline(id)
        .await
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::Message {
                role: MessageRole::User,
                content,
                origin,
            } => is_wake(&content).then_some(origin),
            _ => None,
        })
        .collect()
}

fn is_wake(content: &[ContentPart]) -> bool {
    matches!(content.first(), Some(ContentPart::Text { text }) if text.starts_with(WAKE))
}

/// Waits until the agent got `n` wakes (as prompts or steers) and is idle again.
async fn wait_wakes(h: &mut Harness, id: SessionId, n: usize) {
    h.host
        .wait("the wakes", |host| {
            let mut sent = host.prompts(id);
            sent.extend(host.steers(id));
            sent.iter().filter(|t| t.starts_with(WAKE)).count() >= n
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn an_exit_wake_while_idle_names_its_job() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    h.machines.job_exit(h.devbox, job, 1);
    wait_wakes(&mut h, id, 1).await;
    assert_eq!(
        wake_origins(&h, id).await,
        [from_job(job, "job: build (exit 1)")]
    );
}

#[tokio::test]
async fn an_exit_wake_steered_into_a_run_names_its_job() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    h.machines.job_exit(h.devbox, job, 1);
    h.wait_command(id, "the steer", |c| {
        super::host::steered_text(c).is_some_and(|t| t.starts_with(WAKE))
    })
    .await;
    h.host.release(id, "work");
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(
        wake_origins(&h, id).await,
        [from_job(job, "job: build (exit 1)")]
    );
}

/// The run ends before the agent reads the steered wake: the wake starts the next run.
#[tokio::test]
async fn an_exit_wake_racing_the_end_of_the_run_names_its_job() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    h.host.hold_writes(id, true);
    h.machines.job_exit(h.devbox, job, 1);
    h.host
        .wait("the held wake", |host| host.held_writes(id) == 1)
        .await;
    h.host.release(id, "work");
    h.wait(id, "the run's end", |s| {
        s.last_outcome == Some(RunOutcome::Completed)
    })
    .await;
    h.host.release_writes(id);
    h.wait(id, "the wake's run", |s| {
        s.preview == "ok" && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(
        wake_origins(&h, id).await,
        [from_job(job, "job: build (exit 1)")]
    );
}

/// A followed job's lines wake the agent, then its exit (with lines no wake reported yet).
#[tokio::test]
async fn follow_wakes_and_the_exit_after_them_name_their_job() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let job = start(&h, id, "validate").await;
    h.rt.job_follow(follow(id, job, Some("FAIL"), 5))
        .await
        .unwrap();
    h.machines.job_output(devbox, job, &["ok", "FAIL one"]);
    super::jobs::wait_job(&mut h, id, job, "the match", |j| {
        j.follow.as_ref().is_some_and(|f| f.next_wake_at.is_some())
    })
    .await;
    h.clock.advance(5_000);
    h.rt.tick();
    wait_wakes(&mut h, id, 1).await;
    h.machines.job_output(devbox, job, &["FAIL two"]);
    h.machines.job_exit(devbox, job, 1);
    wait_wakes(&mut h, id, 2).await;
    assert_eq!(
        wake_origins(&h, id).await,
        [
            from_job(job, "job: validate (follow)"),
            from_job(job, "job: validate (exit 1)")
        ]
    );
}

/// Two jobs ending together make one wake: it names both in its label and has no single source.
#[tokio::test]
async fn a_merged_wake_names_its_jobs_in_its_label() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let e2e = start(&h, id, "e2e").await;
    let lint = start(&h, id, "lint").await;
    h.rt.job_follow(follow(id, e2e, None, 5)).await.unwrap();
    h.rt.job_follow(follow(id, lint, None, 5)).await.unwrap();
    h.machines.job_output(devbox, e2e, &["one"]);
    h.machines.job_output(devbox, lint, &["two"]);
    super::jobs::wait_job(&mut h, id, lint, "the lines", |j| {
        j.follow.as_ref().is_some_and(|f| f.next_wake_at.is_some())
    })
    .await;
    h.clock.advance(5_000);
    h.rt.tick();
    wait_wakes(&mut h, id, 1).await;
    assert_eq!(
        wake_origins(&h, id).await,
        [Some(MessageOrigin::Inject {
            origin: InjectOrigin::Job,
            label: "jobs: e2e (follow), lint (follow)".into(),
            source: None,
        })]
    );
}

/// A wake while a prompt is on its way waits for that run and is steered into it.
#[tokio::test]
async fn a_wake_behind_a_prompt_on_its_way_names_its_job() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    h.host.hold_writes(id, true);
    h.prompt(id, "work on it").await;
    h.host
        .wait("the held prompt", |host| host.held_writes(id) == 1)
        .await;
    h.machines.job_exit(h.devbox, job, 0);
    h.host.release_writes(id);
    h.wait_command(id, "the steered wake", |c| {
        super::host::steered_text(c).is_some_and(|t| t.starts_with(WAKE))
    })
    .await;
    h.host.release(id, "work");
    wait_wakes(&mut h, id, 1).await;
    assert_eq!(
        wake_origins(&h, id).await,
        [from_job(job, "job: build (exit 0)")]
    );
}

/// The agent process is gone: the wake starts it again and is its first prompt.
#[tokio::test]
async fn a_wake_that_starts_a_stopped_agent_names_its_job() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    h.host.kill_agent(id);
    h.wait_state(id, SessionState::Dormant).await;
    h.machines.job_exit(h.devbox, job, 2);
    wait_wakes(&mut h, id, 1).await;
    assert_eq!(
        wake_origins(&h, id).await,
        [from_job(job, "job: build (exit 2)")]
    );
}

/// The job exits while the agent daemon is down; after the restart its wake names it, and so
/// does a re-fold of the transcript.
#[tokio::test]
async fn a_wake_after_a_restart_names_its_job() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let job = start(&h, id, "migrate").await;
    let (machines, devbox) = (h.machines.clone(), h.devbox);
    let mut h = h
        .restart_after(move |_| machines.job_exit(devbox, job, 0))
        .await;
    wait_wakes(&mut h, id, 1).await;
    let expected = [from_job(job, "job: migrate (exit 0)")];
    assert_eq!(wake_origins(&h, id).await, expected);
    let db = h.dir.path().join("agent.sqlite");
    let h = h
        .restart_after(|_| {
            let conn = rusqlite::Connection::open(db).unwrap();
            conn.execute_batch("DELETE FROM timeline_items; UPDATE sessions SET timeline_seq = 0;")
                .unwrap();
        })
        .await;
    assert_eq!(wake_origins(&h, id).await, expected);
}
