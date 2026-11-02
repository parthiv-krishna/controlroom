//! Create, prompt, read cursors, op dedup, options, context, subscriptions.

use controlroom_drivers::fake::wire::Command;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::daemon::{
    IfBusy, InitialPrompt, MachineEvent, OptionValue, PromptResult, SessionCreateParams,
    SessionMarkReadParams, SessionOpParams, SessionPromptParams, SessionSetOptionParams,
};
use controlroom_protocol::ids::{OpId, RemotePath, Rev, Seq, SessionId};
use controlroom_protocol::session::{
    ContextUsage, OptionChoice, OptionControl, PromptOrigin, RunOutcome, SessionOption,
    SessionState, StartedBy,
};
use controlroom_protocol::timeline::{ItemBody, MessageRole, RunTrigger, TimelineOp};
use controlroom_protocol::{ErrorData, Resource, RpcError};

use super::{Harness, text};

fn kind(e: &RpcError) -> &ErrorData {
    e.data.as_ref().unwrap()
}

#[tokio::test]
async fn create_with_a_first_prompt_is_one_command() {
    let script = FakeScript::default().default_turn([FakeStep::Text("on it".into())]);
    let mut h = Harness::new(script).await;
    let create = SessionCreateParams {
        op_id: OpId::new("create-with-prompt"),
        agent: "fake".into(),
        cwd: RemotePath::new("~/src/app"),
        name: None,
        parent_id: None,
        spawned_by: None,
        sandbox_level: None,
        sandbox: None,
        prompt: Some(InitialPrompt {
            content: text("fix the flaky test"),
            origin: PromptOrigin::default(),
        }),
    };
    let created = h.rt.create(create.clone()).await.unwrap();
    let id = created.session.id;
    assert!(matches!(created.prompt, Some(PromptResult::Queued { .. })));
    assert_eq!(
        created.session.queue.len, 1,
        "the prompt is queued with the session"
    );
    assert_eq!(created.session.state, SessionState::Starting);
    // A retried create returns the same session and prompt; nothing is created twice.
    assert_eq!(h.rt.create(create.clone()).await.unwrap(), created);

    let s = h
        .wait(id, "the first run", |s| {
            s.last_outcome == Some(RunOutcome::Completed) && s.queue.len == 0
        })
        .await;
    assert_eq!(s.preview, "on it");
    assert_eq!(h.host.prompts(id), ["fix the flaky test"]);
    // It ran as the user's prompt, not as a queued one.
    let sub =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap();
    let triggers: Vec<RunTrigger> = sub
        .backlog
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::Run { trigger, .. } => Some(trigger.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(triggers, [RunTrigger::User { device: None }]);
    let machine = h.rt.subscribe_machine(None).unwrap();
    assert_eq!(machine.snapshot.unwrap().sessions.len(), 1);

    // After a restart the replay still finds the stored result; the prompt isn't resent.
    let mut h = h.restart().await;
    assert_eq!(h.rt.create(create).await.unwrap(), created);
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(h.host.prompts(id), ["fix the flaky test"]);

    // An empty first prompt is refused, and nothing is created.
    let err =
        h.rt.create(SessionCreateParams {
            op_id: OpId::generate(),
            prompt: Some(InitialPrompt {
                content: Vec::new(),
                origin: PromptOrigin::default(),
            }),
            ..plain_create()
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&err), &ErrorData::InvalidParams);
    let machine = h.rt.subscribe_machine(None).unwrap();
    assert_eq!(machine.snapshot.unwrap().sessions.len(), 1);
}

fn plain_create() -> SessionCreateParams {
    SessionCreateParams {
        op_id: OpId::generate(),
        agent: "fake".into(),
        cwd: RemotePath::new("~/src/app"),
        name: None,
        parent_id: None,
        spawned_by: None,
        sandbox_level: None,
        sandbox: None,
        prompt: None,
    }
}

#[tokio::test]
async fn a_detail_only_subscription_has_no_timeline() {
    let script = FakeScript::default().default_turn([FakeStep::Text("hello".into())]);
    let mut h = Harness::new(script).await;
    let id = h.create().await;
    h.prompt(id, "hi").await;
    h.wait(id, "the reply", |s| {
        s.preview == "hello" && s.state == SessionState::Idle
    })
    .await;
    let full =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap();
    let detail = h.rt.subscribe_session(id, None, None).await.unwrap();
    assert!(!full.backlog.is_empty());
    assert!(detail.backlog.is_empty());
    assert_eq!(detail.head_seq, full.head_seq);
    assert_eq!(detail.detail, full.detail);
}

#[tokio::test]
async fn create_prompt_reply_idle() {
    let mut h = Harness::new(FakeScript::default().default_turn([
        FakeStep::Text("hello from the agent\n".into()),
        FakeStep::Text("**second** line".into()),
    ]))
    .await;
    let id = h.create().await;
    let s = h.summary(id);
    assert!(s.resumable);
    assert!(s.queue.auto_send);
    assert_eq!(s.cwd.as_str(), "/home/alice/src/app");

    assert_eq!(h.prompt(id, "hi").await, PromptResult::Sent);
    let s = h
        .wait(id, "turn end", |s| {
            s.last_outcome == Some(RunOutcome::Completed)
        })
        .await;
    assert_eq!(s.state, SessionState::Idle);
    assert!(s.unread);
    assert_eq!(s.preview, "second line", "Markdown stripped, last line");
    let end = s.last_turn_end_seq;

    let read =
        h.rt.mark_read(SessionMarkReadParams {
            op_id: OpId::generate(),
            id,
            seq: end,
        })
        .await
        .unwrap();
    assert_eq!((read.read_seq, read.unread), (end, false));
    // Forward only.
    let read =
        h.rt.mark_read(SessionMarkReadParams {
            op_id: OpId::generate(),
            id,
            seq: Seq(1),
        })
        .await
        .unwrap();
    assert_eq!(read.read_seq, end);
    let unread = h.rt.mark_unread(h.op(id)).await.unwrap();
    assert_eq!((unread.read_seq, unread.unread), (Seq(end.0 - 1), true));

    let detail =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap();
    assert_eq!(detail.detail.agent_sessions.len(), 1);
    assert_eq!(detail.detail.agent_sessions[0].started_by, StartedBy::New);
    // The run's prompt and reply are in the materialized timeline.
    let messages: Vec<_> = detail
        .backlog
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::Message { role, content } => Some((*role, content.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(messages[0], (MessageRole::User, text("hi")));
    assert_eq!(
        messages[1],
        (
            MessageRole::Agent,
            text("hello from the agent\n**second** line")
        )
    );
    assert!(detail.backlog.iter().all(|item| !item.streaming));
}

#[tokio::test]
async fn unknown_agents_and_sessions() {
    let h = Harness::new(FakeScript::default()).await;
    let err =
        h.rt.create(SessionCreateParams {
            op_id: OpId::generate(),
            agent: "nope".into(),
            cwd: RemotePath::new("~/src"),
            name: None,
            parent_id: None,
            spawned_by: None,
            sandbox_level: None,
            sandbox: None,
            prompt: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(kind(&err), ErrorData::NotFound { .. }));
    let err = h.rt.cancel(h.op(SessionId::generate())).await.unwrap_err();
    assert!(matches!(kind(&err), ErrorData::UnknownSession { .. }));
}

#[tokio::test]
async fn op_id_replay_applies_once() {
    let mut h = Harness::new(FakeScript::default()).await;
    let create = SessionCreateParams {
        op_id: OpId::new("create-1"),
        agent: "fake".into(),
        cwd: RemotePath::new("~/src/app"),
        name: Some("app".into()),
        parent_id: None,
        spawned_by: None,
        sandbox_level: None,
        sandbox: None,
        prompt: None,
    };
    let first = h.rt.create(create.clone()).await.unwrap();
    let again = h.rt.create(create).await.unwrap();
    assert_eq!(first, again);
    let id = first.session.id;
    assert_eq!(first.session.cwd.as_str(), "/home/alice/src/app");
    h.wait_state(id, SessionState::Idle).await;

    let prompt = SessionPromptParams {
        op_id: OpId::new("prompt-1"),
        id,
        content: text("only once"),
        origin: PromptOrigin::default(),
        if_busy: IfBusy::Queue,
    };
    let first = h.rt.prompt(prompt.clone()).await.unwrap();
    let again = h.rt.prompt(prompt).await.unwrap();
    assert_eq!(first, again);
    h.wait(id, "turn end", |s| {
        s.last_outcome.is_some() && s.state == SessionState::Idle
    })
    .await;
    assert_eq!(h.host.prompts(id), ["only once"]);

    let err =
        h.rt.cancel(SessionOpParams {
            op_id: OpId::new("prompt-1"),
            id,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&err), &ErrorData::OpIdConflict);
}

fn options_script() -> FakeScript {
    FakeScript::default()
        .options(vec![
            SessionOption {
                id: "model".into(),
                label: "Model".into(),
                control: OptionControl::Select {
                    value: "model-1".into(),
                    choices: ["model-1", "model-2"]
                        .map(|v| OptionChoice {
                            value: v.into(),
                            label: v.into(),
                        })
                        .to_vec(),
                },
            },
            SessionOption {
                id: "fast".into(),
                label: "Fast".into(),
                control: OptionControl::Bool { value: false },
            },
        ])
        .turn(
            "stream",
            [
                FakeStep::Text("streaming".into()),
                FakeStep::Gate("go".into()),
                FakeStep::Text(" and done".into()),
            ],
        )
        .default_turn([
            FakeStep::Text("ok".into()),
            FakeStep::Context {
                used: 120,
                size: 1000,
            },
        ])
}

fn set(id: SessionId, option: &str, value: OptionValue) -> SessionSetOptionParams {
    SessionSetOptionParams {
        op_id: OpId::generate(),
        id,
        option_id: option.into(),
        value,
    }
}

fn model(s: &controlroom_protocol::session::SessionSummary) -> String {
    match &s.options[0].control {
        OptionControl::Select { value, .. } => value.clone(),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn set_option_reaches_the_agent_once() {
    let mut h = Harness::new(options_script()).await;
    let id = h.create().await;
    assert_eq!(model(&h.summary(id)), "model-1");
    assert!(h.summary(id).agent_live);

    let params = set(id, "model", OptionValue::Choice("model-2".into()));
    h.rt.set_option(params.clone()).await.unwrap();
    h.rt.set_option(params).await.unwrap();
    h.wait(id, "the new model", |s| model(s) == "model-2").await;
    let sets = |h: &Harness| {
        h.host
            .commands(id)
            .into_iter()
            .filter(|c| matches!(c, Command::SetOption { .. }))
            .count()
    };
    assert_eq!(sets(&h), 1);

    let err =
        h.rt.set_option(set(id, "nope", OptionValue::Choice("x".into())))
            .await
            .unwrap_err();
    assert_eq!(
        kind(&err),
        &ErrorData::NotFound {
            resource: Resource::Option,
            id: "nope".into()
        }
    );
    for (option, value) in [
        ("model", OptionValue::Choice("model-9".into())),
        ("model", OptionValue::Bool(true)),
        ("fast", OptionValue::Choice("true".into())),
    ] {
        let err = h.rt.set_option(set(id, option, value)).await.unwrap_err();
        assert_eq!(kind(&err), &ErrorData::InvalidParams, "{option}");
    }
    assert_eq!(sets(&h), 1);

    // A stopped agent isn't started for it.
    h.host.kill_agent(id);
    let s = h.wait_state(id, SessionState::Dormant).await;
    assert!(!s.agent_live);
    let err =
        h.rt.set_option(set(id, "fast", OptionValue::Bool(true)))
            .await
            .unwrap_err();
    assert_eq!(
        kind(&err),
        &ErrorData::InvalidState {
            state: SessionState::Dormant
        }
    );
}

#[tokio::test]
async fn set_option_during_a_turn() {
    let mut h = Harness::new(options_script()).await;
    let id = h.create().await;
    h.prompt(id, "stream please").await;
    h.wait(id, "streaming", |s| s.preview == "streaming").await;
    assert_eq!(h.summary(id).state, SessionState::Working);
    h.rt.set_option(set(id, "model", OptionValue::Choice("model-2".into())))
        .await
        .unwrap();
    h.wait(id, "the new model", |s| model(s) == "model-2").await;
    h.host.release(id, "go");
    let s = h
        .wait(id, "turn end", |s| {
            s.last_outcome == Some(RunOutcome::Completed)
        })
        .await;
    assert_eq!(s.preview, "streaming and done");
    assert!(!s.queue.paused);
}

#[tokio::test]
async fn set_option_is_unsupported_without_options() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let err =
        h.rt.set_option(set(id, "model", OptionValue::Choice("a".into())))
            .await
            .unwrap_err();
    assert_eq!(kind(&err), &ErrorData::Unsupported);
    assert_eq!(h.summary(id).context, None);
}

#[tokio::test]
async fn context_usage_is_on_the_summary_and_survives_a_restart() {
    let mut h = Harness::new(options_script()).await;
    let id = h.create().await;
    h.prompt(id, "hi").await;
    let s = h.wait(id, "usage", |s| s.context.is_some()).await;
    assert_eq!(
        s.context,
        Some(ContextUsage {
            used: 120,
            size: Some(1000),
            cost: None
        })
    );
    let h = h.restart().await;
    assert_eq!(h.summary(id).context.unwrap().used, 120);
}

#[tokio::test]
async fn subscriptions_resume_from_since_seq_and_since_rev() {
    let mut h = Harness::new(
        FakeScript::default().default_turn(["a", "b", "c"].map(|t| FakeStep::Text(t.into()))),
    )
    .await;
    let rev0 = h.rt.subscribe_machine(None).unwrap().rev;
    let id = h.create().await;
    h.prompt(id, "go").await;
    h.wait(id, "turn end", |s| {
        s.last_outcome.is_some() && s.state == SessionState::Idle
    })
    .await;

    // Resubscribing from an old rev catches up without a snapshot, in rev order.
    let resumed = h.rt.subscribe_machine(Some(rev0)).unwrap();
    assert!(resumed.snapshot.is_none());
    let revs: Vec<u64> = resumed.backlog.iter().map(|c| c.rev.0).collect();
    assert_eq!(revs.first(), Some(&(rev0.0 + 1)));
    assert!(revs.windows(2).all(|w| w[1] == w[0] + 1));
    assert_eq!(*revs.last().unwrap(), resumed.rev.0);
    assert!(matches!(
        &resumed.backlog[0].event,
        MachineEvent::SessionUpserted { session } if session.id == id
    ));
    // A rev from the future (another daemon's) or none gets a snapshot.
    let future =
        h.rt.subscribe_machine(Some(Rev(resumed.rev.0 + 1000)))
            .unwrap();
    assert_eq!(future.snapshot.unwrap().sessions.len(), 1);
    // The rev survives a restart.
    let rev = resumed.rev;
    let h = h.restart().await;
    let after = h.rt.subscribe_machine(Some(rev)).unwrap();
    assert!(after.snapshot.is_none());
    assert!(after.rev >= rev);

    // The timeline: everything from 0, and from the middle only what changed after it.
    let all =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap();
    let mid = Seq(all.head_seq.0 / 2);
    let rest = h.rt.subscribe_session(id, Some(mid), None).await.unwrap();
    assert!(rest.backlog.len() < all.backlog.len());
    assert!(rest.backlog.iter().all(|item| item.seq_to > mid));
    assert!(
        all.backlog
            .windows(2)
            .all(|w| w[0].seq_from <= w[1].seq_from)
    );
    // Raw frames on request.
    let raw =
        h.rt.subscribe_session(id, Some(all.head_seq), Some(mid))
            .await
            .unwrap();
    assert!(raw.backlog.is_empty());
    assert_eq!(raw.raw.first().unwrap().seq, Seq(mid.0 + 1));
    assert_eq!(raw.raw.last().unwrap().seq, all.head_seq);
}

#[tokio::test]
async fn live_updates_follow_the_backlog_without_gaps() {
    let mut h = Harness::new(FakeScript::default().default_turn([
        FakeStep::Text("one".into()),
        FakeStep::Gate("go".into()),
        FakeStep::Text(" two".into()),
    ]))
    .await;
    let id = h.create().await;
    h.prompt(id, "go").await;
    h.wait(id, "first text", |s| s.preview == "one").await;
    let mut sub =
        h.rt.subscribe_session(id, Some(Seq(0)), None)
            .await
            .unwrap();
    h.host.release(id, "go");
    let mut appended = String::new();
    let mut head = sub.head_seq;
    while !appended.contains("two") {
        let update = sub.updates.recv().await.unwrap();
        assert_eq!(update.head_seq, Seq(head.0 + 1), "no gaps");
        head = update.head_seq;
        for op in update.ops {
            if let TimelineOp::AppendText { text, .. } = op {
                appended.push_str(&text);
            }
        }
    }
    assert_eq!(appended, " two");
}

#[tokio::test]
async fn prompt_while_busy_can_skip() {
    let mut h =
        Harness::new(FakeScript::default().default_turn([FakeStep::Gate("go".into())])).await;
    let id = h.create().await;
    h.prompt(id, "first").await;
    let skipped =
        h.rt.prompt(SessionPromptParams {
            op_id: OpId::generate(),
            id,
            content: text("scheduled"),
            origin: PromptOrigin::default(),
            if_busy: IfBusy::Skip,
        })
        .await
        .unwrap();
    assert_eq!(skipped, PromptResult::Skipped);
    assert!(
        h.queue(id)
            .await
            .iter()
            .all(|item| item.content != text("scheduled"))
    );
}

#[tokio::test]
async fn preview_only_changes_are_throttled_to_one_a_second() {
    let mut h = Harness::new(FakeScript::default().default_turn([
        FakeStep::Text("first".into()),
        FakeStep::Gate("go".into()),
        FakeStep::Text("\nsecond".into()),
        FakeStep::Gate("end".into()),
    ]))
    .await;
    let id = h.create().await;
    h.prompt(id, "go").await;
    h.wait(id, "the first text", |s| s.preview == "first").await;
    h.host.release(id, "go");
    h.wait(id, "the second line", |s| s.preview == "second")
        .await;
    let last_emitted = |h: &Harness| {
        h.emitted
            .iter()
            .rev()
            .find(|s| s.id == id)
            .map(|s| s.preview.clone())
            .unwrap()
    };
    assert_ne!(last_emitted(&h), "second", "held back within the second");
    h.rt.tick();
    h.wait(id, "a tick", |_| true).await;
    assert_ne!(last_emitted(&h), "second", "still within the second");
    h.clock.advance(1000);
    h.rt.tick();
    h.wait(id, "the throttled preview", |_| true).await;
    assert_eq!(last_emitted(&h), "second");
}
