//! End to end with real machines: the controller adds fake remote machines over SSH
//! (client-core's in-process server running exec requests locally), installs the built
//! `controlroomd` from a bundled-daemons directory, and drives it through the UI API. Sessions
//! live on the agent daemon (a fake one here).
#![cfg(unix)]

mod support;

use std::time::Duration;

use controlroom_controller::test_support::{
    DAEMON_VERSION, FakeAgentd, RemoteHost, bundled_daemons, op, session_summary, wait_until,
};
use controlroom_protocol::ids::SessionId;
use controlroom_protocol::session::{Attention, Request, WorkspaceSpec};
use serde_json::json;
use support::{add_machine, controller, ui};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_machine_confirms_the_host_key_recovers_from_auth_failure_and_installs() {
    let bundled = bundled_daemons();
    let setup = controller(Some(bundled.path()), None);
    let host = RemoteHost::start().await;
    let mut ui = ui(&setup.controller, "laptop").await;

    let new = host.new_machine("devbox");
    let added = ui.ok("machines.add", new.clone()).await;
    let fingerprint = added["host_key"]["fingerprint"].clone();
    assert_eq!(added["host_key"]["algorithm"], "ssh-ed25519");
    assert!(ui.mirror.machines.is_empty(), "nothing is saved yet");

    // A different fingerprint than the host presents is refused.
    let mut confirm = new.clone();
    confirm["host_key_fingerprint"] = json!("SHA256:not-it");
    ui.err("machines.add_confirm", confirm.clone(), "host_key_mismatch")
        .await;

    // The key isn't installed yet: the answer repeats the install command.
    confirm["host_key_fingerprint"] = fingerprint.clone();
    let denied = ui.ok("machines.add_confirm", confirm.clone()).await;
    assert_eq!(denied["outcome"], "permission_denied", "{denied}");
    let install = denied["install_command"].as_str().unwrap();
    host.run_local(install);
    let result = ui.ok("machines.add_confirm", confirm.clone()).await;
    assert_eq!(result["outcome"], "added", "{result}");
    let id = result["machine"]["id"].as_str().unwrap().to_owned();
    assert_eq!(result["machine"]["host_key"]["fingerprint"], fingerprint);

    ui.wait("installed, connected and doctored", |m| {
        let machine = m.machine(&id);
        machine["connectivity"]["state"] == "connected"
            && machine["version"] == DAEMON_VERSION
            && machine["doctor"]["daemon_version"] == DAEMON_VERSION
    })
    .await;
    assert_eq!(
        host.current_daemon().as_deref(),
        Some(format!("versions/{DAEMON_VERSION}").as_str())
    );
    let machine = ui.mirror.machine(&id);
    assert_eq!(machine["update_available"], false);
    assert_eq!(machine["arch"], std::env::consts::ARCH);
    assert!(machine["daemon_id"].is_string());

    // The same host can't be added twice.
    ui.err("machines.add", new, "invalid_params").await;

    // A directory listing from the host daemon.
    let listing = ui
        .ok("fs.list", json!({"machine_id": id, "path": host.work()}))
        .await;
    assert!(listing["entries"].is_array());

    // A changed host key is a hard stop until the user confirms the new one.
    let new_key = host.server().rotate_host_key();
    ui.ok("machines.reconnect", json!({"machine_id": id})).await;
    ui.wait("host key changed", |m| {
        let c = &m.machine(&id)["connectivity"];
        c["state"] == "action_required" && c["reason"] == "host_key_changed"
    })
    .await;
    ui.err(
        "fs.list",
        json!({"machine_id": id, "path": "~"}),
        "machine_unavailable",
    )
    .await;
    let presented =
        ui.mirror.machine(&id)["connectivity"]["presented_host_key"]["fingerprint"].clone();
    assert_eq!(presented, json!(new_key.fingerprint()));
    ui.ok(
        "machines.confirm_host_key",
        json!({"machine_id": id, "host_key_fingerprint": presented}),
    )
    .await;
    ui.wait("reconnected", |m| {
        m.machine(&id)["connectivity"]["state"] == "connected"
    })
    .await;

    ui.ok("machines.remove", json!({"machine_id": id})).await;
    ui.wait("removed", |m| m.machines.is_empty()).await;
    setup.controller.shutdown().await;
}

/// The prototype's lost update: two tabs writing different machine settings at once. Writes are
/// column-targeted, so both survive any interleaving (with the daemon's contact writes on
/// connect racing them too).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_settings_writes_keep_both() {
    let bundled = bundled_daemons();
    let setup = controller(Some(bundled.path()), None);
    let host = RemoteHost::start().await;
    let mut a = ui(&setup.controller, "laptop").await;
    let mut b = ui(&setup.controller, "phone").await;
    let machine = add_machine(&mut a, &host, "devbox").await;
    let rounds = 10;
    let names = async {
        for i in 0..rounds {
            a.ok(
                "machines.set_settings",
                json!({"machine_id": machine, "name": format!("box-{i}"), "auto_upgrade": i % 2 == 0}),
            )
            .await;
        }
    };
    let graces = async {
        for i in 0..rounds {
            b.ok(
                "machines.set_settings",
                json!({"machine_id": machine, "grace_s": 100 + i}),
            )
            .await;
        }
    };
    tokio::join!(names, graces);
    a.wait("both writes kept", |m| {
        let machine = m.machine(&machine);
        machine["name"] == format!("box-{}", rounds - 1)
            && machine["grace_s"] == 100 + rounds - 1
            && machine["auto_upgrade"] == false
    })
    .await;
    setup.controller.shutdown().await;
}

/// An unreachable machine (a host that accepts TCP and never speaks SSH) never holds up
/// another machine or the UI.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_machine_does_not_block_others() {
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    // The stuck machine is recorded before the start (adding it would wait for its host key).
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("controller.sqlite");
    let store = controlroom_controller::store::Store::open(&store_path).unwrap();
    let owner = controlroom_controller::OwnerId::local();
    store.ensure_owner(&owner).unwrap();
    let stuck = store
        .owner(&owner)
        .machines()
        .insert(controlroom_controller::store::NewMachineRow {
            name: "stuck".into(),
            host: "127.0.0.1".into(),
            port: silent_port,
            username: "alice".into(),
            key_id: None,
            auth_key_id: None,
            host_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEvYYFqtmyAwG91pMgBKerb/Ly0i8a4OUZAXeWRT7bdw"
                    .into(),
        })
        .unwrap()
        .id
        .to_string();
    drop(store);
    let bundled = bundled_daemons();
    let controller = controlroom_controller::Controller::start(support::config(
        &store_path,
        Some(bundled.path()),
        None,
    ))
    .unwrap();
    let host = RemoteHost::start().await;
    let mut ui = ui(&controller, "laptop").await;
    ui.wait("the stuck machine is connecting", |m| {
        m.machine(&stuck)["connectivity"]["state"] == "connecting"
    })
    .await;

    // Another machine is added and answers meanwhile.
    let machine = add_machine(&mut ui, &host, "devbox").await;
    let listing = ui
        .ok(
            "fs.list",
            json!({"machine_id": machine, "path": host.work(), "include_files": true}),
        )
        .await;
    assert!(listing["entries"].is_array(), "{listing}");
    // Calls to the stuck machine fail at once instead of waiting.
    let started = std::time::Instant::now();
    ui.err(
        "fs.list",
        json!({"machine_id": stuck, "path": "~"}),
        "machine_unavailable",
    )
    .await;
    ui.ok("machines.reconnect", json!({"machine_id": stuck}))
        .await;
    assert!(started.elapsed() < Duration::from_secs(2));
    ui.wait("the stuck machine is unreachable after its timeout", |m| {
        m.machine(&stuck)["connectivity"]["state"] == "unreachable"
    })
    .await;
    held.abort();
    controller.shutdown().await;
}

/// Rotating the controlroom key migrates connected machines over their connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rotation_migrates_connected_machines() {
    let bundled = bundled_daemons();
    let setup = controller(Some(bundled.path()), None);
    let host = RemoteHost::start().await;
    let mut ui = ui(&setup.controller, "laptop").await;
    let machine = add_machine(&mut ui, &host, "devbox").await;
    let old = support::controlroom_key(&mut ui).await;
    let rotated = ui.ok("keys.rotate", json!({})).await;
    assert_eq!(
        (rotated["migrated"].clone(), rotated["pending"].clone()),
        (json!(1), json!(0))
    );
    let new = rotated["key"]["public_key"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    let installed = host.authorized_keys();
    let blob = |line: &str| line.split_whitespace().nth(1).unwrap().to_owned();
    assert!(installed.contains(&blob(&new)));
    assert!(!installed.contains(&blob(&old)), "{installed}");
    assert_eq!(rotated["key"]["retiring_machines"], 0);
    // Reconnecting uses the new key.
    ui.ok("machines.reconnect", json!({"machine_id": machine}))
        .await;
    ui.wait("reconnected with the new key", |m| {
        m.connected_count.get(&machine) == Some(&2)
    })
    .await;
    let keys = ui.ok("keys.list", json!({})).await;
    assert_eq!(keys["controlroom"]["public_key"], json!(new));
    assert_eq!(keys["controlroom"]["retiring_machines"], 0);
    setup.controller.shutdown().await;
}

/// Approvals on a global session: server-owned attention, one push per device per request
/// (none to a device focused on the session), metadata-only payloads, and the answering
/// device recorded by the controller. (The first answer winning is the agent daemon's rule,
/// tested there.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_push_once_per_device_and_the_answer_is_attributed() {
    let push = std::sync::Arc::new(support::RecordingPush::default());
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(
        &dir.path().join("controller.sqlite"),
        Some(bundled.path()),
        Some(push.config()),
    );
    config.agentd_socket = Some(agentd.socket().to_owned());
    let controller = controlroom_controller::Controller::start(config).unwrap();
    let host = RemoteHost::start().await;
    let mut laptop = ui(&controller, "laptop").await;
    let mut phone = ui(&controller, "phone").await;
    for (ui, (device, n)) in [(&mut laptop, ("laptop", 1)), (&mut phone, ("phone", 2))] {
        ui.ok(
            "notifications.subscribe",
            json!({"device_id": device, "subscription": support::browser_subscription(n)}),
        )
        .await;
    }
    agentd.wait_connected(1).await;
    let machine = add_machine(&mut laptop, &host, "devbox").await;
    let id = SessionId::generate();
    let mut session = session_summary(
        id,
        &[WorkspaceSpec {
            machine_id: machine.parse().unwrap(),
            path: "~/src".into(),
        }],
    );
    session.name = Some("cleanup".into());
    agentd.upsert_session(session.clone());
    phone
        .wait("the session", |m| m.sessions.contains_key(&id.to_string()))
        .await;
    laptop
        .ok(
            "client.focus",
            json!({"device_id": "laptop", "visible": true, "session_id": id}),
        )
        .await;

    for round in 1..=2usize {
        let request: Request = serde_json::from_value(json!({
            "id": format!("r{round}"), "kind": "permission", "blocking": true, "state": "pending",
            "title": "Run rm -rf build?", "detail": null, "body": null, "answer": null,
            "origin_child": null, "options": [{"id": "0", "label": "Allow", "style": "allow"}],
            "questions": [], "created_at": round,
        }))
        .unwrap();
        session.requests = vec![request];
        session.attention = Some(Attention::Approval);
        agentd.upsert_session(session.clone());
        // The same summary again is no new occurrence.
        session.preview = format!("waiting {round}");
        agentd.upsert_session(session.clone());
        phone
            .wait("an approval is pending", |m| {
                m.sessions[&id.to_string()]["attention"] == "approval"
            })
            .await;
        wait_until("the push", || push.sent().len() >= round).await;
        phone
            .ok(
                "request.respond",
                json!({"op_id": op(), "id": id, "request_id": format!("r{round}"),
                       "response": {"kind": "choice", "option_id": "0"},
                       // Never trusted: the controller records the answering connection's device.
                       "by": {"device_id": "laptop", "label": "Not me"}}),
            )
            .await;
        let answered = agentd.calls("request.respond").pop().unwrap();
        assert_eq!(
            answered["by"],
            json!({"device_id": "phone", "label": "phone (Firefox on Linux)"})
        );
        session.requests = Vec::new();
        session.attention = None;
        agentd.upsert_session(session.clone());
    }
    let sent = push.sent();
    assert_eq!(sent.len(), 2, "one push per request: {sent:#?}");
    assert!(
        sent.iter().all(|(device, _)| device == "phone"),
        "the focused laptop is skipped"
    );
    let (_, payload) = &sent[0];
    assert_eq!(payload["state"], "approval");
    assert_eq!(payload["name"], "cleanup");
    assert_eq!(payload["machine"], "devbox");
    assert_eq!(payload["session_id"], json!(id));
    assert!(payload.get("machine_id").is_none());
    assert_eq!(payload["url"], json!(format!("#/s/{id}")));
    assert!(!payload.to_string().contains("rm -rf"), "metadata only");
    controller.shutdown().await;
}
