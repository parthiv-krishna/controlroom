//! The UI API without machines: handshake, framing, controller data, the state feed, devices,
//! auth sessions and revocation.
#![cfg(unix)]

mod support;

use controlroom_controller::test_support::UiDriver;
use controlroom_controller::{CloseReason, Controller, Identity};
use serde_json::{Value, json};
use support::{controller, ui};

#[tokio::test]
async fn hello_comes_first_and_bad_frames_fail_alone() {
    let setup = controller(None, None);
    let principal = setup.controller.resolve(&Identity::DevToken).unwrap();
    let mut ui = UiDriver::new(setup.controller.open_trusted_client(principal).unwrap());
    ui.err("keys.list", json!({}), "hello_required").await;
    ui.ok("ping", json!({})).await;
    let hello = ui
        .ok(
            "hello",
            json!({"protocol_version": {"major": 1, "minor": 9}}),
        )
        .await;
    assert_eq!(hello["auth"], "dev_token");
    assert_eq!(hello["user"]["id"], "dev");
    assert_eq!(hello["push"], Value::Null);
    ui.err(
        "hello",
        json!({"protocol_version": {"major": 2, "minor": 0}}),
        "incompatible_version",
    )
    .await;
    ui.err("teleport", json!({}), "method_not_found").await;
    ui.err("tasks.create", json!({"titel": "x"}), "invalid_params")
        .await;
    ui.send_raw("{not json").await;
    // A parse error is answered with a null id; the connection carries on.
    ui.ok("ping", json!({})).await;
}

#[tokio::test]
async fn projects_and_tasks_reach_every_tab_in_order() {
    let setup = controller(None, None);
    let mut a = ui(&setup.controller, "laptop").await;
    let mut b = ui(&setup.controller, "phone").await;
    let project = a
        .ok(
            "projects.create",
            json!({"name": "app", "color": "#a78bfa"}),
        )
        .await;
    let pid = project["id"].as_str().unwrap().to_owned();
    let task = a
        .ok(
            "tasks.create",
            json!({"title": "ship", "project_ids": [pid], "checklist": ["a", "c"]}),
        )
        .await;
    let tid = task["id"].as_str().unwrap().to_owned();
    assert_eq!(task["status"], "backlog");
    assert_eq!(task["priority"], "p2");
    a.ok(
        "tasks.checklist_add",
        json!({"task_id": tid, "text": "b", "index": 1}),
    )
    .await;
    let item = a.mirror.tasks.get(&tid).cloned();
    let item = item.unwrap_or_else(|| task.clone())["checklist"][0]["id"].clone();
    a.ok(
        "tasks.checklist_update",
        json!({"task_id": tid, "item_id": item, "done": true}),
    )
    .await;
    // Sent back to back without waiting: the shared ordering key applies them in order.
    let m1 = a
        .send(
            "tasks.move",
            json!({"task_id": tid, "status": "doing", "order": 1.0}),
        )
        .await;
    let m2 = a
        .send(
            "tasks.move",
            json!({"task_id": tid, "status": "review", "order": 2.0}),
        )
        .await;
    a.response(m1).await;
    a.response(m2).await;
    b.wait("the other tab sees the task", |m| {
        m.tasks.get(&tid).is_some_and(|t| {
            t["status"] == "review"
                && t["checklist"].as_array().unwrap().len() == 3
                && t["checklist"][0]["done"] == true
        })
    })
    .await;
    let texts: Vec<_> = b.mirror.tasks[&tid]["checklist"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(texts, ["a", "b", "c"]);

    a.ok("projects.delete", json!({"project_id": pid})).await;
    b.wait("the project is gone and unassigned", |m| {
        m.projects.is_empty() && m.tasks[&tid]["project_ids"] == json!([])
    })
    .await;
    a.err("tasks.delete", json!({"task_id": pid}), "not_found")
        .await;
}

#[tokio::test]
async fn state_cursor_catches_up_or_snapshots() {
    let setup = controller(None, None);
    let mut a = ui(&setup.controller, "laptop").await;
    let cursor = a.cursor.clone().unwrap();
    a.ok("tasks.create", json!({"title": "one"})).await;
    a.ok("tasks.create", json!({"title": "two"})).await;
    // A reconnecting tab with the old cursor gets just the changes.
    let principal = setup.controller.resolve(&Identity::DevToken).unwrap();
    let mut b = UiDriver::new(setup.controller.open_trusted_client(principal).unwrap());
    b.hello("phone", "Phone").await;
    let resumed = b.ok("state.subscribe", json!({"since": cursor})).await;
    assert_eq!(resumed["snapshot"], Value::Null);
    b.wait("the catch-up", |m| {
        m.tasks.len() == 2 && m.devices.contains_key("phone")
    })
    .await;
    // Another epoch: a snapshot.
    let other = json!({"epoch": "01HZY3K6W8Q9X2V4N7B5C1D3E6", "rev": 1});
    let fresh = b.ok("state.subscribe", json!({"since": other})).await;
    assert_eq!(fresh["snapshot"]["tasks"].as_array().unwrap().len(), 2);
    let sub = fresh["subscription_id"].clone();
    b.ok("unsubscribe", json!({"subscription_id": sub})).await;
    b.err("unsubscribe", json!({"subscription_id": sub}), "not_found")
        .await;
}

#[tokio::test]
async fn devices_preferences_and_sandbox_profiles() {
    let setup = controller(None, None);
    let mut a = ui(&setup.controller, "laptop").await;
    // The server's label wins on re-register.
    let again = a
        .ok(
            "devices.register",
            json!({"device_id": "laptop", "label": "Other"}),
        )
        .await;
    assert_eq!(again["label"], "laptop (Firefox on Linux)");
    a.ok(
        "devices.rename",
        json!({"device_id": "laptop", "label": "Laptop"}),
    )
    .await;
    let device = a
        .ok(
            "notifications.subscribe",
            json!({"device_id": "laptop", "subscription": support::browser_subscription(1)}),
        )
        .await;
    assert_eq!(device["push"], true);
    a.err(
        "notifications.subscribe",
        json!({"device_id": "laptop", "subscription": {"endpoint": "http://x"}}),
        "invalid_params",
    )
    .await;
    let prefs = a.ok("notifications.preferences", json!({})).await;
    assert_eq!(
        prefs,
        json!({"attention": true, "questions": false, "turn_end": false,
               "hold_while_active": true, "delay_s": 30, "idle_s": 120})
    );
    let prefs = a
        .ok(
            "notifications.set_preferences",
            json!({"questions": true, "hold_while_active": false, "delay_s": 0, "idle_s": 300}),
        )
        .await;
    assert_eq!(
        prefs,
        json!({"attention": true, "questions": true, "turn_end": false,
               "hold_while_active": false, "delay_s": 0, "idle_s": 300})
    );
    for bad in [json!({"delay_s": 3601}), json!({"idle_s": 59})] {
        a.err("notifications.set_preferences", bad, "invalid_params")
            .await;
    }
    a.ok("client.activity", json!({})).await;

    let project = a
        .ok("projects.create", json!({"name": "p", "color": "#000"}))
        .await;
    let scope = json!({"kind": "project", "project_id": project["id"]});
    let profiles = a
        .ok(
            "sandbox.set_profile",
            json!({"scope": scope, "change": {"kind": "set_level", "level": "relaxed"}}),
        )
        .await;
    assert_eq!(profiles["profiles"][0]["level"], "relaxed");
    a.err(
        "sandbox.set_profile",
        json!({"scope": {"kind": "machine", "machine_id": "01HZY3K6W8Q9X2V4N7B5C1D3E6"},
               "change": {"kind": "clear_level"}}),
        "not_found",
    )
    .await;

    // Schedules need a valid spec (the rest is in tests/schedules.rs).
    a.err(
        "schedules.create",
        json!({"session_id": "01HZY3K6W8Q9X2V4N7B5C1D3E7", "name": "inbox",
               "spec": {"kind": "cron", "expr": "0 9 * *"}, "timezone": "Etc/UTC",
               "message": "Check inbox/"}),
        "invalid_params",
    )
    .await;
    a.wait("the feed carries devices", |m| {
        m.devices["laptop"]["label"] == "Laptop"
    })
    .await;
}

#[tokio::test]
async fn keys_generate_import_delete() {
    let setup = controller(None, None);
    let mut a = ui(&setup.controller, "laptop").await;
    let list = a.ok("keys.list", json!({})).await;
    assert!(
        list["controlroom"]["install_command"]
            .as_str()
            .unwrap()
            .contains("authorized_keys")
    );
    let generated = a.ok("keys.generate", json!({"name": "work"})).await;
    assert_eq!(generated["imported"], false);
    a.err(
        "keys.import",
        json!({"name": "bad", "private_key": "nope"}),
        "invalid_key",
    )
    .await;
    let list = a.ok("keys.list", json!({})).await;
    assert_eq!(list["additional"].as_array().unwrap().len(), 1);
    a.ok("keys.delete", json!({"key_id": generated["id"]}))
        .await;
    a.err(
        "keys.delete",
        json!({"key_id": generated["id"]}),
        "not_found",
    )
    .await;
}

/// Logins: the allow-list and mode are re-checked per request, and revoking a device or a
/// login session closes its open connections.
#[tokio::test]
async fn revocation_closes_clients() {
    let setup = controller(None, None);
    let c: &Controller = &setup.controller;
    let principal = c.resolve(&Identity::DevToken).unwrap();
    assert!(
        c.resolve(&Identity::Oidc {
            subject: "alice".into(),
            groups: vec![],
            name: None,
            email: None
        })
        .is_err(),
        "another auth mode is refused"
    );

    // A device removed through another tab signs this one out.
    let login = c.create_login(&principal, None).unwrap();
    let auth = c.authenticate(&login.token).unwrap();
    let mut phone = UiDriver::new(c.open_client(&auth).unwrap());
    phone.hello("phone", "Phone").await;
    let mut laptop = ui(c, "laptop").await;
    laptop
        .ok("devices.remove", json!({"device_id": "phone"}))
        .await;
    assert_eq!(phone.closed().await, Some(CloseReason::Unauthenticated));
    assert!(
        c.authenticate(&login.token).is_err(),
        "the login was revoked"
    );

    // Logout closes the session's connections.
    let login = c.create_login(&principal, None).unwrap();
    let auth = c.authenticate(&login.token).unwrap();
    let mut tab = UiDriver::new(c.open_client(&auth).unwrap());
    tab.hello("tablet", "Tablet").await;
    c.logout(&auth).unwrap();
    assert_eq!(tab.closed().await, Some(CloseReason::Unauthenticated));

    // A session revoked behind the connection's back fails its next request and closes it.
    let login = c.create_login(&principal, None).unwrap();
    let auth = c.authenticate(&login.token).unwrap();
    let mut other = UiDriver::new(c.open_client(&auth).unwrap());
    other
        .ok(
            "hello",
            json!({"protocol_version": {"major": 1, "minor": 0}, "device_id": "desk"}),
        )
        .await;
    c.revoke_owner(&auth.principal.owner).unwrap();
    assert_eq!(other.closed().await, Some(CloseReason::Unauthenticated));
    // The trusted laptop tab was closed too.
    assert_eq!(laptop.closed().await, Some(CloseReason::Unauthenticated));
}
