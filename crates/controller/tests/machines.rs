//! End to end with real machines: the controller adds fake remote machines over SSH
//! (client-core's in-process server running exec requests locally), installs the built
//! `controlroomd` from a bundled-daemons directory, and drives sessions with the fake agent
//! through the UI API.
#![cfg(unix)]

mod support;

use std::time::Duration;

use controlroom_controller::test_support::{
    DAEMON_VERSION, RemoteHost, bundled_daemons, gated, op, text, texts,
};
use serde_json::{Value, json};
use support::{add_machine, controller, create_session, ui};

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
    assert!(machine["settings"]["idle_timeout_h"].is_number());

    // The same host can't be added twice.
    ui.err("machines.add", new, "invalid_params").await;

    // Pass-through: agents and a directory listing.
    let agents = ui.ok("agents.list", json!({"machine_id": id})).await;
    assert!(agents["agents"].is_array());
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
        "session.cancel",
        json!({"machine_id": id, "op_id": op(), "id": "01HZY3K6W8Q9X2V4N7B5C1D3E6"}),
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

/// The atomic create (session, task links, first prompt), two tabs seeing the same live state,
/// shared session subscriptions without duplicate text, and the prompt origin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_tabs_run_a_session_end_to_end() {
    let bundled = bundled_daemons();
    let setup = controller(Some(bundled.path()), None);
    let mut host = RemoteHost::start().await;
    host.agent(
        "fake",
        json!({"replies": [
            {"content": [{"type": "text", "text": "hello there"}]},
            gated("streaming ", "hold", "done"),
        ]}),
    );
    let mut a = ui(&setup.controller, "laptop").await;
    let mut b = ui(&setup.controller, "phone").await;
    let machine = add_machine(&mut a, &host, "devbox").await;
    b.wait("the other tab sees the machine", |m| {
        m.machine(&machine)["connectivity"]["state"] == "connected"
    })
    .await;

    let task = a
        .ok("tasks.create", json!({"title": "greet", "status": "todo"}))
        .await;
    let task_id = task["id"].as_str().unwrap().to_owned();
    let session = create_session(
        &mut a,
        &machine,
        &host,
        json!({"prompt": {"content": text("say hello")}, "task_ids": [task_id], "name": "greeter"}),
    )
    .await;
    b.wait("the first run finishes, linked to the task", |m| {
        let s = m.session(&session);
        let Some(t) = m.tasks.get(&task_id) else {
            return false;
        };
        s["state"] == "idle"
            && s["last_outcome"] == "completed"
            && s["stale"] == false
            && t["status"] == "doing"
            && t["links"][0]["session_id"] == json!(session)
    })
    .await;

    // Both tabs subscribe to the timeline; the second shares the daemon subscription.
    let sub_a = a
        .ok(
            "session.subscribe",
            json!({"machine_id": machine, "id": session, "since_seq": 0}),
        )
        .await;
    let sub_b = b
        .ok(
            "session.subscribe",
            json!({"machine_id": machine, "id": session, "since_seq": 0}),
        )
        .await;
    for (ui, sub) in [(&mut a, &sub_a), (&mut b, &sub_b)] {
        let backlog = ui
            .notification("the backlog", |n| {
                n["method"] == "session.events"
                    && n["params"]["subscription_id"] == sub["subscription_id"]
            })
            .await;
        assert_eq!(backlog["params"]["machine_id"], json!(machine));
        let all = backlog["params"]["ops"].to_string();
        assert!(all.contains("hello there"), "{all}");
    }

    // A prompt from the phone streams to both tabs; the laptop queues another meanwhile.
    b.ok(
        "session.prompt",
        json!({"machine_id": machine, "op_id": op(), "id": session, "content": text("go"),
               "origin": {"kind": "schedule", "schedule_id": "01HZY3K6W8Q9X2V4N7B5C1D3E6", "name": "x"}}),
    )
    .await;
    host.wait_gate("hold").await;
    let queued = a
        .ok(
            "session.prompt",
            json!({"machine_id": machine, "op_id": op(), "id": session, "content": text("next")}),
        )
        .await;
    assert_eq!(queued["outcome"], "queued", "{queued}");
    // Queue items carry their origin: the user on the sending device, whatever a client sent.
    let queued_text = |n: &Value, text: &str| -> Option<Value> {
        n["params"]["detail"]["queue"]
            .as_array()?
            .iter()
            .find(|item| item["content"][0]["text"] == text)
            .map(|item| item["origin"].clone())
    };
    let detail = a
        .notification("the queued prompt in the detail", |n| {
            queued_text(n, "next").is_some()
        })
        .await;
    let origin = queued_text(&detail, "next").unwrap();
    assert_eq!(origin["kind"], "user");
    assert_eq!(origin["device"]["device_id"], "laptop");
    assert_eq!(origin["device"]["label"], "laptop (Firefox on Linux)");
    let phone = a
        .notifications
        .iter()
        .chain(b.notifications.iter())
        .find_map(|n| queued_text(n, "go"));
    if let Some(origin) = phone {
        assert_eq!(origin["kind"], "user", "the schedule origin was replaced");
        assert_eq!(origin["device"]["device_id"], "phone");
    }
    host.release("hold");
    for ui in [&mut a, &mut b] {
        ui.wait("both runs finish", |m| {
            let s = m.session(&session);
            s["state"] == "idle" && s["queue"]["len"] == 0
        })
        .await;
    }
    // Each tab got the streamed text exactly once.
    let streamed = |n: &Value| {
        n["method"] == "session.events"
            && n["params"]["ops"].as_array().is_some_and(|ops| {
                ops.iter()
                    .any(|op| op["op"] == "append_text" && op["text"] == "done")
            })
    };
    for ui in [&mut a, &mut b] {
        ui.notification("the streamed text", streamed).await;
        assert!(!ui.notifications.iter().any(streamed), "streamed twice");
    }
    let unsubscribed = a
        .ok(
            "unsubscribe",
            json!({"subscription_id": sub_a["subscription_id"]}),
        )
        .await;
    assert_eq!(unsubscribed, json!({}));
    setup.controller.shutdown().await;
}

/// The prototype's lost update: a UI settings write racing a daemon-settings write. Writes are
/// column-targeted now, so both survive any interleaving.
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
    let settings = async {
        for i in 0..rounds {
            b.ok(
                "settings.set",
                json!({"machine_id": machine, "op_id": op(), "idle_timeout_h": 10 + i}),
            )
            .await;
        }
    };
    tokio::join!(names, settings);
    a.wait("both writes kept", |m| {
        let machine = m.machine(&machine);
        machine["name"] == format!("box-{}", rounds - 1)
            && machine["settings"]["idle_timeout_h"] == 10 + rounds - 1
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
    let mut host = RemoteHost::start().await;
    host.agent("fake", texts(&["fine"]));
    let mut ui = ui(&controller, "laptop").await;
    ui.wait("the stuck machine is connecting", |m| {
        m.machine(&stuck)["connectivity"]["state"] == "connecting"
    })
    .await;

    let machine = add_machine(&mut ui, &host, "devbox").await;
    let session = create_session(&mut ui, &machine, &host, json!({"prompt": {"content": text("hi")}})).await;
    ui.wait("the session runs", |m| {
        m.session(&session)["last_outcome"] == "completed"
    })
    .await;
    // Calls to the stuck machine fail at once instead of waiting.
    let started = std::time::Instant::now();
    ui.err(
        "agents.list",
        json!({"machine_id": stuck}),
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
