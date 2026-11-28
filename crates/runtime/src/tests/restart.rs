//! `session.restart`: a live agent relaunches resumed with a fresh launch (the profile's current
//! policy), a running run stops with it, a stopped agent starts, op ids dedup, and it is refused
//! while the agent is starting and for closed sessions. `stale_credentials`: set when a model
//! request failed at the connection level after the credentials of the session's provider or
//! its profile's policy changed, and only then; a restart clears it.

use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::ErrorData;
use controlroom_protocol::agentd::{
    ModelDefaults, ModelPolicy, ProfilePolicy, SessionCreateParams, SessionRestartParams,
};
use controlroom_protocol::ids::{OpId, ProfileId, SessionId};
use controlroom_protocol::session::{
    OptionChoice, OptionControl, SessionOption, SessionState, StartedBy,
};

use super::Harness;

fn restart(id: SessionId) -> SessionRestartParams {
    SessionRestartParams {
        op_id: OpId::generate(),
        id,
        sandbox: None,
    }
}

fn only(profile_id: ProfileId, accounts: &[&str]) -> ProfilePolicy {
    ProfilePolicy {
        profile_id,
        models: ModelPolicy::Only {
            accounts: accounts.iter().map(|a| (*a).to_owned()).collect(),
        },
        model_defaults: ModelDefaults::default(),
        private: false,
    }
}

/// An agent on `anthropic/m` whose "go" fails with `error`, and whose "build" runs until its
/// gate is released.
fn script(error: &str) -> FakeScript {
    FakeScript::default()
        .options(vec![SessionOption {
            id: "model".into(),
            label: "Model".into(),
            control: OptionControl::Select {
                value: "anthropic/m".into(),
                choices: vec![OptionChoice {
                    value: "anthropic/m".into(),
                    label: "m".into(),
                }],
            },
        }])
        .turn("go", [FakeStep::Fail(error.into())])
        .turn("fine", [FakeStep::Text("ok".into())])
        .turn(
            "build",
            [
                FakeStep::Gate("build".into()),
                FakeStep::Text("built".into()),
            ],
        )
}

/// Creates a session in `profile` and waits until its agent is idle with its options.
async fn create_in(h: &mut Harness, profile: ProfileId) -> SessionId {
    h.machines.set_profiles(h.devbox, vec![profile]);
    let params = SessionCreateParams {
        profile_id: Some(profile),
        ..h.create_params(&[(h.devbox, "~/src/app")])
    };
    let id = h.rt.create(params).await.unwrap().session.id;
    h.wait(id, "the idle agent", |s| {
        s.state == SessionState::Idle && !s.options.is_empty()
    })
    .await;
    id
}

/// Waits for the `n`th launch and the agent it brings up.
async fn relaunched(h: &mut Harness, id: SessionId, n: usize) {
    let host = h.host.clone();
    host.wait("the relaunch", |host| host.launch_models(id).len() == n)
        .await;
    h.wait(id, "the resumed agent", |s| s.agent_live).await;
}

/// Runs a turn and waits until it has settled.
async fn run(h: &mut Harness, id: SessionId, prompt: &str) {
    let ends = h.summary(id).last_turn_end_seq;
    h.prompt(id, prompt).await;
    h.wait(id, "the turn's end", |s| {
        s.last_turn_end_seq > ends && s.state != SessionState::Working
    })
    .await;
}

#[tokio::test]
async fn a_live_agent_restarts_resumed_with_a_fresh_launch() {
    let mut h = Harness::new(script("x")).await;
    let garden = ProfileId::generate();
    h.rt.profiles_report(vec![only(garden, &["local"])])
        .unwrap();
    let id = create_in(&mut h, garden).await;

    // A policy reported after the launch reaches the agent only with a restart.
    h.rt.profiles_report(vec![only(garden, &["local", "anthropic"])])
        .unwrap();
    let params = restart(id);
    h.rt.restart(params.clone()).await.unwrap();
    h.rt.restart(params).await.unwrap();
    relaunched(&mut h, id, 2).await;
    let launches = h.host.launch_models(id);
    assert_eq!(launches.len(), 2, "the op id dedups");
    assert_eq!(launches[1].0, only(garden, &["local", "anthropic"]).models);
    let detail = h.detail(id).await;
    assert_eq!(detail.agent_sessions.len(), 2);
    assert_eq!(detail.agent_sessions[1].started_by, StartedBy::Resume);
    assert_eq!(
        detail.agent_sessions[0].native_id,
        detail.agent_sessions[1].native_id
    );
}

#[tokio::test]
async fn a_running_run_stops_and_a_stopped_agent_starts() {
    let mut h = Harness::new(script("x")).await;
    let garden = ProfileId::generate();
    h.rt.profiles_report(vec![only(garden, &["local"])])
        .unwrap();
    let id = create_in(&mut h, garden).await;

    h.prompt(id, "build").await;
    h.wait_state(id, SessionState::Working).await;
    h.rt.restart(restart(id)).await.unwrap();
    relaunched(&mut h, id, 2).await;
    let s = h.summary(id);
    assert!(s.last_error.is_some(), "the run was cut off: {s:?}");

    h.host.kill_agent(id);
    h.wait_state(id, SessionState::Dormant).await;
    h.rt.restart(restart(id)).await.unwrap();
    relaunched(&mut h, id, 3).await;
    assert_eq!(
        h.detail(id).await.agent_sessions.last().unwrap().started_by,
        StartedBy::Resume
    );
}

#[tokio::test]
async fn refused_while_starting_and_when_closed() {
    let mut h = Harness::new(script("x")).await;
    let garden = ProfileId::generate();
    h.rt.profiles_report(vec![only(garden, &["local"])])
        .unwrap();
    h.machines.set_profiles(h.devbox, vec![garden]);
    let params = SessionCreateParams {
        profile_id: Some(garden),
        ..h.create_params(&[(h.devbox, "~/src/app")])
    };
    let id = h.rt.create(params).await.unwrap().session.id;
    // The agent never hears from the daemon: it stays starting.
    h.host.hold_writes(id, true);
    let err = h.rt.restart(restart(id)).await.unwrap_err();
    assert_eq!(err.data, Some(ErrorData::Busy), "{err:?}");
    h.host.release_writes(id);
    h.wait_state(id, SessionState::Idle).await;

    h.rt.close(h.op(id)).await.unwrap();
    h.wait_state(id, SessionState::Closed).await;
    let err = h.rt.restart(restart(id)).await.unwrap_err();
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
async fn a_connection_failure_after_a_credential_change_is_stale_until_a_restart() {
    let mut h = Harness::new(script("Connection error.")).await;
    let garden = ProfileId::generate();
    h.rt.profiles_report(vec![only(garden, &["anthropic"])])
        .unwrap();
    let id = create_in(&mut h, garden).await;

    // A failure with nothing changed since the launch: quiet.
    run(&mut h, id, "go").await;
    assert!(!h.summary(id).stale_credentials);
    // Another provider's change: quiet.
    h.clock.advance(1_000);
    h.rt.credentials_changed(vec!["openai".into()]).unwrap();
    assert!(!h.summary(id).stale_credentials);

    // The session's provider changed after the launch, and its requests failed: stale.
    h.rt.credentials_changed(vec!["anthropic".into()]).unwrap();
    h.wait(id, "the stale flag", |s| s.stale_credentials).await;
    // A request that gets through clears it.
    run(&mut h, id, "fine").await;
    assert!(!h.summary(id).stale_credentials);
    run(&mut h, id, "go").await;
    assert!(h.summary(id).stale_credentials);

    // The restart's launch has the current credentials.
    h.clock.advance(1_000);
    h.rt.restart(restart(id)).await.unwrap();
    relaunched(&mut h, id, 2).await;
    assert!(!h.summary(id).stale_credentials);
    run(&mut h, id, "go").await;
    assert!(!h.summary(id).stale_credentials, "nothing changed since");

    // So does a change of the profile's policy, and it survives a restart of the daemon.
    h.clock.advance(1_000);
    h.rt.profiles_report(vec![only(garden, &["anthropic", "openai"])])
        .unwrap();
    assert!(h.summary(id).stale_credentials);
    let mut h = h.restart().await;
    h.wait(id, "the stale flag after a restart", |s| {
        s.stale_credentials
    })
    .await;
}

#[tokio::test]
async fn other_failures_are_never_stale() {
    let mut h = Harness::new(script("401 invalid x-api-key")).await;
    let garden = ProfileId::generate();
    h.rt.profiles_report(vec![only(garden, &["anthropic"])])
        .unwrap();
    let id = create_in(&mut h, garden).await;
    h.clock.advance(1_000);
    h.rt.credentials_changed(vec!["anthropic".into()]).unwrap();
    run(&mut h, id, "go").await;
    assert!(!h.summary(id).stale_credentials);
}
