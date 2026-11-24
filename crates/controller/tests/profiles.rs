//! Profiles (tenants): the `profiles.*` methods, their feed events and reports to the agent
//! daemon, inferring the only profile, and every membership refusal (C1–C13, design "profiles as
//! tenants" §2). Machines are seeded in the store (they never connect; the refusals come first),
//! and sessions are the fake agent daemon's.
#![cfg(unix)]

mod support;

use controlroom_controller::store::{NewMachineRow, NewProfile, Store};
use controlroom_controller::test_support::{FakeAgentd, UiDriver, op, session_summary};
use controlroom_controller::{Controller, OwnerId};
use controlroom_protocol::agentd::ModelPolicy;
use controlroom_protocol::ids::{MachineId, ProfileId, SessionId};
use controlroom_protocol::session::{SandboxLevel, SessionState, SessionSummary, WorkspaceSpec};
use controlroom_protocol::ui::ContactScope;
use controlroom_protocol::{ErrorData, RpcError};
use serde_json::{Value, json};
use support::ui;

/// A controller on a seeded store, linked to a fake agent daemon.
struct World {
    agentd: FakeAgentd,
    controller: Controller,
    ui: UiDriver,
    _dir: tempfile::TempDir,
}

impl World {
    /// Seeds the store with `seed`, then starts the controller.
    async fn start(seed: impl FnOnce(&Store, &OwnerId)) -> Self {
        let agentd = FakeAgentd::start().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controller.sqlite");
        {
            let store = Store::open(&path).unwrap();
            let owner = OwnerId::local();
            store.ensure_owner(&owner).unwrap();
            seed(&store, &owner);
        }
        let mut config = support::config(&path, None, None);
        config.agentd_socket = Some(agentd.socket().to_owned());
        let controller = Controller::start(config).unwrap();
        let ui = ui(&controller, "laptop").await;
        agentd.wait_connected(1).await;
        Self {
            agentd,
            controller,
            ui,
            _dir: dir,
        }
    }

    /// Publishes a session of `profile` on `machines`, and waits until the UI sees it.
    async fn session(
        &mut self,
        profile: Option<ProfileId>,
        machines: &[MachineId],
        edit: impl FnOnce(&mut SessionSummary),
    ) -> SessionId {
        let id = SessionId::generate();
        let specs: Vec<_> = machines
            .iter()
            .map(|m| WorkspaceSpec {
                machine_id: *m,
                path: "~/src".into(),
            })
            .collect();
        let mut summary = session_summary(id, &specs);
        summary.profile_id = profile;
        edit(&mut summary);
        self.agentd.upsert_session(summary);
        let key = id.to_string();
        self.ui
            .wait("the session arrives", |m| m.sessions.contains_key(&key))
            .await;
        id
    }
}

fn profile(store: &Store, owner: &OwnerId, name: &str, private: bool) -> ProfileId {
    store
        .owner(owner)
        .profiles()
        .insert(NewProfile {
            name: name.into(),
            color: "#0ea5e9".into(),
            private,
            models: if private {
                ModelPolicy::Only { accounts: vec![] }
            } else {
                ModelPolicy::Any
            },
        })
        .unwrap()
        .id
}

/// A machine that never connects (nothing listens on its port).
fn machine(
    store: &Store,
    owner: &OwnerId,
    name: &str,
    port: u16,
    profiles: &[ProfileId],
) -> MachineId {
    store
        .owner(owner)
        .machines()
        .insert(NewMachineRow {
            name: name.into(),
            host: "127.0.0.1".into(),
            port,
            username: "alice".into(),
            profile_ids: profiles.to_vec(),
            key_id: None,
            auth_key_id: None,
            host_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEvYYFqtmyAwG91pMgBKerb/Ly0i8a4OUZAXeWRT7bdw"
                    .into(),
        })
        .unwrap()
        .id
}

fn unsupported() -> RpcError {
    RpcError::new(ErrorData::Unsupported, "not yet")
}

/// Expects `policy_denied` naming `profile`.
async fn denied(ui: &mut UiDriver, method: &str, params: Value, profile: ProfileId) -> Value {
    let error = ui.err(method, params, "policy_denied").await;
    assert_eq!(error["data"]["profile_id"], json!(profile), "{error}");
    error
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profiles_are_created_updated_deleted_and_reported() {
    let mut w = World::start(|_, _| {}).await;
    let ui = &mut w.ui;

    // A fresh install: no profiles, and an empty report.
    let fresh = ui.ok("state.subscribe", json!({"since": null})).await;
    assert_eq!(fresh["snapshot"]["profiles"], json!([]));
    w.agentd
        .wait_profiles("an empty report", |r| r.profiles.is_empty())
        .await;
    for (method, params) in [
        ("projects.create", json!({"name": "seeds", "color": "#000"})),
        ("tasks.create", json!({"title": "water"})),
        ("session.create", json!({"op_id": op(), "workspaces": []})),
    ] {
        let error = ui.err(method, params, "invalid_params").await;
        assert_eq!(error["message"], "create a profile first");
    }

    let garden = ui
        .ok(
            "profiles.create",
            json!({"name": "  Garden club ", "color": "#16a34a"}),
        )
        .await;
    assert_eq!(garden["name"], "Garden club");
    assert_eq!(garden["models"], json!({"kind": "any"}));
    assert_eq!(garden["private"], false);
    let books = ui
        .ok(
            "profiles.create",
            json!({"name": "Book club", "color": "#f59e0b", "private": true}),
        )
        .await;
    assert_eq!(books["models"], json!({"kind": "only", "accounts": []}));
    assert!(books["position"].as_f64() > garden["position"].as_f64());
    let (garden_id, books_id) = (
        garden["id"].as_str().unwrap().to_owned(),
        books["id"].as_str().unwrap().to_owned(),
    );
    ui.wait("both profiles in the feed", |m| m.profiles.len() == 2)
        .await;
    let report = w
        .agentd
        .wait_profiles("both profiles reported", |r| r.profiles.len() == 2)
        .await;
    assert_eq!(report.profiles[0].models, ModelPolicy::Any);
    assert!(report.profiles[1].private);

    // Bad input.
    for params in [
        json!({"name": "Garden club", "color": "#000"}),
        json!({"name": " ", "color": "#000"}),
        json!({"name": "Chess", "color": ""}),
        json!({"name": "Chess", "color": "#000", "private": true, "models": {"kind": "any"}}),
    ] {
        ui.err("profiles.create", params, "invalid_params").await;
    }

    // C9: a private profile lists its accounts; defaults stay within them.
    ui.err(
        "profiles.update",
        json!({"profile_id": books_id, "models": {"kind": "any"}}),
        "invalid_params",
    )
    .await;
    ui.err(
        "profiles.update",
        json!({"profile_id": books_id, "model_defaults": {"provider": "gateway"}}),
        "invalid_params",
    )
    .await;
    let books = ui
        .ok(
            "profiles.update",
            json!({"profile_id": books_id, "name": "Reading circle", "position": -1.0,
                   "models": {"kind": "only", "accounts": ["gateway"]},
                   "model_defaults": {"provider": "gateway", "model": null, "thinking": "low"}}),
        )
        .await;
    assert_eq!(books["model_defaults"]["thinking"], "low");
    ui.wait("the rename in the feed", |m| {
        m.profiles[&books_id]["name"] == "Reading circle"
    })
    .await;
    let list = ui.ok("profiles.list", json!({})).await;
    assert_eq!(list["profiles"][0]["id"], json!(books_id), "by position");
    let report = w
        .agentd
        .wait_profiles("the new policy", |r| {
            r.profiles
                .iter()
                .any(|p| p.models.admits("gateway") && p.private)
        })
        .await;
    assert_eq!(
        report.profiles[0].model_defaults.provider.as_deref(),
        Some("gateway")
    );
    // Turning the private one public with any account works.
    ui.ok(
        "profiles.update",
        json!({"profile_id": books_id, "private": false, "models": {"kind": "any"}}),
    )
    .await;

    // With several profiles the id is required; it must exist.
    ui.err(
        "projects.create",
        json!({"name": "seeds", "color": "#000"}),
        "invalid_params",
    )
    .await;
    let missing = ProfileId::generate();
    ui.err(
        "projects.create",
        json!({"name": "seeds", "color": "#000", "profile_id": missing}),
        "not_found",
    )
    .await;

    // Deleting one leaves the only profile, which is then inferred.
    ui.ok("profiles.delete", json!({"profile_id": books_id}))
        .await;
    ui.wait("the removal in the feed", |m| m.profiles.len() == 1)
        .await;
    w.agentd
        .wait_profiles("the deletion reported", |r| r.profiles.len() == 1)
        .await;
    let project = ui
        .ok("projects.create", json!({"name": "seeds", "color": "#000"}))
        .await;
    assert_eq!(project["profile_id"], json!(garden_id));
    let task = ui.ok("tasks.create", json!({"title": "water"})).await;
    assert_eq!(task["profile_id"], json!(garden_id));
    let task = ui
        .ok(
            "tasks.create",
            json!({"title": "weed", "project_ids": [project["id"]]}),
        )
        .await;
    assert_eq!(task["profile_id"], json!(garden_id));
    ui.wait("projects and tasks carry the profile", |m| {
        m.projects
            .values()
            .all(|p| p["profile_id"] == json!(garden_id))
            && m.tasks.len() == 2
            && m.tasks
                .values()
                .all(|t| t["profile_id"] == json!(garden_id))
    })
    .await;
    w.controller.shutdown().await;
}

/// An agent daemon before profiles answers `unsupported`: controller operations carry on and
/// the report is sent again after the next `hello`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_daemon_without_profiles_gets_the_report_again_on_reconnect() {
    let w = World::start(|_, _| {}).await;
    let mut ui = w.ui;
    w.agentd.refuse("profiles.report", unsupported());
    ui.create_profile("Chess club").await;
    support_wait(&w.agentd, 2).await;
    ui.ok(
        "projects.create",
        json!({"name": "openings", "color": "#000"}),
    )
    .await;

    let before = w.agentd.profile_reports().len();
    w.agentd.accept("profiles.report");
    w.agentd.drop_connection();
    w.agentd.wait_connected(2).await;
    support_wait(&w.agentd, before + 1).await;
    let report = w.agentd.profile_reports().pop().unwrap();
    assert_eq!(report.profiles.len(), 1);
    assert_eq!(report.profiles[0].models, ModelPolicy::Any);
    w.controller.shutdown().await;
}

/// Waits until the agent daemon got `n` profile reports (answered or not).
async fn support_wait(agentd: &FakeAgentd, n: usize) {
    controlroom_controller::test_support::wait_until("profile reports", || {
        agentd.profile_reports().len() >= n
    })
    .await;
}

/// C1–C4, C7, C8, C12: membership refusals.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn membership_is_enforced() {
    let mut ids = None;
    let mut w = World::start(|store, owner| {
        let garden = profile(store, owner, "Garden club", false);
        let side = profile(store, owner, "Side project", false);
        let books = profile(store, owner, "Book club", true);
        let devbox = machine(store, owner, "devbox", 1, &[garden]);
        let buildbox = machine(store, owner, "buildbox", 2, &[garden, side]);
        let reader = machine(store, owner, "reader", 3, &[books]);
        ids = Some((garden, side, books, devbox, buildbox, reader));
    })
    .await;
    let (garden, side, books, devbox, buildbox, reader) = ids.unwrap();
    let machines = w
        .agentd
        .wait_machines("the machines with their profiles", |m| m.len() == 3)
        .await;
    let shared = machines.iter().find(|m| m.machine_id == buildbox).unwrap();
    assert_eq!(shared.profile_ids, [garden, side]);
    assert_eq!(
        w.ui.mirror.machine(&devbox.to_string())["profile_ids"],
        json!([garden])
    );

    // C1: a machine outside the session's profile; machines that don't name one profile.
    let create = |workspaces: &[MachineId], extra: Value| {
        let mut params = json!({
            "op_id": op(),
            "workspaces": workspaces.iter().map(|m| json!({"machine_id": m, "path": "~/src"})).collect::<Vec<_>>(),
        });
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        params
    };
    let error = denied(
        &mut w.ui,
        "session.create",
        create(&[devbox], json!({"profile_id": side})),
        side,
    )
    .await;
    assert_eq!(error["message"], "devbox isn't in Side project");
    w.ui.err(
        "session.create",
        create(&[devbox, reader], json!({})),
        "invalid_params",
    )
    .await;
    w.ui.err(
        "session.create",
        create(&[buildbox], json!({})),
        "invalid_params",
    )
    .await;
    // C8: level off on a shared machine.
    denied(
        &mut w.ui,
        "session.create",
        create(
            &[buildbox],
            json!({"profile_id": garden, "sandbox_level": "off"}),
        ),
        garden,
    )
    .await;
    denied(
        &mut w.ui,
        "sandbox.set_defaults",
        json!({"scope": {"kind": "machine", "machine_id": buildbox},
               "change": {"kind": "set_level", "level": "off"}}),
        garden,
    )
    .await;

    // C3: a task's projects are all in its profile.
    let ui = &mut w.ui;
    let seeds = ui
        .ok(
            "projects.create",
            json!({"name": "seeds", "color": "#000", "profile_id": garden}),
        )
        .await;
    let chess = ui
        .ok(
            "projects.create",
            json!({"name": "chess", "color": "#000", "profile_id": side}),
        )
        .await;
    denied(
        ui,
        "tasks.create",
        json!({"title": "x", "project_ids": [seeds["id"], chess["id"]]}),
        garden,
    )
    .await;
    denied(
        ui,
        "tasks.create",
        json!({"title": "x", "profile_id": side, "project_ids": [seeds["id"]]}),
        side,
    )
    .await;
    let water = ui
        .ok(
            "tasks.create",
            json!({"title": "water", "project_ids": [seeds["id"]]}),
        )
        .await;
    assert_eq!(water["profile_id"], json!(garden));
    denied(
        ui,
        "tasks.update",
        json!({"task_id": water["id"], "project_ids": [chess["id"]]}),
        garden,
    )
    .await;
    let opening = ui
        .ok(
            "tasks.create",
            json!({"title": "opening", "profile_id": side}),
        )
        .await;

    // C4: links stay within the profile.
    let planner = w.session(Some(garden), &[devbox], |_| {}).await;
    let ui = &mut w.ui;
    denied(
        ui,
        "tasks.link",
        json!({"task_id": opening["id"], "session_id": planner}),
        side,
    )
    .await;
    denied(
        ui,
        "tasks.create",
        json!({"title": "x", "profile_id": side, "links": [planner]}),
        side,
    )
    .await;
    denied(
        ui,
        "session.create",
        create(&[devbox], json!({"task_ids": [opening["id"]]})),
        garden,
    )
    .await;
    ui.ok(
        "tasks.link",
        json!({"task_id": water["id"], "session_id": planner}),
    )
    .await;

    // C2: a new workspace on a machine outside the session's profile, or (C8) a shared one
    // while the session runs at level off.
    let ui = &mut w.ui;
    denied(
        ui,
        "session.workspace_add",
        json!({"op_id": op(), "id": planner, "workspace": {"machine_id": reader, "path": "~"}}),
        garden,
    )
    .await;
    let unsandboxed = w
        .session(Some(garden), &[devbox], |s| {
            s.sandbox.level = SandboxLevel::Off
        })
        .await;
    let ui = &mut w.ui;
    denied(
        ui,
        "session.workspace_add",
        json!({"op_id": op(), "id": unsandboxed, "workspace": {"machine_id": buildbox, "path": "~"}}),
        garden,
    )
    .await;
    assert!(w.agentd.calls("session.workspace_add").is_empty());
    let on_shared = w.session(Some(garden), &[buildbox], |_| {}).await;
    let ui = &mut w.ui;
    denied(
        ui,
        "session.set_sandbox_level",
        json!({"op_id": op(), "id": on_shared, "level": "off"}),
        garden,
    )
    .await;

    // C12: a profile's sandbox defaults; they apply to its sessions.
    ui.err(
        "sandbox.set_defaults",
        json!({"scope": {"kind": "profile", "profile_id": ProfileId::generate()},
               "change": {"kind": "set_level", "level": "relaxed"}}),
        "not_found",
    )
    .await;
    let defaults = ui
        .ok(
            "sandbox.set_defaults",
            json!({"scope": {"kind": "profile", "profile_id": garden},
                   "change": {"kind": "set_level", "level": "relaxed"}}),
        )
        .await;
    assert_eq!(
        defaults["defaults"][0]["scope"],
        json!({"kind": "profile", "profile_id": garden})
    );
    ui.ok(
        "session.set_sandbox_level",
        json!({"op_id": op(), "id": planner, "level": null}),
    )
    .await;
    let sent = w.agentd.calls("session.set_sandbox_level").pop().unwrap();
    assert_eq!(sent["sandbox"]["level"], "relaxed");

    // C7: machine profiles: at least one, kept (a move is profiles.move), never a private one
    // shared, and (C8) never shared while it runs at level off.
    let ui = &mut w.ui;
    ui.err(
        "machines.set_settings",
        json!({"machine_id": devbox, "profile_ids": []}),
        "invalid_params",
    )
    .await;
    ui.err(
        "machines.set_settings",
        json!({"machine_id": devbox, "profile_ids": [side]}),
        "invalid_params",
    )
    .await;
    denied(
        ui,
        "machines.set_settings",
        json!({"machine_id": reader, "profile_ids": [books, garden]}),
        books,
    )
    .await;
    denied(
        ui,
        "machines.add",
        json!({"host": "127.0.0.1", "port": 9, "user": "alice", "profile_ids": [garden, books]}),
        books,
    )
    .await;
    denied(
        ui,
        "machines.set_settings",
        json!({"machine_id": devbox, "profile_ids": [garden, side]}),
        garden,
    )
    .await;
    w.agentd.upsert_session({
        let mut s = w.agentd.session(unsandboxed).unwrap();
        s.state = SessionState::Closed;
        s
    });
    let ui = &mut w.ui;
    ui.wait("the unsandboxed session is archived", |m| {
        m.session(&unsandboxed.to_string())["state"] == "closed"
    })
    .await;
    let machine = ui
        .ok(
            "machines.set_settings",
            json!({"machine_id": devbox, "profile_ids": [garden, side]}),
        )
        .await;
    assert_eq!(machine["profile_ids"], json!([garden, side]));
    // A private profile can't have a shared machine.
    denied(
        ui,
        "profiles.update",
        json!({"profile_id": side, "private": true, "models": {"kind": "only", "accounts": []}}),
        side,
    )
    .await;
    w.agentd
        .wait_machines("devbox is shared", |m| {
            m.iter()
                .any(|m| m.machine_id == devbox && m.profile_ids == [garden, side])
        })
        .await;
    w.controller.shutdown().await;
}

/// C13: `profiles.move` (with `dry_run`), and the agent daemon moving the sessions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moving_to_a_profile() {
    let (s1, s2, s3) = (
        SessionId::generate(),
        SessionId::generate(),
        SessionId::generate(),
    );
    let mut ids = None;
    let mut w = World::start(|store, owner| {
        let garden = profile(store, owner, "Garden club", false);
        let side = profile(store, owner, "Side project", false);
        let books = profile(store, owner, "Book club", true);
        let devbox = machine(store, owner, "devbox", 1, &[garden]);
        let laptop = machine(store, owner, "laptop", 2, &[garden]);
        let grant = store
            .owner(owner)
            .grants()
            .insert(s1, s2, ContactScope::Pair, None)
            .unwrap();
        ids = Some((garden, side, books, devbox, laptop, grant.id));
    })
    .await;
    let (garden, side, books, devbox, laptop, grant) = ids.unwrap();
    for (id, machine, state) in [
        (s1, devbox, SessionState::Idle),
        (s2, laptop, SessionState::Idle),
        (s3, devbox, SessionState::Working),
    ] {
        let mut summary = session_summary(
            id,
            &[WorkspaceSpec {
                machine_id: machine,
                path: "~/src".into(),
            }],
        );
        summary.profile_id = Some(garden);
        summary.state = state;
        w.agentd.upsert_session(summary);
    }
    let ui = &mut w.ui;
    ui.wait("three sessions", |m| m.sessions.len() == 3).await;
    let seeds = ui
        .ok(
            "projects.create",
            json!({"name": "seeds", "color": "#000", "profile_id": garden}),
        )
        .await;
    let herbs = ui
        .ok(
            "projects.create",
            json!({"name": "herbs", "color": "#000", "profile_id": garden}),
        )
        .await;
    let water = ui
        .ok(
            "tasks.create",
            json!({"title": "water", "project_ids": [seeds["id"]], "links": [s1, s2]}),
        )
        .await;

    // Refusals: a session whose machine stays out of the target; a running session; a task
    // split across profiles; a private profile sharing a machine; unknown things.
    let error = denied(
        ui,
        "profiles.move",
        json!({"profile_id": side, "session_ids": [s1]}),
        side,
    )
    .await;
    assert!(error["message"].as_str().unwrap().contains("devbox"));
    ui.err(
        "profiles.move",
        json!({"profile_id": side, "machine_ids": [devbox], "session_ids": [s3]}),
        "busy",
    )
    .await;
    let split = ui
        .ok(
            "tasks.create",
            json!({"title": "split", "project_ids": [seeds["id"], herbs["id"]]}),
        )
        .await;
    denied(
        ui,
        "profiles.move",
        json!({"profile_id": side, "project_ids": [seeds["id"]]}),
        side,
    )
    .await;
    ui.ok("tasks.delete", json!({"task_id": split["id"]})).await;
    denied(
        ui,
        "profiles.move",
        json!({"profile_id": books, "machine_ids": [laptop], "share": true}),
        books,
    )
    .await;
    ui.err(
        "profiles.move",
        json!({"profile_id": ProfileId::generate()}),
        "not_found",
    )
    .await;
    ui.err(
        "profiles.move",
        json!({"profile_id": side, "session_ids": [SessionId::generate()]}),
        "unknown_session",
    )
    .await;

    // The preview changes nothing.
    let params = json!({"profile_id": side, "machine_ids": [devbox], "project_ids": [seeds["id"]],
                        "session_ids": [s1]});
    let mut dry = params.clone();
    dry["dry_run"] = json!(true);
    let preview = ui.ok("profiles.move", dry).await;
    let expected = json!({
        "task_ids": [water["id"]],
        "session_ids": [s1],
        "unlinked": [{"task_id": water["id"], "session_id": s2}],
        "revoked_grants": [grant],
        "stranded": [s3],
    });
    assert_eq!(preview, expected);
    let moved = ui.ok("profiles.list", json!({})).await;
    assert_eq!(moved["profiles"].as_array().unwrap().len(), 3);
    assert_eq!(
        ui.mirror.machine(&devbox.to_string())["profile_ids"],
        json!([garden])
    );
    assert!(w.agentd.calls("session.move").is_empty());

    // The move.
    let result = ui.ok("profiles.move", params.clone()).await;
    assert_eq!(result, expected);
    let (devbox_key, seeds_key, water_key, s1_key) = (
        devbox.to_string(),
        seeds["id"].as_str().unwrap().to_owned(),
        water["id"].as_str().unwrap().to_owned(),
        s1.to_string(),
    );
    ui.wait("everything moved", |m| {
        m.machine(&devbox_key)["profile_ids"] == json!([side])
            && m.projects[&seeds_key]["profile_id"] == json!(side)
            && m.tasks[&water_key]["profile_id"] == json!(side)
            && m.tasks[&water_key]["links"] == json!([s1])
            && m.grants.is_empty()
            && m.session(&s1_key)["profile_id"] == json!(side)
    })
    .await;
    let calls = w.agentd.calls("session.move");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (&calls[0]["id"], &calls[0]["profile_id"]),
        (&json!(s1), &json!(side))
    );
    w.agentd
        .wait_machines("devbox moved", |m| {
            m.iter()
                .any(|m| m.machine_id == devbox && m.profile_ids == [side])
        })
        .await;

    // Again: nothing left to move.
    let ui = &mut w.ui;
    let again = ui.ok("profiles.move", params).await;
    assert_eq!(
        again,
        json!({"task_ids": [], "session_ids": [], "unlinked": [], "revoked_grants": [], "stranded": []})
    );

    // Sharing adds the target. An agent daemon that can't move sessions yet doesn't fail it.
    let result = ui
        .ok(
            "profiles.move",
            json!({"profile_id": side, "machine_ids": [laptop], "share": true, "session_ids": [s2]}),
        )
        .await;
    assert_eq!(result["stranded"], json!([]));
    assert_eq!(
        ui.ok("profiles.list", json!({})).await["profiles"][1]["id"],
        json!(side)
    );
    let laptop_key = laptop.to_string();
    ui.wait("laptop is shared", |m| {
        m.machine(&laptop_key)["profile_ids"] == json!([garden, side])
    })
    .await;
    w.agentd.refuse("session.move", unsupported());
    let ui = &mut w.ui;
    ui.ok(
        "profiles.move",
        json!({"profile_id": garden, "session_ids": [s2]}),
    )
    .await;
    w.controller.shutdown().await;
}

/// C10: only an empty profile is deleted, with its archived sessions; deleting the last one
/// returns to the fresh install.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_profile() {
    let mut ids = None;
    let mut w = World::start(|store, owner| {
        let garden = profile(store, owner, "Garden club", false);
        let devbox = machine(store, owner, "devbox", 1, &[garden]);
        ids = Some((garden, devbox));
    })
    .await;
    let (garden, devbox) = ids.unwrap();
    let ui = &mut w.ui;
    let delete = json!({"profile_id": garden});
    let error = ui
        .err("profiles.delete", delete.clone(), "invalid_state")
        .await;
    assert_eq!(
        error["message"],
        "Garden club still has 1 machine; move or delete them first"
    );
    let project = ui
        .ok("projects.create", json!({"name": "seeds", "color": "#000"}))
        .await;
    let task = ui.ok("tasks.create", json!({"title": "water"})).await;
    ui.ok("machines.remove", json!({"machine_id": devbox}))
        .await;
    let error = ui
        .err("profiles.delete", delete.clone(), "invalid_state")
        .await;
    assert_eq!(
        error["message"],
        "Garden club still has 1 project and 1 task; move or delete them first"
    );
    ui.ok("projects.delete", json!({"project_id": project["id"]}))
        .await;
    ui.ok("tasks.delete", json!({"task_id": task["id"]})).await;

    let open = w.session(Some(garden), &[], |_| {}).await;
    let error =
        w.ui.err("profiles.delete", delete.clone(), "invalid_state")
            .await;
    assert_eq!(error["data"]["state"], "idle");
    let mut archived = w.agentd.session(open).unwrap();
    archived.state = SessionState::Closed;
    w.agentd.upsert_session(archived);
    let key = open.to_string();
    let ui = &mut w.ui;
    ui.wait("archived", |m| m.session(&key)["state"] == "closed")
        .await;
    let error = ui.err("profiles.delete", delete, "invalid_state").await;
    assert_eq!(error["data"]["state"], "closed");
    ui.ok(
        "profiles.delete",
        json!({"profile_id": garden, "delete_archived": true}),
    )
    .await;
    let deleted = w.agentd.calls("session.delete");
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0]["id"], json!(open));

    // The fresh-install state again.
    let ui = &mut w.ui;
    ui.wait("no profiles", |m| m.profiles.is_empty()).await;
    let error = ui
        .err(
            "projects.create",
            json!({"name": "seeds", "color": "#000"}),
            "invalid_params",
        )
        .await;
    assert_eq!(error["message"], "create a profile first");
    w.agentd
        .wait_profiles("an empty report", |r| r.profiles.is_empty())
        .await;
    w.controller.shutdown().await;
}
