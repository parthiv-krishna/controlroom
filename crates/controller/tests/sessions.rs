//! The UI API's sessions against a fake agent daemon: global sessions in the state feed
//! with their project membership (Unassigned without a linked task) and the workspace overlaps,
//! session creation and pass-through, shared session subscriptions, and schedules on global
//! sessions.

mod support;

use controlroom_controller::test_support::{
    FakeAgentd, FakeHost, RemoteHost, bundled_daemons, op, session_summary, text,
};
use controlroom_protocol::UnixMillis;
use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::{SessionState, WorkspaceSpec};
use serde_json::{Value, json};
use support::{add_machine, controller_with_agentd, ui};

fn spec(machine: MachineId, path: &str) -> WorkspaceSpec {
    WorkspaceSpec {
        machine_id: machine,
        path: path.into(),
    }
}

fn s(id: SessionId) -> String {
    id.to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_feed_shows_global_sessions_with_projects_and_overlaps() {
    let agentd = FakeAgentd::start().await;
    let setup = controller_with_agentd(None, agentd.socket());
    let mut next = ui(&setup.controller, "laptop").await;
    let mut phone = ui(&setup.controller, "phone").await;
    agentd.wait_connected(1).await;

    // Two open sessions in one directory (one through a resolved root) overlap; a third
    // elsewhere doesn't.
    let devbox = MachineId::generate();
    let (a, b, c) = (
        SessionId::generate(),
        SessionId::generate(),
        SessionId::generate(),
    );
    let mut first = session_summary(a, &[spec(devbox, "~/src")]);
    first.created_at = UnixMillis(1);
    first.workspaces[0].root = Some("/home/alice/src".into());
    let mut second = session_summary(b, &[spec(devbox, "/home/alice/src")]);
    second.created_at = UnixMillis(2);
    second.workspaces[0].root = Some("/home/alice/src".into());
    agentd.upsert_session(first);
    agentd.upsert_session(second.clone());
    agentd.upsert_session(session_summary(c, &[spec(devbox, "~/other")]));
    next.wait("three sessions", |m| m.sessions.len() == 3).await;
    for id in [a, b, c] {
        let session = &next.mirror.sessions[&s(id)];
        assert_eq!(session["project_ids"], json!([]), "Unassigned");
    }
    next.wait("an overlap", |m| {
        m.overlaps.as_array().is_some_and(|o| o.len() == 1)
    })
    .await;
    assert_eq!(
        next.mirror.overlaps,
        json!([{"machine_id": devbox, "path": "/home/alice/src", "session_ids": [a, b]}])
    );

    // Linking a task puts a session in the task's projects.
    let web = next
        .ok(
            "projects.create",
            json!({"name": "web", "color": "#a78bfa"}),
        )
        .await;
    let ops = next
        .ok(
            "projects.create",
            json!({"name": "ops", "color": "#000000"}),
        )
        .await;
    let task = next
        .ok(
            "tasks.create",
            json!({"title": "ship", "project_ids": [ops["id"], web["id"]]}),
        )
        .await;
    let linked = next
        .ok(
            "tasks.link",
            json!({"task_id": task["id"], "session_id": a}),
        )
        .await;
    assert_eq!(linked["links"], json!([a]));
    next.wait("a joins the task's projects", |m| {
        m.sessions[&s(a)]["project_ids"] == json!([web["id"], ops["id"]])
    })
    .await;
    assert_eq!(next.mirror.sessions[&s(b)]["project_ids"], json!([]));
    // Deleting a project drops it from the membership.
    next.ok("projects.delete", json!({"project_id": ops["id"]}))
        .await;
    next.wait("ops is gone from a", |m| {
        m.sessions[&s(a)]["project_ids"] == json!([web["id"]])
    })
    .await;
    next.ok(
        "tasks.unlink",
        json!({"task_id": task["id"], "session_id": a}),
    )
    .await;
    next.wait("a is Unassigned again", |m| {
        m.sessions[&s(a)]["project_ids"] == json!([])
    })
    .await;

    // Another tab sees the same sessions and projects (projects have no locations).
    phone
        .wait("the other tab sees the sessions and the project", |m| {
            m.sessions.len() == 3 && m.projects.contains_key(web["id"].as_str().unwrap())
        })
        .await;
    assert_eq!(
        phone.mirror.projects[web["id"].as_str().unwrap()],
        json!({"id": web["id"], "name": "web", "color": "#a78bfa"})
    );

    // Closing a session ends its overlap; deleting it removes it.
    second.state = SessionState::Closed;
    agentd.upsert_session(second);
    next.wait("no overlaps", |m| m.overlaps == json!([])).await;
    agentd.remove_session(c);
    next.wait("c is gone", |m| !m.sessions.contains_key(&s(c)))
        .await;

    // A reconnecting agent daemon resumes the feed (no snapshot needed: nothing changed).
    agentd.drop_connection();
    agentd.wait_connected(2).await;
    agentd.upsert_session(session_summary(c, &[spec(devbox, "~/other")]));
    next.wait("c is back", |m| m.sessions.contains_key(&s(c)))
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_are_created_and_driven_through_the_agent_daemon() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let _fake = FakeHost::start(&host);
    let mut next = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let devbox = add_machine(&mut next, &host, "devbox").await;
    next.ok(
        "sandbox.set_profile",
        json!({"scope": {"kind": "machine", "machine_id": devbox},
               "change": {"kind": "set_level", "level": "relaxed"}}),
    )
    .await;
    let task = next.ok("tasks.create", json!({"title": "ship"})).await;

    // The controller fills the sandbox defaults and the prompt's origin, then links the task.
    let create_op = op();
    let params = json!({
        "op_id": create_op,
        "workspaces": [{"machine_id": devbox, "path": "~/src"}],
        "prompt": {"content": text("hi"), "origin": {"kind": "user", "device": {"device_id": "evil", "label": "x"}}},
        "sandbox": {"level": "off", "grants": [], "domains": [], "mediated": []},
        "task_ids": [task["id"]],
    });
    let created = next.ok("session.create", params.clone()).await;
    let session = created["session"]["id"].as_str().unwrap().to_owned();
    let sent = &agentd.calls("session.create")[0];
    assert_eq!(sent["sandbox"]["level"], "relaxed");
    assert_eq!(sent["prompt"]["origin"]["device"]["device_id"], "laptop");
    assert!(sent.get("task_ids").is_none());
    next.wait("the task is linked and in progress", |m| {
        let t = &m.tasks[task["id"].as_str().unwrap()];
        t["links"] == json!([session]) && t["status"] == "doing"
    })
    .await;
    next.wait("the session is in the task's (no) projects", |m| {
        m.sessions.contains_key(&session)
    })
    .await;
    // A retry is the same session.
    let again = next.ok("session.create", params).await;
    assert_eq!(again["session"]["id"], json!(session));

    // Pass-through: the controller's attribution replaces the client's.
    next.ok(
        "session.prompt",
        json!({"op_id": op(), "id": session, "content": text("more"),
               "origin": {"kind": "user", "device": null}}),
    )
    .await;
    let prompt = &agentd.calls("session.prompt")[0];
    assert_eq!(prompt["origin"]["device"]["device_id"], "laptop");
    assert!(prompt.get("machine_id").is_none());
    next.ok(
        "request.respond",
        json!({"op_id": op(), "id": session, "request_id": "r1",
               "response": {"kind": "dismiss"}, "by": {"device_id": "evil", "label": "x"}}),
    )
    .await;
    assert_eq!(
        agentd.calls("request.respond")[0]["by"]["device_id"],
        "laptop"
    );
    next.ok(
        "session.workspace_add",
        json!({"op_id": op(), "id": session, "workspace": {"machine_id": devbox, "path": "~/web"}}),
    )
    .await;
    assert_eq!(agentd.calls("session.workspace_add").len(), 1);

    // Two tabs share one agent daemon subscription; text arrives once per tab, without a
    // machine.
    let mut tab = ui(&setup.controller, "phone").await;
    let first = next
        .ok(
            "session.subscribe",
            json!({"id": session, "since_seq": 0, "timeline": true}),
        )
        .await;
    tab.ok(
        "session.subscribe",
        json!({"id": session, "since_seq": 0, "timeline": true}),
    )
    .await;
    assert_eq!(agentd.calls("session.subscribe").len(), 1);
    let id: SessionId = session.parse().unwrap();
    agentd.append_text(id, 1, "hello");
    for ui in [&mut next, &mut tab] {
        let events = ui
            .notification("session.events", |n| n["method"] == "session.events")
            .await;
        assert_eq!(events["params"]["ops"][0]["text"], "hello");
        assert!(events["params"].get("machine_id").is_none());
    }
    assert!(first["detail"].is_object());

    // Schedules wake global sessions through the agent daemon.
    let schedule = next
        .ok(
            "schedules.create",
            json!({"session_id": session, "name": "inbox", "spec": {"kind": "interval", "every_s": 3600},
                   "timezone": "UTC", "message": "check"}),
        )
        .await;
    assert!(schedule.get("machine_id").is_none());
    next.ok("schedules.run_now", json!({"schedule_id": schedule["id"]}))
        .await;
    let fired = agentd
        .calls("session.prompt")
        .into_iter()
        .find(|p| p["origin"]["kind"] == "schedule")
        .unwrap();
    assert_eq!(fired["origin"]["schedule_id"], schedule["id"]);
    assert!(
        fired["op_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}:", schedule["id"].as_str().unwrap()))
    );

    // Without the agent daemon, pass-through calls time out at once.
    drop(agentd);
    let error = next
        .err(
            "session.cancel",
            json!({"op_id": op(), "id": session}),
            "timeout",
        )
        .await;
    assert_eq!(error["data"]["kind"], Value::from("timeout"));
}

/// The agent daemon refuses machines it doesn't see connected, and learns of connectivity
/// through the controller's queue of machine reports. A machine the UI shows connected again
/// must be connected for the agent daemon too when the create reaches it: the controller lets
/// the queued reports through first (a held report used to be overtaken by the create, which
/// then failed with `machine_unavailable`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_waits_for_the_agent_daemon_to_see_what_the_ui_saw() {
    use controlroom_controller::test_support::Relay;
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let _fake = FakeHost::start(&host);
    let relay = Relay::start(host.port()).await;
    let mut next = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let devbox = support::add_machine_at(&mut next, &host, "devbox", relay.port).await;
    let id: MachineId = devbox.parse().unwrap();

    relay.down();
    next.wait("devbox is down in the UI", |m| {
        m.machine(&devbox)["connectivity"]["state"] != "connected"
    })
    .await;
    agentd
        .wait_machines("devbox is reported down", |m| {
            m.iter()
                .any(|m| m.machine_id == id && !m.availability.is_connected())
        })
        .await;
    // The next report (devbox is back) waits in the controller's queue to the agent daemon.
    agentd.hold_machines(true);
    relay.up();
    next.wait("devbox is back in the UI", |m| {
        m.machine(&devbox)["connectivity"]["state"] == "connected"
    })
    .await;
    let create = next
        .send(
            "session.create",
            json!({"op_id": op(), "workspaces": [{"machine_id": devbox, "path": "~/src"}],
                   "sandbox_level": "off"}),
        )
        .await;
    // The create waits for the held report. (Only a time bound can show that it waits; with
    // the fix it can't finish early however long this takes, so this never flakes.)
    let early =
        tokio::time::timeout(std::time::Duration::from_millis(500), next.response(create)).await;
    assert!(
        early.is_err(),
        "the create overtook the machine report: {early:?}"
    );
    agentd.hold_machines(false);
    let response = next.response(create).await;
    assert!(response.get("error").is_none(), "{response}");
}

/// "Always allow" and "Never" on a file request become standing on the requesting machine's
/// sandbox defaults, recorded with the deciding device; new sessions get them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_and_never_on_a_file_request_change_the_machine_defaults() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let _fake = FakeHost::start(&host);
    let mut next = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let devbox = add_machine(&mut next, &host, "devbox").await;
    let session = SessionId::generate();

    let answered = |option: &str, path: &str, access: &str| {
        json!({"request": {
            "id": "approval-1", "kind": "sandbox", "blocking": true, "state": "answered",
            "title": "Write?", "detail": null,
            "body": {"kind": "path", "path": path, "access": access, "in_workspace": false,
                     "machine_id": devbox, "machine": "devbox"},
            "options": [], "questions": [],
            "answer": {"response": {"kind": "choice", "option_id": option},
                       "label": "x", "by": null, "at": 1},
            "origin_child": null, "created_at": 1},
            "then": null})
    };
    let respond = |option: &str| {
        json!({"op_id": op(), "id": session, "request_id": "approval-1",
               "response": {"kind": "choice", "option_id": option}})
    };
    agentd.answer_with(
        "request.respond",
        answered(
            "allow-always",
            "/home/alice/.cache/nix/x.sqlite-shm",
            "write",
        ),
    );
    next.ok("request.respond", respond("allow-always")).await;
    agentd.answer_with(
        "request.respond",
        answered("never", "/home/alice/.bash_history", "read"),
    );
    next.ok("request.respond", respond("never")).await;
    // Another answer changes nothing.
    agentd.answer_with(
        "request.respond",
        answered("allow-session", "/srv/other", "read"),
    );
    next.ok("request.respond", respond("allow-session")).await;

    let profiles = next.ok("sandbox.profiles", json!({})).await["profiles"].clone();
    let profile = &profiles[0];
    assert_eq!(
        profile["scope"],
        json!({"kind": "machine", "machine_id": devbox})
    );
    let grants = profile["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1, "{profile}");
    assert_eq!(grants[0]["kind"], "path");
    assert_eq!(grants[0]["value"], "/home/alice/.cache/nix/x.sqlite-shm");
    assert_eq!(grants[0]["access"], "read_write");
    assert_eq!(grants[0]["scope"], "standing");
    assert_eq!(grants[0]["by"]["device_id"], "laptop");
    assert_eq!(profile["never"], json!(["/home/alice/.bash_history"]));

    // A new session on the machine starts with both.
    let created = next
        .ok(
            "session.create",
            json!({"op_id": op(), "workspaces": [{"machine_id": devbox, "path": "~/src"}]}),
        )
        .await;
    assert!(created["session"]["id"].is_string());
    let sent = &agentd.calls("session.create")[0]["sandbox"];
    assert_eq!(
        sent["grants"][0]["value"],
        "/home/alice/.cache/nix/x.sqlite-shm"
    );
    assert_eq!(sent["never"], json!(["/home/alice/.bash_history"]));
}

/// Settings › Sandbox adds path grants and never-paths per machine, normalized; relative paths,
/// `..` and a read-write `/` are refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn path_grants_and_never_paths_are_checked_and_normalized() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let _fake = FakeHost::start(&host);
    let mut next = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let devbox = add_machine(&mut next, &host, "devbox").await;
    let scope = json!({"kind": "machine", "machine_id": devbox});
    let grant = |value: &str, access: &str| {
        json!({"scope": scope, "change": {"kind": "add_grant",
               "grant": {"kind": "path", "value": value, "access": access}}})
    };
    for (value, access) in [("rel/x", "read"), ("~/a/../b", "read"), ("/", "read_write")] {
        next.err(
            "sandbox.set_profile",
            grant(value, access),
            "invalid_params",
        )
        .await;
    }
    next.ok("sandbox.set_profile", grant("~/.cache//nix/", "read_write"))
        .await;
    let result = next
        .ok(
            "sandbox.set_profile",
            json!({"scope": scope, "change": {"kind": "add_never", "path": "~/.bash_history"}}),
        )
        .await;
    let profile = &result["profiles"][0];
    assert_eq!(profile["grants"][0]["value"], "~/.cache/nix");
    assert_eq!(profile["grants"][0]["by"]["device_id"], "laptop");
    assert_eq!(profile["never"], json!(["~/.bash_history"]));
    let result = next
        .ok(
            "sandbox.set_profile",
            json!({"scope": scope, "change": {"kind": "remove_never", "path": "~/.bash_history"}}),
        )
        .await;
    assert_eq!(result["profiles"][0]["never"], json!([]));
}
