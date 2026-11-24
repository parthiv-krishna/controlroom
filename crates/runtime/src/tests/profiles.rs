//! Profiles: the reported policies reach every launch (an unreported profile admits nothing)
//! and survive a restart; `session.set_option` refuses a model of another account; a workspace,
//! host call or shell on a machine outside the session's profile is refused; children run in
//! their parent's profile; `session.move` restarts a live agent with the new policy (`busy`
//! while a run is active), and a stopped one gets it at its next start.

use controlroom_drivers::fake::{FakeScript, FakeStep};
use controlroom_protocol::agent::{HostCallParams, ShellNewParams};
use controlroom_protocol::agentd::{
    ModelDefaults, ModelPolicy, OptionValue, ProfilePolicy, SessionCreateParams, SessionMoveParams,
    SessionSetOptionParams, WorkspaceAddParams,
};
use controlroom_protocol::host::HostOp;
use controlroom_protocol::ids::{CallId, MachineId, OpId, ProfileId, RemotePath, SessionId};
use controlroom_protocol::session::{
    OptionChoice, OptionControl, SessionOption, SessionState, StartedBy, WorkspaceSpec,
};
use controlroom_protocol::{ErrorData, RpcError};

use super::Harness;

fn only(accounts: &[&str]) -> ModelPolicy {
    ModelPolicy::Only {
        accounts: accounts.iter().map(|a| (*a).to_owned()).collect(),
    }
}

fn policy(profile_id: ProfileId, models: ModelPolicy) -> ProfilePolicy {
    ProfilePolicy {
        profile_id,
        models,
        model_defaults: ModelDefaults::default(),
        private: false,
    }
}

/// The refusal's profile, if it is `policy_denied`.
fn denied_by(error: &RpcError) -> Option<ProfileId> {
    match &error.data {
        Some(ErrorData::PolicyDenied { profile_id, .. }) => Some(*profile_id),
        _ => None,
    }
}

fn in_profile(
    h: &Harness,
    profile: ProfileId,
    workspaces: &[(MachineId, &str)],
) -> SessionCreateParams {
    SessionCreateParams {
        profile_id: Some(profile),
        ..h.create_params(workspaces)
    }
}

/// Creates a session in `profile` on `devbox:~/src/app` and waits until its agent is idle.
async fn create_in(h: &mut Harness, profile: ProfileId) -> SessionId {
    let params = in_profile(h, profile, &[(h.devbox, "~/src/app")]);
    let id = h.rt.create(params).await.unwrap().session.id;
    h.wait_state(id, SessionState::Idle).await;
    id
}

fn move_to(id: SessionId, profile_id: ProfileId) -> SessionMoveParams {
    SessionMoveParams {
        op_id: OpId::generate(),
        id,
        profile_id,
    }
}

#[tokio::test]
async fn launches_get_their_profiles_policy_and_an_unreported_one_admits_nothing() {
    let mut h = Harness::new(FakeScript::default()).await;
    let (garden, unreported) = (ProfileId::generate(), ProfileId::generate());
    let defaults = ModelDefaults {
        provider: Some("local".into()),
        model: Some("m".into()),
        thinking: None,
    };
    h.rt.profiles_report(vec![ProfilePolicy {
        model_defaults: defaults.clone(),
        ..policy(garden, only(&["local"]))
    }])
    .unwrap();
    h.machines.set_profiles(h.devbox, vec![garden, unreported]);

    let id = create_in(&mut h, garden).await;
    assert_eq!(h.summary(id).profile_id, Some(garden));
    assert_eq!(h.host.launch_models(id), [(only(&["local"]), defaults)]);

    let id = create_in(&mut h, unreported).await;
    assert_eq!(
        h.host.launch_models(id),
        [(only(&[]), ModelDefaults::default())]
    );

    // Once profiles are known, a session needs one.
    let err =
        h.rt.create(h.create_params(&[(h.devbox, "~/src/app")]))
            .await
            .unwrap_err();
    assert_eq!(err.data, Some(ErrorData::InvalidParams), "{err:?}");
}

#[tokio::test]
async fn the_report_survives_a_restart() {
    let h = Harness::new(FakeScript::default()).await;
    let garden = ProfileId::generate();
    h.machines.set_profiles(h.devbox, vec![garden]);
    h.rt.profiles_report(vec![policy(garden, only(&["local"]))])
        .unwrap();
    let mut h = h.restart().await;
    let id = create_in(&mut h, garden).await;
    assert_eq!(h.host.launch_models(id)[0].0, only(&["local"]));

    // A new report replaces the whole previous one.
    let book = ProfileId::generate();
    h.rt.profiles_report(vec![policy(book, ModelPolicy::Any)])
        .unwrap();
    let mut h = h.restart().await;
    h.machines.set_profiles(h.devbox, vec![garden, book]);
    let id = create_in(&mut h, garden).await;
    assert_eq!(h.host.launch_models(id)[0].0, only(&[]));
    let id = create_in(&mut h, book).await;
    assert_eq!(h.host.launch_models(id)[0].0, ModelPolicy::Any);
}

fn models_script() -> FakeScript {
    FakeScript::default().options(vec![SessionOption {
        id: "model".into(),
        label: "Model".into(),
        control: OptionControl::Select {
            value: "local/m".into(),
            choices: ["local/m", "paid/m"]
                .map(|v| OptionChoice {
                    value: v.into(),
                    label: v.into(),
                })
                .to_vec(),
        },
    }])
}

fn set_model(id: SessionId, value: &str) -> SessionSetOptionParams {
    SessionSetOptionParams {
        op_id: OpId::generate(),
        id,
        option_id: "model".into(),
        value: OptionValue::Choice(value.into()),
    }
}

#[tokio::test]
async fn set_option_refuses_a_model_of_another_account_under_the_current_policy() {
    let mut h = Harness::new(models_script()).await;
    let garden = ProfileId::generate();
    h.machines.set_profiles(h.devbox, vec![garden]);
    h.rt.profiles_report(vec![policy(garden, only(&["local"]))])
        .unwrap();
    let id = create_in(&mut h, garden).await;
    h.wait(id, "the options", |s| !s.options.is_empty()).await;

    let err = h.rt.set_option(set_model(id, "paid/m")).await.unwrap_err();
    assert_eq!(denied_by(&err), Some(garden), "{err:?}");
    h.rt.set_option(set_model(id, "local/m")).await.unwrap();

    // A new report applies at once.
    h.rt.profiles_report(vec![policy(garden, ModelPolicy::Any)])
        .unwrap();
    h.rt.set_option(set_model(id, "paid/m")).await.unwrap();
}

fn bash(call: &str) -> HostCallParams {
    HostCallParams {
        call_id: CallId::new(call),
        machine: "devbox".into(),
        op: HostOp::Bash {
            shell: "devbox".into(),
            command: "make".into(),
            timeout_s: None,
        },
    }
}

#[tokio::test]
async fn machines_outside_the_profile_are_refused() {
    let mut h = Harness::new(FakeScript::default()).await;
    let (garden, book) = (ProfileId::generate(), ProfileId::generate());
    let buildbox = h.machines.add("buildbox");
    h.machines.set_profiles(h.devbox, vec![garden]);
    h.machines.set_profiles(buildbox, vec![book]);
    h.rt.profiles_report(vec![
        policy(garden, ModelPolicy::Any),
        policy(book, ModelPolicy::Any),
    ])
    .unwrap();

    // A workspace on another profile's machine: refused before anything opens there (A9).
    let params = in_profile(&h, garden, &[(h.devbox, "~/a"), (buildbox, "~/b")]);
    let err = h.rt.create(params).await.unwrap_err();
    assert_eq!(denied_by(&err), Some(garden), "{err:?}");
    assert!(h.machines.opened(buildbox).is_empty());
    assert!(h.machines.opened(h.devbox).is_empty());

    let id = create_in(&mut h, garden).await;
    let add = WorkspaceAddParams {
        op_id: OpId::generate(),
        id,
        workspace: WorkspaceSpec {
            machine_id: buildbox,
            path: RemotePath::new("~/b"),
        },
    };
    let err = h.rt.workspace_add(add).await.unwrap_err();
    assert_eq!(denied_by(&err), Some(garden), "{err:?}");
    assert!(h.machines.opened(buildbox).is_empty());

    // Host calls go through while devbox is in the profile, and are refused once it moved to
    // another one (A8).
    h.rt.host_call(id, bash("b1")).await.unwrap();
    h.machines.set_profiles(h.devbox, vec![book]);
    let err = h.rt.host_call(id, bash("b2")).await.unwrap_err();
    assert_eq!(denied_by(&err), Some(garden), "{err:?}");
    let shell = ShellNewParams {
        call_id: CallId::new("s1"),
        name: "second".into(),
        machine: "devbox".into(),
        cwd: None,
    };
    let err = h.rt.shell_open(id, shell).await.unwrap_err();
    assert_eq!(denied_by(&err), Some(garden), "{err:?}");
    assert_eq!(h.machines.ran(h.devbox), ["make"]);
}

#[tokio::test]
async fn children_run_in_their_parents_profile() {
    let mut h = Harness::new(FakeScript::default()).await;
    let (garden, book) = (ProfileId::generate(), ProfileId::generate());
    h.machines.set_profiles(h.devbox, vec![garden, book]);
    h.rt.profiles_report(vec![
        policy(garden, only(&["local"])),
        policy(book, ModelPolicy::Any),
    ])
    .unwrap();
    let parent = create_in(&mut h, garden).await;

    let child = SessionCreateParams {
        parent_id: Some(parent),
        ..h.create_params(&[(h.devbox, "~/src/app")])
    };
    let child = h.rt.create(child).await.unwrap().session;
    assert_eq!(child.profile_id, Some(garden));
    h.wait_state(child.id, SessionState::Idle).await;
    assert_eq!(h.host.launch_models(child.id)[0].0, only(&["local"]));

    let elsewhere = SessionCreateParams {
        parent_id: Some(parent),
        ..in_profile(&h, book, &[(h.devbox, "~/src/app")])
    };
    let err = h.rt.create(elsewhere).await.unwrap_err();
    assert_eq!(denied_by(&err), Some(garden), "{err:?}");
}

fn gated() -> FakeScript {
    FakeScript::default().turn(
        "build",
        [
            FakeStep::Gate("build".into()),
            FakeStep::Text("built".into()),
        ],
    )
}

#[tokio::test]
async fn a_move_restarts_a_live_agent_with_the_new_policy() {
    let mut h = Harness::new(gated()).await;
    let (garden, book) = (ProfileId::generate(), ProfileId::generate());
    h.machines.set_profiles(h.devbox, vec![garden, book]);
    h.rt.profiles_report(vec![
        policy(garden, only(&["local"])),
        policy(book, ModelPolicy::Any),
    ])
    .unwrap();
    let id = create_in(&mut h, garden).await;

    // Not while a run is active.
    h.prompt(id, "build").await;
    h.wait_state(id, SessionState::Working).await;
    let err = h.rt.move_session(move_to(id, book)).await.unwrap_err();
    assert_eq!(err.data, Some(ErrorData::Busy), "{err:?}");
    assert_eq!(h.summary(id).profile_id, Some(garden));
    h.host.release(id, "build");
    h.wait_state(id, SessionState::Idle).await;

    // Idle: the summary has the new profile at once; the agent comes back resumed under it.
    let params = move_to(id, book);
    h.rt.move_session(params.clone()).await.unwrap();
    h.rt.move_session(params).await.unwrap();
    assert_eq!(h.summary(id).profile_id, Some(book));
    let host = h.host.clone();
    host.wait("the restart", |host| host.launch_models(id).len() == 2)
        .await;
    assert_eq!(h.host.launch_models(id)[1].0, ModelPolicy::Any);
    h.wait(id, "the resumed agent", |s| {
        s.state == SessionState::Idle && s.agent_live
    })
    .await;
    let detail = h.detail(id).await;
    assert_eq!(detail.agent_sessions.len(), 2);
    assert_eq!(detail.agent_sessions[1].started_by, StartedBy::Resume);

    // Moving to the profile it is in changes nothing.
    h.rt.move_session(move_to(id, book)).await.unwrap();

    // A closed session moves without a start; it gets the policy when it next starts.
    h.rt.close(h.op(id)).await.unwrap();
    h.wait_state(id, SessionState::Closed).await;
    h.host
        .wait("the agent to stop", |host| !host.supervisor_running(id))
        .await;
    h.rt.move_session(move_to(id, garden)).await.unwrap();
    assert_eq!(h.summary(id).profile_id, Some(garden));
    assert_eq!(h.host.launch_models(id).len(), 2);
    h.rt.reopen(h.op(id)).await.unwrap();
    h.wait_state(id, SessionState::Dormant).await;
    h.prompt(id, "hello").await;
    let host = h.host.clone();
    host.wait("the next start", |host| host.launch_models(id).len() == 3)
        .await;
    assert_eq!(h.host.launch_models(id)[2].0, only(&["local"]));
}
