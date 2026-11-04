//! Auto-continue (spec §8): every rule and pause reason.

use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::{
    AgentQuestion, AgentTodoAddParams, AgentTodoUpdateParams, QuestionAskParams,
};
use controlroom_protocol::agentd::SessionUpdateParams;
use controlroom_protocol::ids::{OpId, SessionId, TodoId};
use controlroom_protocol::session::{
    Attention, AutoContinueStatus, RunOutcome, SessionState, SessionSummary, TodoStatus,
};
use controlroom_protocol::timeline::RunTrigger;

use super::Harness;

const CONTINUE: &str = "Auto-continue";

/// Continue runs hold at gate `cont`; "work" runs at gate `work`; "fail" runs fail.
fn script() -> FakeScript {
    FakeScript::default()
        .turn(
            CONTINUE,
            [
                FakeStep::Text("continuing".into()),
                FakeStep::Gate("cont".into()),
            ],
        )
        .turn(
            "work",
            [
                FakeStep::Text("working".into()),
                FakeStep::Gate("work".into()),
            ],
        )
        .turn("fail", [FakeStep::Fail("Overloaded".into())])
        .default_turn([FakeStep::Text("ok".into())])
}

async fn add(h: &Harness, id: SessionId, texts: &[&str]) -> Vec<TodoId> {
    h.rt.agent_todo_add(
        id,
        AgentTodoAddParams {
            call_id: OpId::generate().to_string(),
            texts: texts.iter().map(|t| (*t).to_owned()).collect(),
        },
    )
    .await
    .unwrap()
    .todos
    .iter()
    .map(|t| t.id)
    .collect()
}

async fn set(h: &Harness, id: SessionId, todo: TodoId, status: TodoStatus) {
    h.rt.agent_todo_update(
        id,
        AgentTodoUpdateParams {
            call_id: OpId::generate().to_string(),
            todo_id: todo,
            text: None,
            status: Some(status),
            reason: (status == TodoStatus::Blocked).then(|| "needs a key".to_owned()),
        },
    )
    .await
    .unwrap();
}

async fn update(h: &Harness, id: SessionId, on: Option<bool>, budget: Option<u32>) {
    h.rt.update(SessionUpdateParams {
        op_id: OpId::generate(),
        id,
        name: None,
        auto_send: None,
        auto_continue: on,
        auto_continue_budget: budget,
    })
    .await
    .unwrap();
}

fn continues(h: &Harness, id: SessionId) -> usize {
    h.host
        .prompts(id)
        .iter()
        .filter(|p| p.contains(CONTINUE))
        .count()
}

fn status(s: &SessionSummary) -> AutoContinueStatus {
    s.todo
        .map_or(AutoContinueStatus::Unknown, |t| t.auto_continue)
}

/// Runs "work" with `texts` added as todos during the run, and waits for its end.
async fn work_with_todos(h: &mut Harness, id: SessionId, texts: &[&str]) -> Vec<TodoId> {
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    let ids = add(h, id, texts).await;
    h.host.release(id, "work");
    ids
}

/// Waits until the `n`th continue run is at its gate.
async fn wait_continue(h: &mut Harness, id: SessionId, n: usize) {
    h.host
        .wait("a continue run", |host| {
            host.prompts(id)
                .iter()
                .filter(|p| p.contains(CONTINUE))
                .count()
                >= n
        })
        .await;
    h.wait(id, "continuing", |s| {
        s.preview == "continuing" && s.state == SessionState::Working
    })
    .await;
}

#[tokio::test]
async fn it_continues_while_open_items_remain_and_stops_when_they_are_done() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    let todos = work_with_todos(&mut h, id, &["one", "two"]).await;

    wait_continue(&mut h, id, 1).await;
    let prompt = h.host.prompts(id).pop().unwrap();
    assert!(prompt.contains("1. [pending] one"), "{prompt}");
    assert_eq!(
        h.run_triggers(id).await.last(),
        Some(&RunTrigger::AutoContinue)
    );
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(
        (detail.status, detail.runs_used),
        (AutoContinueStatus::On, 1)
    );

    set(&h, id, todos[0], TodoStatus::Done).await;
    h.host.release(id, "cont");
    wait_continue(&mut h, id, 2).await;
    set(&h, id, todos[1], TodoStatus::Done).await;
    h.host.release(id, "cont");
    let s = h.wait_state(id, SessionState::Idle).await;
    // Nothing left: no third continuation, and nothing needs the user.
    assert_eq!(continues(&h, id), 2);
    assert_eq!(status(&s), AutoContinueStatus::On);
    assert_eq!(s.attention, Some(Attention::Unread));
    assert_eq!(s.todo.map(|t| (t.done, t.total)), Some((2, 2)));
}

#[tokio::test]
async fn it_pauses_when_only_blocked_items_remain() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    let todos = work_with_todos(&mut h, id, &["one", "two"]).await;
    wait_continue(&mut h, id, 1).await;
    set(&h, id, todos[0], TodoStatus::Done).await;
    set(&h, id, todos[1], TodoStatus::Blocked).await;
    h.host.release(id, "cont");
    let s = h
        .wait(id, "paused", |s| status(s) == AutoContinueStatus::Paused)
        .await;
    assert_eq!(s.attention, Some(Attention::Problem));
    assert_eq!(
        h.detail(id).await.auto_continue.reason.as_deref(),
        Some("Only blocked items remain")
    );
    assert_eq!(continues(&h, id), 1);
}

#[tokio::test]
async fn it_stalls_after_three_runs_without_progress() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    work_with_todos(&mut h, id, &["stubborn"]).await;
    for n in 1..=3 {
        wait_continue(&mut h, id, n).await;
        h.host.release(id, "cont");
    }
    let s = h
        .wait(id, "stalled", |s| status(s) == AutoContinueStatus::Stalled)
        .await;
    assert_eq!(s.state, SessionState::Idle);
    assert_eq!(s.attention, Some(Attention::Problem));
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(detail.reason.as_deref(), Some("3 runs without progress"));
    assert_eq!(continues(&h, id), 3);

    // Resuming starts a continue run at once, with a fresh budget.
    update(&h, id, Some(true), None).await;
    wait_continue(&mut h, id, 4).await;
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(
        (detail.status, detail.runs_used),
        (AutoContinueStatus::On, 1)
    );
    h.host.release(id, "cont");
}

#[tokio::test]
async fn it_pauses_when_the_budget_is_used_and_raising_it_resumes() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    update(&h, id, None, Some(2)).await;
    let todos = work_with_todos(&mut h, id, &["a", "b", "c", "d"]).await;
    for (n, todo) in todos.iter().take(2).enumerate() {
        wait_continue(&mut h, id, n + 1).await;
        set(&h, id, *todo, TodoStatus::Done).await;
        h.host.release(id, "cont");
    }
    let s = h
        .wait(id, "budget", |s| status(s) == AutoContinueStatus::Budget)
        .await;
    assert_eq!(s.attention, Some(Attention::Problem));
    let detail = h.detail(id).await.auto_continue;
    assert_eq!(detail.reason.as_deref(), Some("Budget of 2 runs used"));
    assert_eq!((detail.runs_used, detail.budget), (2, 2));

    update(&h, id, None, Some(4)).await;
    wait_continue(&mut h, id, 3).await;
    assert_eq!(
        h.detail(id).await.auto_continue.status,
        AutoContinueStatus::On
    );
    h.host.release(id, "cont");
}

#[tokio::test]
async fn an_open_question_waits_for_its_answer_instead() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    add(&h, id, &["one"]).await;
    h.rt.agent_ask(
        id,
        QuestionAskParams {
            call_id: "q".into(),
            title: None,
            questions: vec![AgentQuestion {
                text: "Which?".into(),
                options: Vec::new(),
                multi: false,
                free_text: true,
            }],
        },
    )
    .await
    .unwrap();
    h.host.release(id, "work");
    let s = h.wait_state(id, SessionState::Idle).await;
    assert_eq!(continues(&h, id), 0);
    assert_eq!(status(&s), AutoContinueStatus::On);
    assert_eq!(
        h.detail(id).await.auto_continue.reason.as_deref(),
        Some("waiting on an answer")
    );
}

#[tokio::test]
async fn queued_prompts_go_first() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    h.prompt(id, "work on it").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    add(&h, id, &["one"]).await;
    h.prompt(id, "queued thing").await;
    h.host.release(id, "work");
    // The queued prompt runs next; auto-continue decides after it.
    wait_continue(&mut h, id, 1).await;
    let prompts = h.host.prompts(id);
    assert_eq!(prompts[1], "queued thing");
    assert!(prompts[2].contains(CONTINUE));
    h.host.release(id, "cont");
}

#[tokio::test]
async fn a_failed_run_or_auto_continue_off_doesnt_continue() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    add(&h, id, &["one"]).await;
    h.prompt(id, "fail now").await;
    let s = h.wait_state(id, SessionState::Failed).await;
    assert_eq!(s.last_outcome, Some(RunOutcome::Failed));
    assert_eq!(continues(&h, id), 0);

    update(&h, id, Some(false), None).await;
    h.prompt(id, "hello").await;
    let s = h
        .wait(id, "the run", |s| {
            s.last_outcome == Some(RunOutcome::Completed)
        })
        .await;
    assert_eq!(status(&s), AutoContinueStatus::Off);
    assert_eq!(continues(&h, id), 0);
}

#[tokio::test]
async fn a_user_cancel_pauses_it() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    work_with_todos(&mut h, id, &["one"]).await;
    wait_continue(&mut h, id, 1).await;
    h.rt.cancel(h.op(id)).await.unwrap();
    let s = h
        .wait(id, "cancelled", |s| {
            s.state == SessionState::Idle && s.last_outcome == Some(RunOutcome::Cancelled)
        })
        .await;
    assert_eq!(status(&s), AutoContinueStatus::Paused);
    assert_eq!(
        h.detail(id).await.auto_continue.reason.as_deref(),
        Some("Cancelled by the user")
    );
    assert!(
        h.host
            .commands(id)
            .iter()
            .any(|c| matches!(c, Command::Cancel))
    );
    assert_eq!(continues(&h, id), 1);
}

#[tokio::test]
async fn a_user_run_starts_a_fresh_budget() {
    let mut h = Harness::new(script()).await;
    let id = h.create().await;
    let todos = work_with_todos(&mut h, id, &["one", "two", "three"]).await;
    wait_continue(&mut h, id, 1).await;
    set(&h, id, todos[0], TodoStatus::Done).await;
    update(&h, id, Some(false), None).await;
    h.host.release(id, "cont");
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(h.detail(id).await.auto_continue.runs_used, 1);
    update(&h, id, Some(true), None).await;
    wait_continue(&mut h, id, 2).await;
    set(&h, id, todos[1], TodoStatus::Done).await;
    h.host.release(id, "cont");
    wait_continue(&mut h, id, 3).await;
    assert_eq!(h.detail(id).await.auto_continue.runs_used, 2);
    set(&h, id, todos[2], TodoStatus::Done).await;
    h.host.release(id, "cont");
    h.wait_state(id, SessionState::Idle).await;
    h.prompt(id, "work again").await;
    h.wait(id, "working", |s| s.preview == "working").await;
    assert_eq!(h.detail(id).await.auto_continue.runs_used, 0);
    h.host.release(id, "work");
}
