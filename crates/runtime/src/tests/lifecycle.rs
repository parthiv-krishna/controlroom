//! Lifecycle: start fresh, close/reopen/delete, resume, idle-stop, retention, sandbox levels.

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::{
    RetentionPolicy, SessionCreateParams, SessionResumeParams, SessionStartFreshParams,
    SessionUpdateParams, SettingsSetParams,
};
use controlroom_protocol::ids::{OpId, Seq, SessionId};
use controlroom_protocol::session::{
    RunOutcome, SandboxLevel, SandboxSpec, SessionState, StartedBy,
};

use super::{Harness, caps};

const HOUR_MS: i64 = 3600 * 1000;

fn fresh(id: SessionId, include_history: bool) -> SessionStartFreshParams {
    SessionStartFreshParams {
        op_id: OpId::generate(),
        id,
        include_history,
        sandbox: None,
    }
}

async fn agent_sessions(
    h: &Harness,
    id: SessionId,
) -> Vec<controlroom_protocol::session::AgentSession> {
    h.rt.subscribe_session(id, Some(Seq(0)), None)
        .await
        .unwrap()
        .detail
        .agent_sessions
}

#[tokio::test]
async fn start_fresh_with_history() {
    let mut h = Harness::new(FakeScript::default().default_turn([
        FakeStep::Tool {
            title: "ls".into(),
            output: "a.txt".into(),
            gate: None,
        },
        FakeStep::Text("one file".into()),
    ]))
    .await;
    let id = h.create().await;
    h.prompt(id, "list files").await;
    h.wait(id, "run end", |s| {
        s.last_outcome == Some(RunOutcome::Completed)
    })
    .await;

    // The agent is live and can't start a new session in-process: it is restarted.
    let result = h.rt.start_fresh(fresh(id, true)).await.unwrap();
    let path = result.history_path.unwrap();
    assert!(path.as_str().ends_with("history-1.md"), "{path}");
    let history = h.host.file(path.as_str()).unwrap();
    for expected in ["list files", "Tool call: ls", "a.txt", "one file"] {
        assert!(history.contains(expected), "{expected} in {history}");
    }
    h.wait(id, "the fresh agent", |s| {
        s.state == SessionState::Idle && s.last_turn_end_seq.0 > 0
    })
    .await;
    h.host
        .wait("the fresh agent session", |host| {
            host.supervisor_running(id)
        })
        .await;
    let mut sessions = agent_sessions(&h, id).await;
    while sessions.len() < 2 {
        h.wait(id, "a second agent session", |s| {
            s.state == SessionState::Idle
        })
        .await;
        sessions = agent_sessions(&h, id).await;
    }
    assert_eq!(sessions[1].started_by, StartedBy::Fresh);
    assert_ne!(sessions[0].native_id, sessions[1].native_id);

    h.prompt(id, "continue").await;
    h.wait(id, "run end", |s| {
        s.state == SessionState::Idle && s.queue.len == 0
    })
    .await;
    let last = h.host.prompts(id).pop().unwrap();
    let note =
        format!("Earlier conversation from this session is in `{path}`; read it if relevant.");
    assert!(last.starts_with(&note), "{last}");
    assert!(last.ends_with("continue"));
    // Only the first prompt after the restart carries the note.
    h.prompt(id, "more").await;
    h.wait(id, "run end", |s| {
        s.state == SessionState::Idle && s.queue.len == 0
    })
    .await;
    assert_eq!(h.host.prompts(id).pop().unwrap(), "more");
}

#[tokio::test]
async fn start_fresh_in_process_when_the_driver_can() {
    let caps = Capabilities {
        new_session: true,
        ..caps()
    };
    let mut h = Harness::with(FakeScript::default(), caps).await;
    let id = h.create().await;
    let metas = |h: &Harness| {
        h.host
            .transcript(id)
            .iter()
            .filter(|r| matches!(r.body, controlroom_transcript::Body::Meta(_)))
            .count()
    };
    assert_eq!(metas(&h), 1);
    let result = h.rt.start_fresh(fresh(id, false)).await.unwrap();
    assert_eq!(result.history_path, None);
    let mut sessions = agent_sessions(&h, id).await;
    while sessions.len() < 2 {
        h.wait(id, "the new agent session", |s| {
            s.state == SessionState::Idle
        })
        .await;
        sessions = agent_sessions(&h, id).await;
    }
    assert_eq!(sessions[1].started_by, StartedBy::Fresh);
    assert!(sessions[0].ended_at.is_some());
    assert_eq!(metas(&h), 1, "same process");
}

/// pi names its new session only when asked: after an in-process start fresh the runtime
/// asks, so the new agent session is still recorded (and resumed later, not replaced).
#[tokio::test]
async fn start_fresh_in_process_asks_an_agent_that_reports_nothing() {
    let caps = Capabilities {
        new_session: true,
        ..caps()
    };
    let mut h = Harness::with(FakeScript::default().quiet_switch(), caps).await;
    let id = h.create().await;
    h.rt.start_fresh(fresh(id, false)).await.unwrap();
    let mut sessions = agent_sessions(&h, id).await;
    while sessions.len() < 2 {
        h.wait(id, "the new agent session", |s| {
            s.state == SessionState::Idle
        })
        .await;
        sessions = agent_sessions(&h, id).await;
    }
    assert_eq!(sessions[1].started_by, StartedBy::Fresh);
    assert_ne!(sessions[1].native_id, sessions[0].native_id);
}

#[tokio::test]
async fn close_reopen_delete() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    h.rt.close(h.op(id)).await.unwrap();
    let s = h.wait_state(id, SessionState::Closed).await;
    assert!(s.closed_at.is_some());
    h.host
        .wait("the agent to stop", |host| !host.supervisor_running(id))
        .await;
    let err = h.try_prompt(id, "x").await.unwrap_err();
    assert_eq!(
        err.data,
        Some(ErrorData::InvalidState {
            state: SessionState::Closed
        })
    );

    h.rt.reopen(h.op(id)).await.unwrap();
    let s = h.wait_state(id, SessionState::Dormant).await;
    assert_eq!(s.closed_at, None);
    h.rt.update(SessionUpdateParams {
        op_id: OpId::generate(),
        id,
        name: Some("renamed".into()),
        auto_send: None,
        auto_continue: None,
        auto_continue_budget: None,
    })
    .await
    .unwrap();
    assert_eq!(h.summary(id).name.as_deref(), Some("renamed"));

    h.rt.delete(h.op(id)).await.unwrap();
    assert!(h.rt.summary(id).is_err());
    h.host
        .wait("the files removed", |host| host.removed(id))
        .await;
    let err = h.rt.cancel(h.op(id)).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::UnknownSession { .. })));
    assert!(
        h.rt.subscribe_sessions(None)
            .unwrap()
            .snapshot
            .unwrap()
            .sessions
            .is_empty()
    );
}

#[tokio::test]
async fn closing_or_deleting_while_starting_leaves_no_agent() {
    let h = Harness::new(FakeScript::default()).await;
    let closed =
        h.rt.create(h.create_params(&[(h.devbox, "~/src/app")]))
            .await
            .unwrap()
            .session
            .id;
    h.rt.close(h.op(closed)).await.unwrap();
    let deleted =
        h.rt.create(h.create_params(&[(h.devbox, "~/src/app")]))
            .await
            .unwrap()
            .session
            .id;
    h.rt.delete(h.op(deleted)).await.unwrap();
    h.host
        .wait("no agents left", |host| {
            !host.supervisor_running(closed) && !host.supervisor_running(deleted)
        })
        .await;
    assert_eq!(h.summary(closed).state, SessionState::Closed);
}

#[tokio::test]
async fn resume_and_not_resumable() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    h.host.kill_agent(id);
    h.wait_state(id, SessionState::Dormant).await;
    let resume = SessionResumeParams {
        op_id: OpId::generate(),
        id,
        sandbox: None,
    };
    h.rt.resume(resume.clone()).await.unwrap();
    h.wait_state(id, SessionState::Idle).await;
    assert_eq!(
        agent_sessions(&h, id).await[1].started_by,
        StartedBy::Resume
    );
    // Resuming a live session is refused (a new op id; the old one replays).
    let again = SessionResumeParams {
        op_id: OpId::generate(),
        ..resume
    };
    let err = h.rt.resume(again).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::InvalidState { .. })));

    let caps = Capabilities {
        resume: false,
        ..caps()
    };
    let mut h = Harness::with(FakeScript::default(), caps).await;
    let id = h.create().await;
    h.host.kill_agent(id);
    let s = h.wait_state(id, SessionState::Dormant).await;
    assert!(!s.resumable);
    let err = h.try_prompt(id, "hello?").await.unwrap_err();
    assert_eq!(err.data, Some(ErrorData::NotResumable));
}

#[tokio::test]
async fn idle_stop_only_stops_resumable_idle_agents() {
    let mut h = Harness::new(FakeScript::default()).await;
    h.rt.set_settings(SettingsSetParams {
        op_id: OpId::generate(),
        idle_timeout_h: Some(2),
        retention: None,
    })
    .await
    .unwrap();
    let id = h.create().await;
    h.clock.advance(HOUR_MS);
    h.rt.tick();
    assert_eq!(
        h.summary(id).state,
        SessionState::Idle,
        "not idle long enough"
    );
    h.clock.advance(2 * HOUR_MS);
    h.rt.tick();
    let s = h.wait_state(id, SessionState::Dormant).await;
    assert!(s.resumable);

    let caps = Capabilities {
        resume: false,
        ..caps()
    };
    let mut h = Harness::with(FakeScript::default(), caps).await;
    let id = h.create().await;
    h.clock.advance(100 * HOUR_MS);
    h.rt.tick();
    h.prompt(id, "still here").await;
    h.wait(id, "run end", |s| {
        s.last_outcome.is_some() && s.state == SessionState::Idle
    })
    .await;
    assert!(
        h.host.supervisor_running(id),
        "unresumable agents are never stopped"
    );
}

#[tokio::test]
async fn retention_purges_closed_sessions() {
    let mut h = Harness::new(FakeScript::default()).await;
    h.rt.set_settings(SettingsSetParams {
        op_id: OpId::generate(),
        idle_timeout_h: None,
        retention: Some(RetentionPolicy {
            delete_closed_after_days: Some(1),
        }),
    })
    .await
    .unwrap();
    let open = h.create().await;
    let id = h.create().await;
    h.rt.close(h.op(id)).await.unwrap();
    h.wait_state(id, SessionState::Closed).await;
    h.clock.advance(23 * HOUR_MS);
    h.rt.tick();
    assert!(h.rt.summary(id).is_ok(), "closed for less than a day");
    h.clock.advance(2 * HOUR_MS);
    h.rt.tick();
    assert!(h.rt.summary(id).is_err(), "closed for a day: purged");
    h.host
        .wait("the files removed", |host| host.removed(id))
        .await;
    let err = h.rt.reopen(h.op(id)).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::Deleted { .. })));
    assert!(h.rt.summary(open).is_ok(), "open sessions stay");
    // The tombstone survives a restart.
    let h = h.restart().await;
    let err = h.rt.reopen(h.op(id)).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::Deleted { .. })));
}

#[tokio::test]
async fn explicit_sandbox_levels_win_in_either_direction() {
    let mut h = Harness::new(FakeScript::default()).await;
    let spec = SandboxSpec {
        level: SandboxLevel::Relaxed,
        grants: Vec::new(),
        domains: Vec::new(),
        mediated: Vec::new(),
    };
    let looser =
        h.rt.create(SessionCreateParams {
            sandbox_level: Some(SandboxLevel::Off),
            sandbox: Some(spec.clone()),
            ..h.create_params(&[(h.devbox, "~/src/app")])
        })
        .await
        .unwrap()
        .session;
    assert_eq!(looser.sandbox.level, SandboxLevel::Off);
    assert!(looser.sandbox.explicit && looser.sandbox.looser_than_default);
    let stricter =
        h.rt.create(SessionCreateParams {
            sandbox_level: Some(SandboxLevel::Strict),
            sandbox: Some(spec),
            ..h.create_params(&[(h.devbox, "~/src/app")])
        })
        .await
        .unwrap()
        .session;
    assert_eq!(stricter.sandbox.level, SandboxLevel::Strict);
    assert!(!stricter.sandbox.looser_than_default);
    let default =
        h.rt.create(h.create_params(&[(h.devbox, "~/src/app")]))
            .await
            .unwrap()
            .session;
    assert_eq!(default.sandbox.level, SandboxLevel::Strict);
    assert!(!default.sandbox.explicit);
    h.wait_state(looser.id, SessionState::Idle).await;
    let h = h.restart().await;
    assert_eq!(h.summary(looser.id).sandbox.level, SandboxLevel::Off);
}
