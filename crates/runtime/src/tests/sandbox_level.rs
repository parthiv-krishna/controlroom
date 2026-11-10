//! `session.set_sandbox_level`: the agent restarts (resumed) at the new level, the workspaces are
//! reopened with it (an offline host when it reconnects), the timeline shows a divider and the
//! agent gets a note in its next turn; `null` goes back to the defaults; op ids dedup; closed
//! sessions refuse it.

use controlroom_drivers::Capabilities;
use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::{SessionCreateParams, SessionSetSandboxLevelParams};
use controlroom_protocol::ids::{OpId, SessionId};
use controlroom_protocol::session::{SandboxLevel, SandboxSpec, SessionState, StartedBy};
use controlroom_protocol::timeline::ItemBody;

use super::host::steered_text;
use super::{Harness, caps};

fn steering() -> Capabilities {
    Capabilities {
        steer: true,
        ..caps()
    }
}

/// "build" and "next" each run until the test releases their gate.
fn script() -> FakeScript {
    FakeScript::default()
        .turn(
            "build",
            [
                FakeStep::Gate("build".into()),
                FakeStep::Text("built".into()),
            ],
        )
        .turn(
            "next",
            [FakeStep::Gate("next".into()), FakeStep::Text("done".into())],
        )
}

fn set(id: SessionId, level: Option<SandboxLevel>) -> SessionSetSandboxLevelParams {
    SessionSetSandboxLevelParams {
        op_id: OpId::generate(),
        id,
        level,
        sandbox: None,
    }
}

fn defaults(level: SandboxLevel) -> SandboxSpec {
    SandboxSpec {
        level,
        grants: Vec::new(),
        domains: Vec::new(),
        mediated: Vec::new(),
        never: Vec::new(),
    }
}

/// The levels of the session's `session_info` dividers, in order.
async fn dividers(h: &Harness, id: SessionId) -> Vec<SandboxLevel> {
    h.timeline(id)
        .await
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::SessionInfo { sandbox, .. } => sandbox,
            _ => None,
        })
        .collect()
}

/// The levels the session's workspaces were (re)opened with on devbox, in order.
fn opened(h: &Harness) -> Vec<SandboxLevel> {
    h.machines
        .opened(h.devbox)
        .into_iter()
        .map(|(_, level)| level)
        .collect()
}

#[tokio::test]
async fn a_new_level_restarts_the_agent_resumed_and_reopens_the_workspaces() {
    let mut h = Harness::with(script(), steering()).await;
    let id = h.create().await;
    assert_eq!(h.host.launch_levels(id), ["strict"]);
    h.prompt(id, "build").await;
    h.wait_state(id, SessionState::Working).await;

    h.rt.set_sandbox_level(set(id, Some(SandboxLevel::Relaxed)))
        .await
        .unwrap();
    // The summary has the new level at once.
    let sandbox = h.summary(id).sandbox;
    assert_eq!(sandbox.level, SandboxLevel::Relaxed);
    assert!(sandbox.explicit && sandbox.looser_than_default);

    // The running turn stops; the agent comes back at the new level, resumed.
    let host = h.host.clone();
    host.wait("the restart", |host| host.launch_levels(id).len() == 2)
        .await;
    assert_eq!(h.host.launch_levels(id), ["strict", "relaxed"]);
    let s = h
        .wait(id, "the resumed agent", |s| {
            s.state == SessionState::Idle && s.agent_live
        })
        .await;
    assert!(s.last_outcome.is_none() && s.last_error.is_some(), "{s:?}");
    let detail = h.detail(id).await;
    assert_eq!(detail.agent_sessions.len(), 2);
    assert_eq!(detail.agent_sessions[1].started_by, StartedBy::Resume);
    assert_eq!(
        detail.agent_sessions[0].native_id,
        detail.agent_sessions[1].native_id
    );

    // The workspace is reopened with the new level.
    let devbox = h.devbox;
    h.machines
        .wait("the reopen", move |m| {
            m.opened(devbox).last().map(|(_, l)| *l) == Some(SandboxLevel::Relaxed)
        })
        .await;
    assert_eq!(dividers(&h, id).await, [SandboxLevel::Relaxed]);

    // The note never reached the old process; it goes into the next turn, without waking it.
    assert!(h.host.steers(id).is_empty());
    assert_eq!(h.host.prompts(id), ["build"]);
    h.prompt(id, "next").await;
    h.wait_command(id, "the note", |c| steered_text(c).is_some())
        .await;
    let note = h.host.steers(id).remove(0);
    assert!(
        note.starts_with("[controlroom] The user changed the sandbox level: it is now relaxed"),
        "{note}"
    );
    assert!(
        note.contains("jobs already running keep the old level"),
        "{note}"
    );
    h.host.release(id, "next");
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn null_goes_back_to_the_defaults_and_an_unchanged_level_restarts_nothing() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id =
        h.rt.create(SessionCreateParams {
            sandbox_level: Some(SandboxLevel::Strict),
            sandbox: Some(defaults(SandboxLevel::Relaxed)),
            ..h.create_params(&[(h.devbox, "~/src/app")])
        })
        .await
        .unwrap()
        .session
        .id;
    h.wait_state(id, SessionState::Idle).await;
    let opens = opened(&h).len();

    // Explicit strict → the defaults (relaxed): a change.
    h.rt.set_sandbox_level(set(id, None)).await.unwrap();
    let sandbox = h.summary(id).sandbox;
    assert_eq!(sandbox.level, SandboxLevel::Relaxed);
    assert!(!sandbox.explicit && !sandbox.looser_than_default);
    let host = h.host.clone();
    host.wait("the restart", |host| host.launch_levels(id).len() == 2)
        .await;
    h.wait(id, "the resumed agent", |s| s.agent_live).await;
    let devbox = h.devbox;
    h.machines
        .wait("the reopen", move |m| m.opened(devbox).len() > opens)
        .await;

    // Choosing the level it already runs at only makes it explicit: no restart, no reopen,
    // no divider.
    let opens = opened(&h).len();
    h.rt.set_sandbox_level(set(id, Some(SandboxLevel::Relaxed)))
        .await
        .unwrap();
    let sandbox = h.summary(id).sandbox;
    assert_eq!(sandbox.level, SandboxLevel::Relaxed);
    assert!(sandbox.explicit);
    h.rt.set_sandbox_level(set(id, None)).await.unwrap();
    assert!(!h.summary(id).sandbox.explicit);
    assert_eq!(h.host.launch_levels(id), ["strict", "relaxed"]);
    assert_eq!(opened(&h).len(), opens);
    assert_eq!(dividers(&h, id).await, [SandboxLevel::Relaxed]);

    // New defaults that change the level count too.
    h.rt.set_sandbox_level(SessionSetSandboxLevelParams {
        sandbox: Some(defaults(SandboxLevel::Off)),
        ..set(id, None)
    })
    .await
    .unwrap();
    assert_eq!(h.summary(id).sandbox.level, SandboxLevel::Off);
    let host = h.host.clone();
    host.wait("the restart", |host| host.launch_levels(id).len() == 3)
        .await;
    assert_eq!(h.host.launch_levels(id)[2], "off");
}

#[tokio::test]
async fn op_ids_dedup_and_closed_sessions_refuse() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let params = set(id, Some(SandboxLevel::Off));
    h.rt.set_sandbox_level(params.clone()).await.unwrap();
    h.rt.set_sandbox_level(params.clone()).await.unwrap();
    let err =
        h.rt.set_sandbox_level(SessionSetSandboxLevelParams {
            level: Some(SandboxLevel::Relaxed),
            ..params
        })
        .await
        .unwrap_err();
    assert_eq!(err.data, Some(ErrorData::OpIdConflict));
    let host = h.host.clone();
    host.wait("the restart", |host| host.launch_levels(id).len() == 2)
        .await;
    h.wait(id, "the resumed agent", |s| s.agent_live).await;
    assert_eq!(h.host.launch_levels(id), ["strict", "off"]);
    assert_eq!(dividers(&h, id).await, [SandboxLevel::Off]);

    let err =
        h.rt.set_sandbox_level(set(id, Some(SandboxLevel::Unknown)))
            .await
            .unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams));

    h.rt.close(h.op(id)).await.unwrap();
    h.wait_state(id, SessionState::Closed).await;
    let err =
        h.rt.set_sandbox_level(set(id, Some(SandboxLevel::Strict)))
            .await
            .unwrap_err();
    assert!(
        matches!(
            err.data,
            Some(ErrorData::InvalidState {
                state: SessionState::Closed
            })
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_stopped_agent_gets_the_level_when_it_starts() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    h.host.kill_agent(id);
    h.wait_state(id, SessionState::Dormant).await;
    h.rt.set_sandbox_level(set(id, Some(SandboxLevel::Relaxed)))
        .await
        .unwrap();
    assert_eq!(h.summary(id).state, SessionState::Dormant);
    assert!(!h.host.supervisor_running(id));
    h.prompt(id, "hello").await;
    h.wait(id, "the run", |s| {
        s.state == SessionState::Idle && s.last_outcome.is_some()
    })
    .await;
    assert_eq!(h.host.launch_levels(id), ["strict", "relaxed"]);
    // Without steering the note goes as the first prompt of the started agent's turn.
    let prompts = h.host.prompts(id);
    assert!(
        prompts.iter().any(|p| p.contains("it is now relaxed")),
        "{prompts:?}"
    );
}

#[tokio::test]
async fn an_offline_host_gets_the_level_when_it_reconnects() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    h.machines.drop_link(devbox);
    let opens = opened(&h).len();
    h.rt.set_sandbox_level(set(id, Some(SandboxLevel::Off)))
        .await
        .unwrap();
    // The session shows it at once, and the agent (on the server) restarts.
    assert_eq!(h.summary(id).sandbox.level, SandboxLevel::Off);
    let host = h.host.clone();
    host.wait("the restart", |host| host.launch_levels(id).len() == 2)
        .await;
    assert_eq!(opened(&h).len(), opens, "the host is away");

    h.machines.reconnect(devbox);
    h.machines
        .wait("the reopen", move |m| m.opened(devbox).len() > opens)
        .await;
    assert!(
        opened(&h)[opens..].iter().all(|l| *l == SandboxLevel::Off),
        "{:?}",
        opened(&h)
    );
}
