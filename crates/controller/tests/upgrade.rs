//! Automatic daemon upgrades: they wait until no run is in flight, keep the sessions running,
//! and roll back a bad binary.
//!
//! Only one real `controlroomd` build exists, so the "older" daemon is a wrapper script that
//! reports an older version and runs a copy of the real binary from its own version directory.
#![cfg(unix)]

mod support;

use std::path::Path;
use std::time::Duration;

use controlroom_controller::Controller;
use controlroom_controller::test_support::{
    DAEMON_VERSION, RemoteHost, UiDriver, bundled_daemons, bundled_script, copy_daemon, gated, op,
    text,
};
use serde_json::json;
use support::{add_machine, config, create_session, ui};

const OLD: &str = "0.0.0";

/// Bundles a wrapper reporting [`OLD`] that runs the real daemon from
/// `versions/old-real/` on the host (so the stop script recognizes its `serve`).
fn old_daemon(host: &RemoteHost) -> tempfile::TempDir {
    let real = host
        .home()
        .join(".local/share/controlroom/versions/old-real");
    std::fs::create_dir_all(&real).unwrap();
    copy_daemon(&real.join("controlroomd"));
    bundled_script(
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'controlroomd {OLD}'; exit 0; fi\n\
             exec \"$HOME/.local/share/controlroom/versions/old-real/controlroomd\" \"$@\"\n"
        ),
        OLD,
    )
}

fn start(store: &Path, bundled: &Path) -> Controller {
    Controller::start(config(store, Some(bundled), None)).unwrap()
}

async fn machine(ui: &mut UiDriver, id: &str) -> serde_json::Value {
    ui.ok("ping", json!({})).await;
    ui.mirror.machine(id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_upgrade_waits_for_the_run_and_keeps_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let mut host = RemoteHost::start().await;
    host.agent(
        "fake",
        json!({"replies": [gated("working ", "hold", "done")]}),
    );

    // An older controller installs the older daemon and starts a run that waits at a gate.
    let old_bundle = old_daemon(&host);
    let old = start(&store, old_bundle.path());
    let mut old_ui = ui(&old, "laptop").await;
    let id = add_machine(&mut old_ui, &host, "devbox").await;
    assert_eq!(host.current_daemon().as_deref(), Some("versions/0.0.0"));
    assert_eq!(machine(&mut old_ui, &id).await["update_available"], false);
    let session = create_session(
        &mut old_ui,
        &id,
        &host,
        json!({"prompt": {"content": text("work")}}),
    )
    .await;
    host.wait_gate("hold").await;
    drop(old_ui);
    old.shutdown().await;

    // A newer controller connects to the busy machine and doesn't upgrade it yet.
    let bundled = bundled_daemons();
    let new = start(&store, bundled.path());
    let mut ui = ui(&new, "laptop").await;
    ui.wait("connected to the old daemon, still working", |m| {
        let machine = m.machine(&id);
        machine["connectivity"]["state"] == "connected"
            && machine["update_available"] == true
            && m.session(&session)["state"] == "working"
    })
    .await;
    // Several quiet periods pass without an upgrade.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(host.current_daemon().as_deref(), Some("versions/0.0.0"));
    assert_eq!(ui.mirror.connected_count.get(&id), Some(&1), "no reconnect");
    let supervisors = host.processes(" supervise ");
    assert_eq!(supervisors.len(), 1, "one supervisor runs the agent");

    // The run ends; the idle machine is upgraded.
    host.release("hold");
    ui.wait("upgraded", |m| {
        let machine = m.machine(&id);
        machine["connectivity"]["state"] == "connected"
            && machine["update_available"] == false
            && m.connected_count.get(&id) == Some(&2)
    })
    .await;
    assert_eq!(
        host.current_daemon().as_deref(),
        Some(format!("versions/{DAEMON_VERSION}").as_str())
    );
    assert!(
        host.processes("versions/old-real/controlroomd serve")
            .is_empty(),
        "the old daemon stopped"
    );
    assert_eq!(
        host.processes(" supervise "),
        supervisors,
        "the agent kept running"
    );

    // The session lives on under the new daemon.
    ui.ok(
        "session.prompt",
        json!({"machine_id": id, "op_id": op(), "id": session, "content": text("again")}),
    )
    .await;
    ui.wait("the next run finishes", |m| {
        let s = m.session(&session);
        s["state"] == "idle" && s["queue"]["len"] == 0 && s["last_outcome"] == "completed"
    })
    .await;
    new.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_upgrade_rolls_back_and_keeps_the_machine_usable() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let host = RemoteHost::start().await;
    let bundled = bundled_daemons();
    let first = start(&store, bundled.path());
    let mut first_ui = ui(&first, "laptop").await;
    let id = add_machine(&mut first_ui, &host, "devbox").await;
    drop(first_ui);
    first.shutdown().await;

    // A "newer" daemon that passes the version check but can't serve.
    let bad = bundled_script(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'controlroomd 99.0.0'; exit 0; fi\n\
         echo broken >&2; exit 1\n",
        "99.0.0",
    );
    let second = start(&store, bad.path());
    let mut ui = ui(&second, "laptop").await;
    ui.wait("the failed upgrade is reported on the old daemon", |m| {
        let machine = m.machine(&id);
        machine["connectivity"]["state"] == "connected"
            && machine["last_error"]
                .as_str()
                .is_some_and(|e| e.contains("automatic upgrade failed"))
    })
    .await;
    assert_eq!(
        host.current_daemon().as_deref(),
        Some(format!("versions/{DAEMON_VERSION}").as_str()),
        "rolled back"
    );
    let machine = machine(&mut ui, &id).await;
    assert_eq!(machine["update_available"], true);
    assert_eq!(machine["version"], DAEMON_VERSION);
    // The machine still works.
    ui.ok("agents.list", json!({"machine_id": id})).await;
    second.shutdown().await;
}
