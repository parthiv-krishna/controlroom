//! Two profiles across the real stack (design `docs/design/profiles-tenants.md` §2, §6, §9):
//! the controller, the real agent daemon running **real pi** (the `controlroom-pi` bundle) on
//! two mock models, `local` and `paid`, and two real host daemons. "Garden club" owns devbox and
//! may use any account; "Book club" is private, owns buildbox and lists `local` only. The
//! server's default provider is `paid`.
//!
//! 1. A session can't use the other profile's machine: the controller refuses the create and
//!    the workspace, and a private profile's machine can't be shared.
//! 2. The private session launches without `paid` (pi doesn't list it, and the server's default
//!    is dropped), can't switch to it, its profile can't take `any` or a `paid` default, and its
//!    push is metadata only (the mock push service).
//! 3. A live agent whose profile just dropped `paid` can't switch to it (the agent daemon
//!    refuses what pi still offers); task links and tasks across profiles are refused.
//! 4. Once devbox moves to Book club, the agent daemon refuses Garden club's host calls there.
//! 5. `profiles.move` moves the session along: its task link is removed, its live agent
//!    restarts with Book club's accounts, devbox works again and its push turns private.
//! 6. `profiles.delete` refuses a profile with machines, projects, tasks or archived sessions,
//!    then deletes the archived ones with it.
//!
//! The `paid` mock is never called.
//!
//! pi runs under the harness's stand-in nono, which runs the command unconfined; the command
//! sets the variables nono's profile would (`HOME`, `TMPDIR`, `PI_CODING_AGENT_DIR`: the
//! session's own agent dir, built from the profile's accounts). The mocks don't check keys.
//!
//! Needs the bundle, so it is `#[ignore]`d; the flake check `e2e-profiles` runs it with
//! `CR_TEST_PI_BUNDLE` set (or: `nix build -o /tmp/pi .#controlroom-pi`, `cargo build
//! --workspace --bins` and `CR_TEST_PI_BUNDLE=/tmp/pi cargo test -p controlroom-e2e --test
//! profiles -- --ignored`). Every process is killed when the world drops, pass or fail.

use std::path::{Path, PathBuf};

use controlroom_controller::test_support::op;
use controlroom_e2e::agentd::Agentd;
use controlroom_e2e::world::{Which, World};
use controlroom_fixture_tool::mock_model::{MockModel, Reply};
use serde_json::{Value, json};

/// The ids every phase uses.
struct Ids {
    garden: String,
    book: String,
    devbox: String,
    buildbox: String,
}

/// The agent daemon's pi config (`local` and `paid`, one model `m` each, `paid` the default)
/// and the agent command: the bundle's launcher on the session's own dirs.
fn configure_server(agentd: &Agentd, bundle: &Path, local: &MockModel, paid: &MockModel) {
    let agent_dir = agentd.state().join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let provider = |mock: &MockModel| {
        json!({"baseUrl": mock.base_url(), "api": "openai-completions", "apiKey": "sk-fake-test-key",
               "models": [{"id": "m", "name": "Mock", "input": ["text"], "contextWindow": 128000,
                           "maxTokens": 4096,
                           "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}]})
    };
    std::fs::write(
        agent_dir.join("models.json"),
        json!({"providers": {"local": provider(local), "paid": provider(paid)}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("settings.json"),
        json!({"defaultProvider": "paid", "defaultModel": "m", "quietStartup": true}).to_string(),
    )
    .unwrap();
    let sessions = agentd.state().join("sessions");
    let script = format!(
        r#"root={sessions}/$CONTROLROOM_SESSION_ID
HOME=$root/home TMPDIR=$root/tmp PI_CODING_AGENT_DIR=$root/agent PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0 exec {launcher} -- "$@""#,
        sessions = sessions.display(),
        launcher = bundle.join("bin/controlroom-pi").display(),
    );
    agentd.agent(&format!(
        "sh -c {} pi",
        controlroom_sandbox::launch::shell_quote(&script)
    ));
}

/// The result of tool call `call` as `mock` last saw it.
fn tool_result(mock: &MockModel, call: &str) -> String {
    mock.requests()
        .iter()
        .rev()
        .flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == call)
        .map(|m| m["content"].to_string())
        .unwrap_or_else(|| panic!("no result of {call}"))
}

/// The values of a session's model choices.
fn model_choices(session: &Value) -> Vec<String> {
    let model = session["options"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["id"] == "model");
    model.map_or_else(Vec::new, |m| {
        m["choices"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| c["value"].as_str().unwrap().to_owned())
            .collect()
    })
}

/// The session's current model.
fn model(session: &Value) -> Value {
    session["options"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["id"] == "model")
        .map_or(Value::Null, |m| m["value"].clone())
}

impl Ids {
    async fn set_up(world: &mut World) -> Self {
        let garden = world
            .ui
            .ok(
                "profiles.create",
                json!({"name": "Garden club", "color": "#16a34a"}),
            )
            .await;
        assert_eq!(garden["models"], json!({"kind": "any"}));
        let book = world
            .ui
            .ok(
                "profiles.create",
                json!({"name": "Book club", "color": "#7c3aed", "private": true,
                       "models": {"kind": "only", "accounts": ["local"]}}),
            )
            .await;
        let (garden, book) = (id(&garden), id(&book));
        let devbox = world.add_machine(Which::Devbox, &[&garden]).await;
        let buildbox = world.add_machine(Which::Buildbox, &[&book]).await;
        // Pushes go out at once to the one device, for every finished run.
        world
            .ui
            .ok(
                "notifications.subscribe",
                json!({"device_id": "laptop",
                       "subscription": {"endpoint": "https://push.example.com/send/1",
                                        "keys": {"p256dh": "BPubKey", "auth": "AuthSecret"}}}),
            )
            .await;
        world
            .ui
            .ok(
                "notifications.set_preferences",
                json!({"turn_end": true, "delay_s": 0, "hold_while_active": false}),
            )
            .await;
        Self {
            garden,
            book,
            devbox,
            buildbox,
        }
    }
}

fn id(value: &Value) -> String {
    value["id"].as_str().unwrap().to_owned()
}

/// Creates a session in `profile` and waits until its agent is live on a model.
async fn session(
    world: &mut World,
    profile: &str,
    machine: &str,
    path: &str,
    name: &str,
) -> String {
    let id = world
        .create_session(
            &[(machine, path)],
            json!({"profile_id": profile, "name": name}),
        )
        .await;
    world
        .wait_session(&id, "live on a model", |s| {
            s["agent_live"] == true && model(s).is_string()
        })
        .await;
    id
}

/// `session.set_option` of the model.
fn set_model(id: &str, value: &str) -> Value {
    json!({"op_id": op(), "id": id, "option_id": "model", "value": value})
}

/// The `policy_denied` refusal of `method`, checked to name `profile` only.
async fn denied(world: &mut World, method: &str, params: Value, profile: &str) -> String {
    let error = world.ui.err(method, params, "policy_denied").await;
    assert_eq!(error["data"]["profile_id"], profile, "{error}");
    error["message"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the controlroom-pi bundle (CR_TEST_PI_BUNDLE); run by the flake check e2e-profiles"]
async fn two_profiles_stay_apart_across_the_real_stack() {
    let bundle = PathBuf::from(
        std::env::var_os("CR_TEST_PI_BUNDLE")
            .expect("CR_TEST_PI_BUNDLE is the controlroom-pi bundle"),
    );
    let local = MockModel::start(Vec::new()).await.expect("local mock");
    let paid = MockModel::start(Vec::new()).await.expect("paid mock");
    let mut world =
        World::fresh(|agentd, _, _| configure_server(agentd, &bundle, &local, &paid)).await;
    let ids = Ids::set_up(&mut world).await;
    std::fs::write(world.devbox.dir("work").join("notes.txt"), "remote notes\n").unwrap();
    world.buildbox.dir("notes");

    machines_stay_in_their_profile(&mut world, &ids).await;
    let private = the_private_session_uses_only_its_accounts(&mut world, &ids, &local).await;
    let (seeds, task) = refusals_across_profiles(&mut world, &ids, &private).await;
    let archived = session(&mut world, &ids.garden, &ids.devbox, "~/work", "Old notes").await;
    world
        .ui
        .ok("session.close", json!({"op_id": op(), "id": archived}))
        .await;
    world
        .wait_session(&archived, "archived", |s| s["state"] == "closed")
        .await;
    a_machine_that_left_refuses_host_calls(&mut world, &ids, &seeds, &local).await;
    moving_restarts_the_agent_in_its_new_profile(&mut world, &ids, &seeds, &task, &local).await;
    deleting_needs_an_empty_profile(&mut world, &ids, &archived).await;

    assert!(paid.requests().is_empty(), "the paid mock was called");
}

/// C1 and C7: Garden club's session can't use Book club's machine, and a private profile's
/// machine can't be shared.
async fn machines_stay_in_their_profile(world: &mut World, ids: &Ids) {
    let params = world.create_params(
        &[(&ids.buildbox, "~/notes")],
        json!({"profile_id": ids.garden}),
    );
    let message = denied(world, "session.create", params, &ids.garden).await;
    assert!(!message.contains("Book club"), "{message}");
    // Without a profile, the machines name it: devbox and buildbox name none together.
    let params = world.create_params(
        &[(&ids.devbox, "~/work"), (&ids.buildbox, "~/notes")],
        json!({}),
    );
    world
        .ui
        .err("session.create", params, "invalid_params")
        .await;
    denied(
        world,
        "machines.set_settings",
        json!({"machine_id": ids.buildbox, "profile_ids": [ids.book, ids.garden]}),
        &ids.book,
    )
    .await;
}

/// A1, A3 (as pi offers it), A4, C9 and C11 for the private profile; returns its session.
async fn the_private_session_uses_only_its_accounts(
    world: &mut World,
    ids: &Ids,
    local: &MockModel,
) -> String {
    let private = session(world, &ids.book, &ids.buildbox, "~/notes", "Plot twist").await;
    assert_eq!(world.session(&private)["profile_id"], ids.book.as_str());
    // Launched without `paid`: its agent dir has no `paid`, and the server's `paid` default was
    // dropped.
    let models = world
        .agentd
        .state()
        .join(format!("sessions/{private}/agent/models.json"));
    let models = std::fs::read_to_string(models).unwrap();
    assert!(
        models.contains("local") && !models.contains("paid"),
        "{models}"
    );
    assert_eq!(model(&world.session(&private)), "local/m");

    let calls = local.requests().len();
    world.prompt(&private, "hello").await;
    assert!(local.requests().len() > calls, "the local mock answered");
    // pi offers `local` only (its model list arrived before the turn ended).
    assert_eq!(model_choices(&world.session(&private)), ["local/m"]);
    world
        .ui
        .err(
            "session.set_option",
            set_model(&private, "paid/m"),
            "invalid_params",
        )
        .await;
    for change in [
        json!({"models": {"kind": "any"}}),
        json!({"model_defaults": {"provider": "paid", "model": "m"}}),
    ] {
        let mut params = change;
        params["profile_id"] = json!(ids.book);
        world
            .ui
            .err("profiles.update", params, "invalid_params")
            .await;
    }

    // The push names the profile, never the session or its machine.
    let push = world.push.wait_for(&private).await;
    assert_eq!(push["kind"], "private", "{push}");
    assert_eq!(push["profile"], "Book club");
    assert_eq!(push["profile_id"], ids.book.as_str());
    assert_eq!(push["state"], "finished");
    let text = push.to_string();
    assert!(
        !text.contains("Plot twist") && !text.contains("buildbox") && !text.contains("notes"),
        "{text}"
    );
    private
}

/// C2, A3 (a stale choice), C3 and C4 with Garden club's session "Seed catalog"; returns it and
/// its task.
async fn refusals_across_profiles(world: &mut World, ids: &Ids, private: &str) -> (String, String) {
    let seeds = session(world, &ids.garden, &ids.devbox, "~/work", "Seed catalog").await;
    world
        .wait_session(&seeds, "offering both accounts", |s| {
            let mut choices = model_choices(s);
            choices.sort();
            choices == ["local/m", "paid/m"]
        })
        .await;
    denied(
        world,
        "session.workspace_add",
        json!({"op_id": op(), "id": seeds,
               "workspace": {"machine_id": ids.buildbox, "path": "~/notes"}}),
        &ids.garden,
    )
    .await;

    // Garden club drops `paid`: its live pi still offers it, the agent daemon refuses it.
    let only_local = json!({"kind": "only", "accounts": ["local"]});
    world
        .ui
        .ok(
            "profiles.update",
            json!({"profile_id": ids.garden, "models": only_local}),
        )
        .await;
    denied(
        world,
        "session.set_option",
        set_model(&seeds, "paid/m"),
        &ids.garden,
    )
    .await;
    world
        .ui
        .ok("session.set_option", set_model(&seeds, "local/m"))
        .await;
    world
        .wait_session(&seeds, "on local", |s| model(s) == "local/m")
        .await;
    world
        .ui
        .ok(
            "profiles.update",
            json!({"profile_id": ids.garden, "models": {"kind": "any"}}),
        )
        .await;

    let project = world
        .ui
        .ok(
            "projects.create",
            json!({"name": "seeds", "color": "#16a34a", "profile_id": ids.garden}),
        )
        .await;
    let task = world
        .ui
        .ok(
            "tasks.create",
            json!({"title": "order seeds", "project_ids": [project["id"]]}),
        )
        .await;
    let task = id(&task);
    denied(
        world,
        "tasks.link",
        json!({"task_id": task, "session_id": private}),
        &ids.garden,
    )
    .await;
    world
        .ui
        .err(
            "tasks.create",
            json!({"title": "read", "profile_id": ids.book, "project_ids": [project["id"]]}),
            "policy_denied",
        )
        .await;
    world
        .ui
        .ok("tasks.link", json!({"task_id": task, "session_id": seeds}))
        .await;

    // A public profile's push names the session and its machine.
    world.prompt(&seeds, "hello").await;
    let push = world.push.wait_for(&seeds).await;
    assert_eq!(push["kind"], "session", "{push}");
    assert_eq!(push["name"], "Seed catalog");
    assert_eq!(push["machine"], "devbox");
    (seeds, task)
}

/// A8: devbox moves to Book club; Garden club's session stays and the agent daemon refuses its
/// host calls there.
async fn a_machine_that_left_refuses_host_calls(
    world: &mut World,
    ids: &Ids,
    seeds: &str,
    local: &MockModel,
) {
    let mv = json!({"profile_id": ids.book, "machine_ids": [ids.devbox]});
    let mut preview = mv.clone();
    preview["dry_run"] = json!(true);
    let preview = world.ui.ok("profiles.move", preview).await;
    assert_eq!(preview["stranded"], json!([seeds]), "{preview}");
    assert_eq!(world.ui.ok("profiles.move", mv).await, preview);
    let (devbox, book) = (ids.devbox.clone(), ids.book.clone());
    world
        .ui
        .wait("devbox in Book club", |m| {
            m.machine(&devbox)["profile_ids"] == json!([book])
        })
        .await;

    local.push([
        Reply::tool_call("call_left", "read", json!({"path": "devbox:notes.txt"})),
        Reply::text("devbox is gone."),
    ]);
    world.prompt(seeds, "read the notes").await;
    let result = tool_result(local, "call_left");
    assert!(
        result.contains("not in this session's profile") && !result.contains("remote notes"),
        "{result}"
    );
}

/// C13 and `session.move`: the session follows its machine; its task link is removed and its
/// live agent restarts with Book club's accounts.
async fn moving_restarts_the_agent_in_its_new_profile(
    world: &mut World,
    ids: &Ids,
    seeds: &str,
    task: &str,
    local: &MockModel,
) {
    assert_eq!(model_choices(&world.session(seeds)).len(), 2);
    let mv = json!({"profile_id": ids.book, "session_ids": [seeds]});
    let mut preview = mv.clone();
    preview["dry_run"] = json!(true);
    let preview = world.ui.ok("profiles.move", preview).await;
    assert_eq!(preview["session_ids"], json!([seeds]), "{preview}");
    assert_eq!(
        preview["unlinked"],
        json!([{"task_id": task, "session_id": seeds}])
    );
    assert_eq!(world.ui.ok("profiles.move", mv).await, preview);
    let book = ids.book.clone();
    world
        .wait_session(seeds, "restarted in Book club", |s| {
            s["profile_id"] == book.as_str()
                && s["agent_live"] == true
                && model_choices(s) == ["local/m"]
        })
        .await;
    let task_id = task.to_owned();
    world
        .ui
        .wait("the link removed", |m| {
            m.tasks[&task_id]["links"] == json!([])
        })
        .await;

    // devbox is in the session's profile again: the read works, and the push is private now.
    let pushes = world.push.payloads().len();
    local.push([
        Reply::tool_call("call_back", "read", json!({"path": "devbox:notes.txt"})),
        Reply::text("Read it."),
    ]);
    world.prompt(seeds, "read the notes again").await;
    let result = tool_result(local, "call_back");
    assert!(result.contains("remote notes"), "{result}");
    let (push, seeds) = (world.push.clone(), seeds.to_owned());
    controlroom_controller::test_support::wait_until("the moved session's push", || {
        push.payloads()[pushes..]
            .iter()
            .any(|p| p["session_id"] == seeds.as_str())
    })
    .await;
    let after = world.push.payloads()[pushes..]
        .iter()
        .find(|p| p["session_id"] == seeds.as_str())
        .cloned()
        .unwrap();
    assert_eq!(after["kind"], "private", "{after}");
}

/// C10: a profile with machines, projects, tasks or archived sessions can't be deleted; the
/// archived ones go with it on request.
async fn deleting_needs_an_empty_profile(world: &mut World, ids: &Ids, archived: &str) {
    let error = world
        .ui
        .err(
            "profiles.delete",
            json!({"profile_id": ids.book}),
            "invalid_state",
        )
        .await;
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("Book club still has 2 machines"),
        "{error}"
    );
    let delete = json!({"profile_id": ids.garden});
    let error = world
        .ui
        .err("profiles.delete", delete.clone(), "invalid_state")
        .await;
    assert_eq!(
        error["message"],
        "Garden club still has 1 project and 1 task; move or delete them first"
    );
    let (projects, tasks): (Vec<String>, Vec<String>) = (
        world.ui.mirror.projects.keys().cloned().collect(),
        world.ui.mirror.tasks.keys().cloned().collect(),
    );
    for project in projects {
        world
            .ui
            .ok("projects.delete", json!({"project_id": project}))
            .await;
    }
    for task in tasks {
        world.ui.ok("tasks.delete", json!({"task_id": task})).await;
    }
    let error = world
        .ui
        .err("profiles.delete", delete, "invalid_state")
        .await;
    assert_eq!(error["data"]["state"], "closed", "{error}");
    world
        .ui
        .ok(
            "profiles.delete",
            json!({"profile_id": ids.garden, "delete_archived": true}),
        )
        .await;
    let (garden, archived) = (ids.garden.clone(), archived.to_owned());
    world
        .ui
        .wait("Garden club gone with its archived session", |m| {
            !m.profiles.contains_key(&garden) && m.session(&archived).is_null()
        })
        .await;
    assert_eq!(
        world.ui.ok("profiles.list", json!({})).await["profiles"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
