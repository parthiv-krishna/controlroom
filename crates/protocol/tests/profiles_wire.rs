//! Wire-shape tests for profiles (tenants): the UI and agent daemon methods, the new fields on
//! machines, projects, tasks, sessions and host machines, the state-feed events, the sandbox
//! defaults scope, `policy_denied` and the push payload. Golden JSON, `unknown` fallbacks, and
//! JSON from peers before profiles (without the new fields).
//!
//! Profile names are arbitrary on purpose: none is built in or special-cased.

use controlroom_protocol::agentd::{
    self, AgentdRequest, HostMachine, ModelDefaults, ModelPolicy, ProfilePolicy,
    ProfilesReportParams, SessionMoveParams,
};
use controlroom_protocol::ids::{
    MachineId, OpId, ProfileId, ProjectId, SessionId, TaskId, UnixMillis,
};
use controlroom_protocol::ui::{
    self, DefaultsScope, Machine, NewMachine, Profile, ProfilesCreateParams, ProfilesDeleteParams,
    ProfilesListResult, ProfilesMoveParams, ProfilesMoveResult, ProfilesUpdateParams, Project,
    PushContent, PushPayload, PushState, StateChange, StateEvent, StateSnapshot, Task, TaskLink,
    UiRequest,
};
use controlroom_protocol::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Asserts `value` serializes to exactly `golden` and parses back to itself.
fn golden<T>(value: &T, golden: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).unwrap();
    assert_eq!(json, golden, "wire shape");
    let back: T = serde_json::from_value(json).unwrap();
    assert_eq!(&back, value, "round-trip");
}

const PRID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3F1";
const OTHER: &str = "01HZY3K6W8Q9X2V4N7B5C1D3F2";
const MID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3E7";
const PID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3EE";
const TID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3F3";
const SID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3E6";
const GID: &str = "01HZY3K6W8Q9X2V4N7B5C1D3F4";

fn prid() -> ProfileId {
    PRID.parse().unwrap()
}

fn profile() -> Profile {
    Profile {
        id: prid(),
        name: "Garden club".into(),
        color: "#0ea5e9".into(),
        private: true,
        models: ModelPolicy::Only {
            accounts: vec!["local-gateway".into()],
        },
        model_defaults: ModelDefaults {
            provider: Some("local-gateway".into()),
            model: None,
            thinking: None,
        },
        position: 1.5,
        created_at: UnixMillis(1_000),
    }
}

fn profile_json() -> Value {
    json!({
        "id": PRID,
        "name": "Garden club",
        "color": "#0ea5e9",
        "private": true,
        "models": {"kind": "only", "accounts": ["local-gateway"]},
        "model_defaults": {"provider": "local-gateway", "model": null, "thinking": null},
        "position": 1.5,
        "created_at": 1000
    })
}

#[test]
fn golden_profile_and_model_policy() {
    golden(&profile(), profile_json());
    golden(&ModelPolicy::Any, json!({"kind": "any"}));
    // Model defaults may be absent: the server's apply.
    let mut without = profile_json();
    without.as_object_mut().unwrap().remove("model_defaults");
    let decoded: Profile = serde_json::from_value(without).unwrap();
    assert_eq!(decoded.model_defaults, ModelDefaults::default());
}

#[test]
fn model_policies_admit_their_accounts_and_unknown_ones_fail_closed() {
    let only = ModelPolicy::Only {
        accounts: vec!["acct-a".into()],
    };
    assert!(only.admits("acct-a"));
    assert!(!only.admits("acct-b"));
    assert!(ModelPolicy::Any.admits("anything"));
    let newer: ModelPolicy =
        serde_json::from_value(json!({"kind": "per_provider", "providers": ["x"]})).unwrap();
    assert_eq!(newer, ModelPolicy::Unknown);
    assert!(!newer.admits("x"));
}

#[test]
fn golden_profiles_methods() {
    let call = |method: &str, params: Value| UiRequest::from_parts(method, params).unwrap();

    assert_eq!(call("profiles.list", json!({})).method(), "profiles.list");
    golden(
        &ProfilesListResult {
            profiles: vec![profile()],
        },
        json!({"profiles": [profile_json()]}),
    );

    // Create: only a name and a color are required.
    let minimal = ProfilesCreateParams {
        name: "Side project".into(),
        color: "#22c55e".into(),
        private: false,
        models: None,
    };
    golden(
        &minimal,
        json!({"name": "Side project", "color": "#22c55e"}),
    );
    golden(
        &ProfilesCreateParams {
            private: true,
            models: Some(ModelPolicy::Only {
                accounts: vec!["acct-a".into()],
            }),
            ..minimal
        },
        json!({"name": "Side project", "color": "#22c55e", "private": true,
               "models": {"kind": "only", "accounts": ["acct-a"]}}),
    );
    assert!(matches!(
        call("profiles.create", json!({"name": "x", "color": "#000"})),
        UiRequest::ProfilesCreate(_)
    ));

    // Update: absent fields are left unchanged.
    let update: ProfilesUpdateParams =
        serde_json::from_value(json!({"profile_id": PRID, "models": {"kind": "any"}})).unwrap();
    assert_eq!(update.models, Some(ModelPolicy::Any));
    assert_eq!(
        (update.name, update.private, update.position),
        (None, None, None)
    );
    golden(
        &ProfilesUpdateParams {
            profile_id: prid(),
            name: Some("Renamed".into()),
            color: None,
            private: Some(false),
            models: None,
            model_defaults: Some(ModelDefaults::default()),
            position: Some(0.5),
        },
        json!({"profile_id": PRID, "name": "Renamed", "private": false,
               "model_defaults": {"provider": null, "model": null, "thinking": null},
               "position": 0.5}),
    );

    // Delete: `delete_archived` defaults to false.
    golden(
        &ProfilesDeleteParams {
            profile_id: prid(),
            delete_archived: false,
        },
        json!({"profile_id": PRID}),
    );
    golden(
        &ProfilesDeleteParams {
            profile_id: prid(),
            delete_archived: true,
        },
        json!({"profile_id": PRID, "delete_archived": true}),
    );

    // Move: empty lists and false flags are omitted.
    golden(
        &ProfilesMoveParams {
            profile_id: prid(),
            machine_ids: vec![MID.parse().unwrap()],
            project_ids: vec![PID.parse().unwrap()],
            session_ids: vec![],
            share: true,
            dry_run: true,
        },
        json!({"profile_id": PRID, "machine_ids": [MID], "project_ids": [PID],
               "share": true, "dry_run": true}),
    );
    let only_sessions: ProfilesMoveParams =
        serde_json::from_value(json!({"profile_id": PRID, "session_ids": [SID]})).unwrap();
    assert!(!only_sessions.share && !only_sessions.dry_run);
    assert!(only_sessions.machine_ids.is_empty() && only_sessions.project_ids.is_empty());
    golden(
        &ProfilesMoveResult {
            task_ids: vec![TID.parse::<TaskId>().unwrap()],
            session_ids: vec![SID.parse::<SessionId>().unwrap()],
            unlinked: vec![TaskLink {
                task_id: TID.parse().unwrap(),
                session_id: SID.parse().unwrap(),
            }],
            revoked_grants: vec![GID.parse().unwrap()],
            stranded: vec![],
        },
        json!({"task_ids": [TID], "session_ids": [SID],
               "unlinked": [{"task_id": TID, "session_id": SID}],
               "revoked_grants": [GID], "stranded": []}),
    );
    for method in [
        "profiles.list",
        "profiles.create",
        "profiles.update",
        "profiles.delete",
        "profiles.move",
    ] {
        assert!(UiRequest::METHODS.contains(&method), "{method}");
    }
}

#[test]
fn golden_profile_feed_events_and_snapshot() {
    golden(
        &StateChange {
            rev: Rev(7),
            event: StateEvent::ProfileUpserted { profile: profile() },
        },
        json!({"rev": 7, "kind": "profile_upserted", "profile": profile_json()}),
    );
    golden(
        &StateChange {
            rev: Rev(8),
            event: StateEvent::ProfileRemoved { profile_id: prid() },
        },
        json!({"rev": 8, "kind": "profile_removed", "profile_id": PRID}),
    );
    // A snapshot from a controller before profiles has none.
    let older: StateSnapshot = serde_json::from_value(json!({
        "machines": [], "sessions": [], "overlaps": [], "projects": [], "tasks": [],
        "grants": [], "schedules": [], "devices": []
    }))
    .unwrap();
    assert!(older.profiles.is_empty());
}

fn machine_json() -> Value {
    json!({
        "id": MID, "name": "devbox", "host": "devbox.example.com", "port": 22, "user": "alice",
        "key_id": null, "host_key": null, "daemon_id": null,
        "connectivity": {"state": "connected"}, "update_available": false,
        "auto_upgrade": true, "grace_s": 600, "last_seen": null, "version": null, "arch": null,
        "last_error": null, "capabilities": [], "doctor": null, "bootstrap": null
    })
}

#[test]
fn machines_projects_and_tasks_carry_their_profile() {
    // A shared machine entry names several profiles.
    let mut shared = machine_json();
    shared["profile_ids"] = json!([PRID, OTHER]);
    let machine: Machine = serde_json::from_value(shared.clone()).unwrap();
    assert_eq!(machine.profile_ids.len(), 2);
    assert_eq!(serde_json::to_value(&machine).unwrap(), shared);
    // Before profiles: no field, no profiles.
    let older: Machine = serde_json::from_value(machine_json()).unwrap();
    assert!(older.profile_ids.is_empty());

    golden(
        &NewMachine {
            host: "devbox.example.com".into(),
            port: 22,
            user: "alice".into(),
            name: None,
            key_id: None,
            profile_ids: vec![prid()],
        },
        json!({"host": "devbox.example.com", "port": 22, "user": "alice",
               "profile_ids": [PRID]}),
    );
    let settings: ui::MachinesSetSettingsParams =
        serde_json::from_value(json!({"machine_id": MID, "profile_ids": [PRID, OTHER]})).unwrap();
    assert_eq!(settings.profile_ids.map(|p| p.len()), Some(2));

    golden(
        &Project {
            id: PID.parse::<ProjectId>().unwrap(),
            name: "Roses".into(),
            color: "#f43f5e".into(),
            profile_id: Some(prid()),
        },
        json!({"id": PID, "name": "Roses", "color": "#f43f5e", "profile_id": PRID}),
    );
    let older: Project =
        serde_json::from_value(json!({"id": PID, "name": "Roses", "color": "#f43f5e"})).unwrap();
    assert_eq!(older.profile_id, None);
    let create: ui::ProjectsCreateParams =
        serde_json::from_value(json!({"name": "Roses", "color": "#f43f5e"})).unwrap();
    assert_eq!(create.profile_id, None);

    let task = json!({
        "id": TID, "title": "prune", "status": "todo", "order": 1.0, "priority": "p2",
        "project_ids": [], "notes": "", "checklist": [], "links": [],
        "created_at": 1, "updated_at": 2
    });
    let older: Task = serde_json::from_value(task.clone()).unwrap();
    assert_eq!(older.profile_id, None);
    let mut with = task;
    with["profile_id"] = json!(PRID);
    let decoded: Task = serde_json::from_value(with.clone()).unwrap();
    assert_eq!(decoded.profile_id, Some(prid()));
    assert_eq!(serde_json::to_value(&decoded).unwrap(), with);
    let create: ui::TasksCreateParams =
        serde_json::from_value(json!({"title": "water", "profile_id": PRID})).unwrap();
    assert_eq!(create.profile_id, Some(prid()));
}

#[test]
fn session_create_carries_an_optional_profile() {
    let ui: ui::UiSessionCreateParams = serde_json::from_value(json!({
        "op_id": "op-1",
        "workspaces": [{"machine_id": MID, "path": "~/garden"}],
        "profile_id": PRID,
        "task_ids": [TID]
    }))
    .unwrap();
    assert_eq!(ui.create.profile_id, Some(prid()));
    let without: agentd::SessionCreateParams = serde_json::from_value(json!({
        "op_id": "op-2",
        "workspaces": [{"machine_id": MID, "path": "~/garden"}]
    }))
    .unwrap();
    assert_eq!(without.profile_id, None);
    assert!(
        !serde_json::to_value(&without)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("profile_id")
    );
}

#[test]
fn golden_agentd_profiles_report_session_move_and_machines() {
    golden(
        &ProfilesReportParams {
            profiles: vec![
                ProfilePolicy {
                    profile_id: prid(),
                    models: ModelPolicy::Only { accounts: vec![] },
                    model_defaults: ModelDefaults::default(),
                    private: true,
                },
                ProfilePolicy {
                    profile_id: OTHER.parse().unwrap(),
                    models: ModelPolicy::Any,
                    model_defaults: ModelDefaults::default(),
                    private: false,
                },
            ],
        },
        json!({"profiles": [
            {"profile_id": PRID, "models": {"kind": "only", "accounts": []},
             "model_defaults": {"provider": null, "model": null, "thinking": null},
             "private": true},
            {"profile_id": OTHER, "models": {"kind": "any"},
             "model_defaults": {"provider": null, "model": null, "thinking": null},
             "private": false}
        ]}),
    );
    // A policy from a newer controller still decodes, and fails closed.
    let newer: ProfilesReportParams = serde_json::from_value(json!({"profiles": [
        {"profile_id": PRID, "models": {"kind": "budgeted", "usd": 5}}
    ]}))
    .unwrap();
    assert_eq!(newer.profiles[0].models, ModelPolicy::Unknown);
    assert!(!newer.profiles[0].private);

    golden(
        &SessionMoveParams {
            op_id: OpId::new("move-1"),
            id: SID.parse().unwrap(),
            profile_id: prid(),
        },
        json!({"op_id": "move-1", "id": SID, "profile_id": PRID}),
    );
    for (method, params) in [
        ("profiles.report", json!({"profiles": []})),
        (
            "session.move",
            json!({"op_id": "m", "id": SID, "profile_id": PRID}),
        ),
    ] {
        assert_eq!(
            AgentdRequest::from_parts(method, params).unwrap().method(),
            method
        );
    }

    let older: HostMachine = serde_json::from_value(json!({
        "machine_id": MID, "name": "devbox",
        "availability": {"connectivity": {"state": "connected"}, "since": 1},
        "epoch": null, "grace_s": 600
    }))
    .unwrap();
    assert!(older.profile_ids.is_empty());
    let _: MachineId = older.machine_id;
}

#[test]
fn golden_profile_sandbox_scope() {
    golden(
        &DefaultsScope::Profile { profile_id: prid() },
        json!({"kind": "profile", "profile_id": PRID}),
    );
    let newer: DefaultsScope =
        serde_json::from_value(json!({"kind": "team", "team_id": "x"})).unwrap();
    assert_eq!(newer, DefaultsScope::Unknown);
}

#[test]
fn golden_policy_denied() {
    let err = RpcError::new(
        ErrorData::PolicyDenied {
            profile_id: prid(),
            reason: "Not available in Garden club".into(),
        },
        "Not available in Garden club",
    );
    golden(
        &err,
        json!({"code": -32024, "message": "Not available in Garden club",
               "data": {"kind": "policy_denied", "profile_id": PRID,
                        "reason": "Not available in Garden club"}}),
    );
    golden(
        &ErrorData::NotFound {
            resource: Resource::Profile,
            id: PRID.into(),
        },
        json!({"kind": "not_found", "resource": "profile", "id": PRID}),
    );
}

#[test]
fn golden_push_payloads() {
    golden(
        &PushPayload {
            session_id: SID.parse().unwrap(),
            profile_id: Some(prid()),
            state: PushState::Approval,
            url: format!("#/s/{SID}"),
            content: PushContent::Session {
                name: "fix flaky test".into(),
                machine: "devbox".into(),
            },
        },
        json!({"session_id": SID, "profile_id": PRID, "state": "approval",
               "url": format!("#/s/{SID}"), "kind": "session",
               "name": "fix flaky test", "machine": "devbox"}),
    );
    // A private profile's push: metadata only (no session name, machine or text).
    let private = PushPayload {
        session_id: SID.parse().unwrap(),
        profile_id: Some(prid()),
        state: PushState::Finished,
        url: format!("#/s/{SID}"),
        content: PushContent::Private {
            profile: "Garden club".into(),
        },
    };
    golden(
        &private,
        json!({"session_id": SID, "profile_id": PRID, "state": "finished",
               "url": format!("#/s/{SID}"), "kind": "private", "profile": "Garden club"}),
    );
    let text = serde_json::to_string(&private).unwrap();
    assert!(
        !text.contains("name") && !text.contains("machine"),
        "{text}"
    );
    // Newer states and content kinds decode as unknown.
    let newer: PushPayload = serde_json::from_value(json!({
        "session_id": SID, "state": "budget", "url": "#/", "kind": "digest", "count": 3
    }))
    .unwrap();
    assert_eq!(
        (newer.state, newer.content, newer.profile_id),
        (PushState::Unknown, PushContent::Unknown, None)
    );
}
