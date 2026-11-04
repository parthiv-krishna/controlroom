//! The agent's primitives: non-blocking questions (answer, dismiss, withdraw; answered while
//! busy → steer, while idle → a new run), the shared todo list with attribution and the
//! user's edits injected as a notice, notices, and run triggers from the runtime's knowledge.

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::{
    AgentQuestion, AgentQuestionOption, AgentTodoAddParams, AgentTodoUpdateParams, NotifyParams,
    QuestionAskParams, QuestionWithdrawParams,
};
use controlroom_protocol::agentd::{
    IfBusy, RequestRespondParams, SessionPromptParams, TodoAddParams, TodoRemoveParams,
    TodoReorderParams, TodoUpdateParams,
};
use controlroom_protocol::ids::{DeviceId, OpId, RequestId, ScheduleId, SessionId, TodoId};
use controlroom_protocol::session::{
    Attention, Author, DeviceRef, PromptOrigin, QuestionAnswer, RequestKind, RequestState,
    Response, SessionState, TodoStatus,
};
use controlroom_protocol::timeline::{InjectOrigin, ItemBody, NoticeLevel, RunTrigger};
use controlroom_protocol::{ErrorData, Seq};

use super::{Harness, caps, text};

fn steering() -> Capabilities {
    Capabilities {
        steer: true,
        ..caps()
    }
}

fn laptop() -> DeviceRef {
    DeviceRef {
        device_id: DeviceId::new("laptop-1"),
        label: "Laptop".into(),
    }
}

/// A script whose "work" turn holds at gate `work` until the test releases it.
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

async fn ask(h: &Harness, id: SessionId, call: &str, question: &str) -> RequestId {
    h.rt.agent_ask(
        id,
        QuestionAskParams {
            call_id: call.into(),
            title: None,
            questions: vec![AgentQuestion {
                text: question.into(),
                options: vec![
                    AgentQuestionOption {
                        label: "Postgres".into(),
                        description: None,
                    },
                    AgentQuestionOption {
                        label: "SQLite".into(),
                        description: Some("embedded".into()),
                    },
                ],
                multi: false,
                free_text: true,
            }],
        },
    )
    .await
    .unwrap()
    .question_id
}

fn answer(id: SessionId, request: &RequestId, choice: &str) -> RequestRespondParams {
    RequestRespondParams {
        op_id: OpId::generate(),
        id,
        request_id: request.clone(),
        response: Response::Answers {
            answers: vec![QuestionAnswer {
                question_id: "q1".into(),
                selected: vec![choice.into()],
                text: None,
            }],
        },
        then: None,
        by: Some(laptop()),
    }
}

fn prompts_containing(h: &Harness, id: SessionId, needle: &str) -> usize {
    h.host
        .prompts(id)
        .iter()
        .filter(|p| p.contains(needle))
        .count()
}

fn steers(h: &Harness, id: SessionId) -> Vec<String> {
    h.host
        .commands(id)
        .into_iter()
        .filter_map(|c| match c {
            Command::Steer { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_question_never_blocks_and_its_answer_starts_a_run_when_idle() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;

    let result =
        h.rt.agent_ask(
            id,
            QuestionAskParams {
                call_id: "call-1".into(),
                title: None,
                questions: vec![AgentQuestion {
                    text: "Which database?".into(),
                    options: Vec::new(),
                    multi: false,
                    free_text: false,
                }],
            },
        )
        .await
        .unwrap();
    assert!(result.reminder.starts_with("Question sent;"));
    // The lighter tier: attention `question`, the session stays idle.
    let s = h.wait(id, "question", |s| !s.requests.is_empty()).await;
    assert_eq!(s.state, SessionState::Idle);
    assert_eq!(s.attention, Some(Attention::Question));
    assert_eq!(s.preview, "Which database?");
    let request = &s.requests[0];
    assert_eq!(request.kind, RequestKind::Question);
    assert!(!request.blocking);
    // A free-text question even when the agent said otherwise: it offered no options.
    assert!(request.questions[0].free_text);
    // The ask is idempotent by the tool call id.
    let again =
        h.rt.agent_ask(
            id,
            QuestionAskParams {
                call_id: "call-1".into(),
                title: None,
                questions: vec![AgentQuestion {
                    text: "Which database?".into(),
                    options: Vec::new(),
                    multi: false,
                    free_text: false,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(again.question_id, result.question_id);
    assert_eq!(h.summary(id).requests.len(), 1);

    // The answer is recorded with the device and injected as a new run.
    let mut params = answer(id, &result.question_id, "");
    params.response = Response::Answers {
        answers: vec![QuestionAnswer {
            question_id: "q1".into(),
            selected: Vec::new(),
            text: Some("Postgres 16".into()),
        }],
    };
    let answered = h.rt.respond(params).await.unwrap();
    assert_eq!(answered.request.state, RequestState::Answered);
    assert_eq!(answered.request.answer.unwrap().by, Some(laptop()));
    let s = h
        .wait(id, "the answer's run", |s| {
            s.state == SessionState::Idle && s.last_turn_end_seq > Seq(0)
        })
        .await;
    assert!(s.requests.is_empty());
    assert_eq!(s.attention, Some(Attention::Unread));
    let prompts = h.host.prompts(id);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("answered your question"), "{prompts:?}");
    assert!(prompts[0].contains("→ Postgres 16"), "{prompts:?}");
    assert_eq!(
        h.run_triggers(id).await,
        [RunTrigger::Inject {
            origin: InjectOrigin::Answer,
            label: "answer: Which database?".into()
        }]
    );
    // The request stays in the timeline, answered.
    let item = h
        .timeline(id)
        .await
        .into_iter()
        .find_map(|i| match i.body {
            ItemBody::Request { request } => Some(request),
            _ => None,
        })
        .unwrap();
    assert_eq!(item.state, RequestState::Answered);
}

#[tokio::test]
async fn an_answer_while_the_agent_works_is_steered_in() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    let question = ask(&h, id, "c1", "Which database?").await;
    let s = h.summary(id);
    assert_eq!(s.state, SessionState::Working);
    assert_eq!(s.attention, Some(Attention::Question));

    h.rt.respond(answer(id, &question, "SQLite")).await.unwrap();
    h.wait_command(id, "the steer", |c| matches!(c, Command::Steer { .. }))
        .await;
    let steered = steers(&h, id);
    assert!(steered[0].contains("→ SQLite"), "{steered:?}");
    h.host.release(id, "work");
    h.wait_state(id, SessionState::Idle).await;
    // No extra run: the answer joined the one in progress.
    assert_eq!(h.host.prompts(id), ["work on it"]);
    assert_eq!(h.run_triggers(id).await.len(), 1);
}

#[tokio::test]
async fn without_steering_an_answer_waits_for_the_run_to_end() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    let question = ask(&h, id, "c1", "Which database?").await;
    h.rt.respond(answer(id, &question, "Postgres"))
        .await
        .unwrap();
    assert_eq!(prompts_containing(&h, id, "answered"), 0);
    h.host.release(id, "work");
    h.wait(id, "the answer's run", |_| true).await;
    h.host
        .wait("the answer", |host| {
            host.prompts(id).iter().any(|p| p.contains("→ Postgres"))
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(h.run_triggers(id).await.len(), 2);
}

#[tokio::test]
async fn dismissing_a_question_tells_the_agent_it_was_declined() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let question = ask(&h, id, "c1", "Deploy now?").await;
    let mut dismiss = answer(id, &question, "");
    dismiss.response = Response::Dismiss;
    let result = h.rt.respond(dismiss).await.unwrap();
    assert_eq!(result.request.answer.unwrap().label, "Dismissed");
    h.host
        .wait("the declined notice", |host| {
            host.prompts(id).iter().any(|p| p.contains("declined"))
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn the_agent_can_withdraw_a_question_but_not_an_answer() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let open = ask(&h, id, "c1", "Which database?").await;
    let withdraw = |call: &str, question: &RequestId| QuestionWithdrawParams {
        call_id: call.into(),
        question_id: question.clone(),
    };
    let result =
        h.rt.agent_withdraw(id, withdraw("c2", &open))
            .await
            .unwrap();
    assert_eq!(result.state, RequestState::Withdrawn);
    assert!(h.summary(id).requests.is_empty());
    let err = h.rt.respond(answer(id, &open, "SQLite")).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::AlreadyResolved { .. })));

    let answered = ask(&h, id, "c3", "Which region?").await;
    h.rt.respond(answer(id, &answered, "Postgres"))
        .await
        .unwrap();
    let result =
        h.rt.agent_withdraw(id, withdraw("c4", &answered))
            .await
            .unwrap();
    assert_eq!(result.state, RequestState::Answered);
    assert_eq!(result.answer.unwrap().label, "Postgres");
    let err =
        h.rt.agent_withdraw(id, withdraw("c5", &RequestId::new("nope")))
            .await
            .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::NotFound { .. })));
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn questions_stay_open_across_a_restart_and_closing_withdraws_them() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let question = ask(&h, id, "c1", "Which database?").await;
    let mut h = h.restart().await;
    let s = h.wait_state(id, SessionState::Idle).await;
    assert_eq!(s.requests.len(), 1);
    assert_eq!(s.requests[0].id, question);

    h.rt.close(h.op(id)).await.unwrap();
    let s = h.wait_state(id, SessionState::Closed).await;
    assert!(s.requests.is_empty());
    let err =
        h.rt.respond(answer(id, &question, "SQLite"))
            .await
            .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::AlreadyResolved { .. })));
}

#[tokio::test]
async fn todos_are_shared_attributed_and_rolled_up() {
    let mut h = Harness::with(gated(), steering()).await;
    let id = h.create().await;

    let list =
        h.rt.agent_todo_add(
            id,
            AgentTodoAddParams {
                call_id: "t1".into(),
                texts: vec!["write tests".into(), "ship".into(), "demo".into()],
            },
        )
        .await
        .unwrap()
        .todos;
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|t| t.updated_by == Author::Agent));
    let [tests, ship, demo] = [list[0].id, list[1].id, list[2].id];
    h.rt.agent_todo_update(
        id,
        AgentTodoUpdateParams {
            call_id: "t2".into(),
            todo_id: tests,
            text: None,
            status: Some(TodoStatus::Done),
            reason: None,
        },
    )
    .await
    .unwrap();

    // The user's edits carry the device.
    let user = |op| TodoUpdateParams {
        op_id: OpId::new(op),
        id,
        todo_id: demo,
        text: None,
        status: Some(TodoStatus::Skipped),
        reason: Some("no time".into()),
        by: Some(laptop()),
    };
    let skipped = h.rt.todo_update(user("u1")).await.unwrap();
    assert_eq!(
        skipped.updated_by,
        Author::User {
            device: Some(laptop())
        }
    );
    assert_eq!(skipped.reason.as_deref(), Some("no time"));
    let added =
        h.rt.todo_add(TodoAddParams {
            op_id: OpId::generate(),
            id,
            texts: vec!["announce".into()],
            index: Some(0),
            by: Some(laptop()),
        })
        .await
        .unwrap()
        .todos;
    h.rt.todo_reorder(TodoReorderParams {
        op_id: OpId::generate(),
        id,
        todo_id: added[0].id,
        to_index: 9,
        by: Some(laptop()),
    })
    .await
    .unwrap();

    // Roll-up: skipped entries don't count.
    let s = h.wait(id, "roll-up", |s| s.todo.is_some()).await;
    let todo = s.todo.unwrap();
    assert_eq!((todo.done, todo.total), (1, 3));
    let detail = h.detail(id).await;
    let texts: Vec<&str> = detail.todos.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["write tests", "ship", "demo", "announce"]);

    // The user's edits wait for the next run (no wake), then arrive as one notice.
    assert!(h.host.prompts(id).is_empty());
    h.prompt(id, "work on it").await;
    h.wait_command(id, "the todo notice", |c| {
        matches!(c, Command::Steer { .. })
    })
    .await;
    let notice = steers(&h, id).remove(0);
    assert!(notice.starts_with("[controlroom] The user changed your todo list:"));
    assert!(notice.contains("marked \"demo\" skipped"), "{notice}");
    assert!(notice.contains("added \"announce\""), "{notice}");
    assert!(notice.contains("moved \"announce\" to #4"), "{notice}");
    // The agent's own edits are never echoed back to it.
    assert!(!notice.contains("write tests"), "{notice}");
    h.host.release(id, "work");
    h.wait_state(id, SessionState::Idle).await;

    // The timeline has a todo snapshot per turn.
    let snapshots: Vec<usize> = h
        .timeline(id)
        .await
        .into_iter()
        .filter_map(|i| match i.body {
            ItemBody::Todo { items } => Some(items.len()),
            _ => None,
        })
        .collect();
    assert_eq!(snapshots, [4]);
    h.rt.todo_remove(TodoRemoveParams {
        op_id: OpId::generate(),
        id,
        todo_id: ship,
        by: None,
    })
    .await
    .unwrap();
    let snapshots = h
        .timeline(id)
        .await
        .into_iter()
        .filter(|i| matches!(i.body, ItemBody::Todo { .. }))
        .count();
    assert_eq!(snapshots, 2);

    // Unknown entries and empty texts are refused.
    let err =
        h.rt.todo_remove(TodoRemoveParams {
            op_id: OpId::generate(),
            id,
            todo_id: TodoId::generate(),
            by: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::NotFound { .. })));
    let err =
        h.rt.agent_todo_add(
            id,
            AgentTodoAddParams {
                call_id: "t3".into(),
                texts: vec!["  ".into()],
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::InvalidParams)));
}

#[tokio::test]
async fn the_agent_state_follows_todos_and_questions() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let mut state = h.rt.agent_state(id).await.unwrap();
    assert!(state.borrow().as_ref().unwrap().todos.is_empty());
    h.rt.agent_todo_add(
        id,
        AgentTodoAddParams {
            call_id: "t1".into(),
            texts: vec!["a".into()],
        },
    )
    .await
    .unwrap();
    state.changed().await.unwrap();
    assert_eq!(state.borrow_and_update().as_ref().unwrap().todos.len(), 1);
    ask(&h, id, "q", "Which?").await;
    state.changed().await.unwrap();
    assert_eq!(
        state.borrow_and_update().as_ref().unwrap().questions.len(),
        1
    );
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn notices_appear_in_the_timeline() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    h.rt.agent_notify(
        id,
        NotifyParams {
            call_id: "n1".into(),
            text: "Deployed to staging".into(),
            level: NoticeLevel::Warning,
        },
    )
    .await
    .unwrap();
    let notices: Vec<(NoticeLevel, String)> = h
        .timeline(id)
        .await
        .into_iter()
        .filter_map(|i| match i.body {
            ItemBody::Notice { level, text } => Some((level, text)),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices,
        [(NoticeLevel::Warning, "Deployed to staging".to_owned())]
    );
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn run_triggers_name_the_device_queue_and_schedule() {
    let mut h = Harness::new(gated()).await;
    let id = h.create().await;
    let send = |prompt: &str, origin: PromptOrigin| SessionPromptParams {
        op_id: OpId::generate(),
        id,
        content: text(prompt),
        origin,
        if_busy: IfBusy::Queue,
    };
    let user = PromptOrigin::User {
        device: Some(laptop()),
    };
    let schedule = PromptOrigin::Schedule {
        schedule_id: ScheduleId::generate(),
        name: "Inbox triage".into(),
    };
    h.rt.prompt(send("work first", user.clone())).await.unwrap();
    h.wait(id, "working", |s| s.preview == "working").await;
    h.rt.prompt(send("then this", user)).await.unwrap();
    h.rt.prompt(send("check the inbox", schedule.clone()))
        .await
        .unwrap();
    h.host.release(id, "work");
    h.wait(id, "the queue", |s| {
        s.queue.len == 0 && s.state == SessionState::Idle
    })
    .await;
    let PromptOrigin::Schedule { schedule_id, name } = schedule else {
        unreachable!()
    };
    let expected = [
        RunTrigger::User {
            device: Some(laptop()),
        },
        RunTrigger::Queue {
            device: Some(laptop()),
        },
        RunTrigger::Schedule { schedule_id, name },
    ];
    assert_eq!(h.run_triggers(id).await, expected);
    // A restart re-folds the transcript: the triggers stay.
    let h = h.restart().await;
    assert_eq!(h.run_triggers(id).await, expected);
}

#[tokio::test]
async fn an_answer_resumes_an_agent_that_was_idle_stopped_meanwhile() {
    let mut h = Harness::new(FakeScript::default()).await;
    h.rt.set_settings(controlroom_protocol::agentd::SettingsSetParams {
        op_id: OpId::generate(),
        idle_timeout_h: Some(1),
        retention: None,
    })
    .await
    .unwrap();
    let id = h.create().await;
    let question = ask(&h, id, "c1", "Which database?").await;
    // The open question doesn't keep the agent up.
    h.clock.advance(2 * 3600 * 1000);
    h.rt.tick();
    let s = h.wait_state(id, SessionState::Dormant).await;
    assert_eq!(s.attention, Some(Attention::Question));

    h.rt.respond(answer(id, &question, "Postgres"))
        .await
        .unwrap();
    h.host
        .wait("the answer after the resume", |host| {
            host.prompts(id).iter().any(|p| p.contains("→ Postgres"))
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
    assert!(h.summary(id).requests.is_empty());
}
