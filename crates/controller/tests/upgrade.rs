//! Automatic daemon upgrades: they wait until no host call is in flight and roll back a bad
//! binary.
//!
//! Only one real `controlroomd` build exists, so the "older" daemon is a wrapper script that
//! reports an older version and runs a copy of the real binary from its own version directory.
#![cfg(unix)]

mod support;

use std::path::Path;
use std::time::Duration;

use controlroom_controller::Controller;
use controlroom_controller::test_support::{
    DAEMON_VERSION, FakeAgentd, FakeHost, RemoteHost, UiDriver, bundled_daemons, bundled_script,
    copy_daemon,
};
use controlroom_protocol::RpcOutcome;
use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{HostOp, HostRequest, ToolRunParams};
use controlroom_protocol::ids::{CallId, HostOpId, MachineId, WorkspaceId};
use serde_json::json;
use support::{add_machine, config, ui, ui_next};

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

fn start(store: &Path, bundled: &Path, agentd: Option<&Path>) -> Controller {
    let mut config = config(store, Some(bundled), None);
    config.agentd_socket = agentd.map(Path::to_owned);
    Controller::start(config).unwrap()
}

async fn machine(ui: &mut UiDriver, id: &str) -> serde_json::Value {
    ui.ok("ping", json!({})).await;
    ui.mirror.machine(id)
}

/// An automatic upgrade waits while a host call is in flight on the machine, then upgrades the
/// idle machine; the call was answered once and the router works on the new daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_upgrade_waits_for_host_calls() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let agentd = FakeAgentd::start().await;
    let host = RemoteHost::start().await;
    let fake = FakeHost::start(&host);

    // An older controller installs the older daemon.
    let old_bundle = old_daemon(&host);
    let old = start(&store, old_bundle.path(), Some(agentd.socket()));
    let mut old_ui = ui(&old, "laptop").await;
    let id = add_machine(&mut old_ui, &host, "devbox").await;
    assert_eq!(host.current_daemon().as_deref(), Some("versions/0.0.0"));
    drop(old_ui);
    old.shutdown().await;

    // A host call waits on the machine when a newer controller connects to it: no upgrade yet.
    let machine_id = id.parse().unwrap();
    agentd.request(bash(machine_id, "c1", "hold"));
    let bundled = bundled_daemons();
    let new = start(&store, bundled.path(), Some(agentd.socket()));
    let mut ui = ui(&new, "laptop").await;
    ui.wait("connected to the old daemon", |m| {
        let machine = m.machine(&id);
        machine["connectivity"]["state"] == "connected" && machine["update_available"] == true
    })
    .await;
    fake.wait_started("c1").await;
    // Several quiet periods pass without an upgrade.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(host.current_daemon().as_deref(), Some("versions/0.0.0"));
    assert_eq!(ui.mirror.connected_count.get(&id), Some(&1), "no reconnect");

    // The call is answered; the idle machine is upgraded.
    fake.release("c1");
    let RpcOutcome::Result(answer) = agentd.response(&HostOpId::new("c1")).await else {
        panic!("c1 failed");
    };
    assert_eq!(answer["ran"], 1);
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
    assert_eq!(fake.received("tool.run").len(), 1, "c1 was sent once");

    // Host calls go through the new daemon.
    agentd.request(bash(machine_id, "c2", "true"));
    assert!(matches!(
        agentd.response(&HostOpId::new("c2")).await,
        RpcOutcome::Result(_)
    ));
    new.shutdown().await;
}

/// A `bash` host call.
fn bash(machine: MachineId, call: &str, command: &str) -> HostOpRequest {
    HostOpRequest {
        id: HostOpId::new(call),
        machine_id: machine,
        session_id: None,
        request: HostRequest::ToolRun(ToolRunParams {
            call_id: CallId::new(call),
            workspace_id: WorkspaceId::generate(),
            op: HostOp::Bash {
                shell: "devbox".into(),
                command: command.into(),
                timeout_s: None,
            },
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_upgrade_rolls_back_and_keeps_the_machine_usable() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("controller.sqlite");
    let host = RemoteHost::start().await;
    let bundled = bundled_daemons();
    let first = start(&store, bundled.path(), None);
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
    let second = start(&store, bad.path(), None);
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
    // The machine still works: its host daemon answers.
    let mut next = ui_next(&second, "laptop").await;
    next.ok("fs.list", json!({"machine_id": id, "path": "~"}))
        .await;
    second.shutdown().await;
}
