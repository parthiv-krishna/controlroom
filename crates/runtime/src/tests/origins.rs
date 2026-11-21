//! Message origins: every user-role line the runtime writes names where it came from on its
//! message item (a prompt, the queue, a schedule, a steered send-now, an injection with its
//! source, auto-continue), steered injections inside a user's run included, and a re-fold from
//! scratch yields the same origins (or none, for lines from before origins existed).

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::{AgentTodoAddParams, AgentTodoUpdateParams};
use controlroom_protocol::agentd::{
    IfBusy, PromptResult, QueueSendNowParams, SendNowResult, SessionPromptParams, TodoUpdateParams,
};
use controlroom_protocol::ids::{OpId, ScheduleId, SessionId};
use controlroom_protocol::session::{
    ContentPart, PromptOrigin, RunOutcome, SessionState, TodoStatus,
};
use controlroom_protocol::timeline::{
    InjectOrigin, ItemBody, MessageOrigin, MessageRole, RunTrigger,
};

use super::agent::{answer, ask, laptop};
use super::jobs::start;
use super::{Harness, caps, text};

fn steering() -> Capabilities {
    Capabilities {
        steer: true,
        ..caps()
    }
}

/// "busy" runs hold at gate `busy`, auto-continue runs at gate `cont`; everything else answers
/// "ok".
fn gated() -> FakeScript {
    FakeScript::default()
        .turn(
            "busy",
            [FakeStep::Text("busy".into()), FakeStep::Gate("busy".into())],
        )
        .turn(
            "Auto-continue",
            [
                FakeStep::Text("continuing".into()),
                FakeStep::Gate("cont".into()),
            ],
        )
        .default_turn([FakeStep::Text("ok".into())])
}

/// The session's user messages, in timeline order: the text and its origin.
async fn origins(h: &Harness, id: SessionId) -> Vec<(String, Option<MessageOrigin>)> {
    h.timeline(id)
        .await
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::Message {
                role: MessageRole::User,
                content,
                origin,
            } => Some((plain(&content), origin)),
            _ => None,
        })
        .collect()
}

/// A message's text parts, joined.
fn plain(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The origin of the one user message containing `needle`.
async fn origin_of(h: &Harness, id: SessionId, needle: &str) -> Option<MessageOrigin> {
    let found: Vec<_> = origins(h, id)
        .await
        .into_iter()
        .filter(|(text, _)| text.contains(needle))
        .collect();
    assert_eq!(found.len(), 1, "one message with {needle:?}: {found:?}");
    found.into_iter().next().unwrap().1
}

async fn send(h: &Harness, id: SessionId, prompt: &str, origin: PromptOrigin) -> PromptResult {
    h.rt.prompt(SessionPromptParams {
        op_id: OpId::generate(),
        id,
        content: text(prompt),
        origin,
        if_busy: IfBusy::Queue,
    })
    .await
    .unwrap()
}

fn from_laptop() -> PromptOrigin {
    PromptOrigin::User {
        device: Some(laptop()),
    }
}

fn schedule() -> (ScheduleId, PromptOrigin) {
    let schedule_id = ScheduleId::generate();
    let origin = PromptOrigin::Schedule {
        schedule_id,
        name: "Inbox triage".into(),
    };
    (schedule_id, origin)
}

fn send_now(id: SessionId, result: PromptResult) -> QueueSendNowParams {
    let PromptResult::Queued { queue_id } = result else {
        panic!("not queued: {result:?}");
    };
    QueueSendNowParams {
        op_id: OpId::generate(),
        id,
        queue_id,
        interrupt: false,
    }
}

/// Stops the runtime, wipes the materialized timeline (and, with `forget_lines`, the recorded
/// line origins, as in a store from before origins), and opens it again: the timeline is
/// re-folded from the transcript alone.
async fn refold_from_scratch(h: Harness, forget_lines: bool) -> Harness {
    let db = h.dir.path().join("agent.sqlite");
    h.restart_after(|_| {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch("DELETE FROM timeline_items; UPDATE sessions SET timeline_seq = 0;")
            .unwrap();
        if forget_lines {
            conn.execute("UPDATE sessions SET run_triggers = '[]'", [])
                .unwrap();
        }
    })
    .await
}

/// A user's prompt, a queued one, a schedule's, a send-now steered into the run and a job wake
/// steered into it: each message names its origin, and the steered ones stay apart from the
/// user's run they land in.
#[tokio::test]
async fn prompts_queue_schedules_steers_and_wakes_name_their_origin() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    let job = start(&h, id, "build").await;
    let (schedule_id, scheduled) = schedule();

    assert_eq!(
        send(&h, id, "keep busy", from_laptop()).await,
        PromptResult::Sent
    );
    h.wait(id, "busy", |s| s.preview == "busy").await;
    send(&h, id, "then this", from_laptop()).await;
    send(&h, id, "check the inbox", scheduled).await;
    let now = send(&h, id, "change of plan", from_laptop()).await;
    assert_eq!(
        h.rt.send_now(send_now(id, now)).await.unwrap(),
        SendNowResult::Steered
    );
    h.machines.job_exit(h.devbox, job, 0);
    h.wait_command(id, "the job wake", |c| {
        super::host::steered_text(c).is_some_and(|t| t.contains("[controlroom] Job"))
    })
    .await;
    h.host.release(id, "busy");
    h.wait(id, "the queue", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle
    })
    .await;

    let laptop = Some(laptop());
    assert_eq!(
        origin_of(&h, id, "keep busy").await,
        Some(MessageOrigin::User {
            device: laptop.clone()
        })
    );
    assert_eq!(
        origin_of(&h, id, "change of plan").await,
        Some(MessageOrigin::Steer {
            device: laptop.clone()
        })
    );
    assert_eq!(
        origin_of(&h, id, "[controlroom] Job").await,
        Some(MessageOrigin::Inject {
            origin: InjectOrigin::Job,
            label: "job: build (exit 0)".into(),
            source: Some(job.to_string()),
        })
    );
    assert_eq!(
        origin_of(&h, id, "then this").await,
        Some(MessageOrigin::Queue {
            device: laptop.clone()
        })
    );
    assert_eq!(
        origin_of(&h, id, "check the inbox").await,
        Some(MessageOrigin::Schedule {
            schedule_id,
            name: "Inbox triage".into(),
        })
    );
    // The steered lines didn't start runs: the user's run holds them.
    assert_eq!(
        h.run_triggers(id).await,
        [
            RunTrigger::User {
                device: laptop.clone()
            },
            RunTrigger::Queue {
                device: laptop.clone()
            },
            RunTrigger::Schedule {
                schedule_id,
                name: "Inbox triage".into()
            },
        ]
    );

    // A re-fold from the transcript alone gives the same origins; one without the recorded
    // lines (a store from before origins) gives none.
    let before = origins(&h, id).await;
    let h = refold_from_scratch(h, false).await;
    assert_eq!(origins(&h, id).await, before);
    let h = refold_from_scratch(h, true).await;
    let after = origins(&h, id).await;
    assert_eq!(after.len(), before.len());
    assert!(after.iter().all(|(_, o)| o.is_none()), "{after:?}");
}

/// A send-now steered into a run that ends before the agent reads it starts the next run: its
/// message, like the run, names the steer and its device.
#[tokio::test]
async fn a_send_now_racing_the_end_of_the_run_names_its_steer_and_device() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    send(&h, id, "keep busy", from_laptop()).await;
    h.wait(id, "busy", |s| s.preview == "busy").await;
    let late = send(&h, id, "change of plan", from_laptop()).await;
    h.host.hold_writes(id, true);
    assert_eq!(
        h.rt.send_now(send_now(id, late)).await.unwrap(),
        SendNowResult::Steered
    );
    h.host
        .wait("the held steer", |host| host.held_writes(id) == 1)
        .await;
    h.host.release(id, "busy");
    h.wait(id, "the run's end", |s| {
        s.last_outcome == Some(RunOutcome::Completed)
    })
    .await;
    h.host.release_writes(id);
    h.wait(id, "the steer's run", |s| {
        s.preview == "ok" && s.state == SessionState::Idle
    })
    .await;
    let steer = MessageOrigin::Steer {
        device: Some(laptop()),
    };
    assert_eq!(origin_of(&h, id, "change of plan").await, Some(steer));
    assert_eq!(
        h.run_triggers(id).await[1],
        RunTrigger::Steer {
            device: Some(laptop())
        }
    );
}

/// An answer to a question, the user's todo edits and an auto-continue nudge each name their
/// origin (the answer its request).
#[tokio::test]
async fn answers_todo_edits_and_auto_continue_name_their_origin() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;

    let question = ask(&h, id, "c1", "Which database?").await;
    h.rt.respond(answer(id, &question, "Postgres"))
        .await
        .unwrap();
    h.wait(id, "the answer's run", |s| {
        s.state == SessionState::Idle && s.requests.is_empty() && s.last_outcome.is_some()
    })
    .await;

    send(&h, id, "keep busy", PromptOrigin::default()).await;
    h.wait(id, "busy", |s| s.preview == "busy").await;
    let todos =
        h.rt.agent_todo_add(
            id,
            AgentTodoAddParams {
                call_id: "t1".into(),
                texts: vec!["write tests".into()],
            },
        )
        .await
        .unwrap()
        .todos;
    h.rt.todo_update(TodoUpdateParams {
        op_id: OpId::generate(),
        id,
        todo_id: todos[0].id,
        text: Some("write more tests".into()),
        status: None,
        reason: None,
        by: Some(laptop()),
    })
    .await
    .unwrap();
    h.wait_command(id, "the todo notice", |c| {
        super::host::steered_text(c).is_some_and(|t| t.contains("write more tests"))
    })
    .await;
    h.host.release(id, "busy");
    h.wait(id, "the continue run", |s| s.preview == "continuing")
        .await;
    h.rt.agent_todo_update(
        id,
        AgentTodoUpdateParams {
            call_id: "t2".into(),
            todo_id: todos[0].id,
            text: None,
            status: Some(TodoStatus::Done),
            reason: None,
        },
    )
    .await
    .unwrap();
    h.host.release(id, "cont");
    h.wait_state(id, SessionState::Idle).await;

    assert_eq!(
        origin_of(&h, id, "answered your question").await,
        Some(MessageOrigin::Inject {
            origin: InjectOrigin::Answer,
            label: "answer: Which database?".into(),
            source: Some(question.to_string()),
        })
    );
    assert_eq!(
        origin_of(&h, id, "keep busy").await,
        Some(MessageOrigin::User { device: None })
    );
    assert_eq!(
        origin_of(&h, id, "changed your todo list").await,
        Some(MessageOrigin::Inject {
            origin: InjectOrigin::Todo,
            label: "todo edits".into(),
            source: None,
        })
    );
    assert_eq!(
        origin_of(&h, id, "Auto-continue").await,
        Some(MessageOrigin::AutoContinue)
    );
}
