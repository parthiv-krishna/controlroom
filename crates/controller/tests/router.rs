//! The host router end to end: a fake agent daemon publishes host requests; the controller
//! carries them over SSH (`controlroomd connect`) to a fake host daemon and answers them, resends
//! unanswered ones after a reconnect, forwards host notifications, and reports its machines.

mod support;

use controlroom_controller::test_support::Relay;
use controlroom_controller::test_support::{FakeAgentd, FakeHost, RemoteHost, bundled_daemons};
use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{
    HostNotification, HostOp, HostRequest, ToolCallRef, ToolRunParams,
};
use controlroom_protocol::ids::{CallId, HostOpId, MachineId, WorkspaceId};
use controlroom_protocol::ui::Connectivity;
use controlroom_protocol::{ErrorData, RpcOutcome};
use serde_json::{Value, json};
use support::{add_machine, add_machine_at, controller_with_agentd, ui};

/// A `tool.run` request: `bash` (held while the command starts with `hold`).
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

fn result(outcome: RpcOutcome<Value>) -> Value {
    match outcome {
        RpcOutcome::Result(v) => v,
        RpcOutcome::Error(e) => panic!("expected a result, got {e:?}"),
    }
}

fn error(outcome: RpcOutcome<Value>) -> ErrorData {
    match outcome {
        RpcOutcome::Error(e) => e.data.unwrap(),
        RpcOutcome::Result(v) => panic!("expected an error, got {v}"),
    }
}

fn machine_id(id: &str) -> MachineId {
    id.parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_requests_are_routed_and_answered_verbatim() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let fake = FakeHost::start(&host);
    let mut ui = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let id = machine_id(&add_machine(&mut ui, &host, "devbox").await);

    // The machine report: name, availability, the host's epoch and the default grace.
    let machines = agentd
        .wait_machines("devbox is reported connected", |m| {
            m.iter()
                .any(|m| m.machine_id == id && m.availability.is_connected())
        })
        .await;
    let devbox = machines.iter().find(|m| m.machine_id == id).unwrap();
    assert_eq!(devbox.name, "devbox");
    assert_eq!(devbox.epoch, Some(fake.epoch()));
    assert_eq!(devbox.grace_s, 600);

    // A tool call runs on the host once and its result comes back verbatim.
    agentd.request(bash(id, "c1", "true"));
    let answer = result(agentd.response(&HostOpId::new("c1")).await);
    assert_eq!(answer["call_id"], "c1");
    assert_eq!(answer["ran"], 1);
    assert_eq!(fake.executions("c1"), 1);

    // A host error comes back verbatim too.
    agentd.request(HostOpRequest {
        id: HostOpId::new("a1"),
        machine_id: id,
        session_id: None,
        request: HostRequest::ToolAttach(ToolCallRef {
            call_id: CallId::new("never"),
        }),
    });
    assert!(matches!(
        error(agentd.response(&HostOpId::new("a1")).await),
        ErrorData::NotFound { .. }
    ));

    // An unknown machine is `not_found {machine}`.
    agentd.request(bash(MachineId::generate(), "x1", "true"));
    match error(agentd.response(&HostOpId::new("x1")).await) {
        ErrorData::NotFound { resource, .. } => {
            assert_eq!(resource, controlroom_protocol::error::Resource::Machine)
        }
        other => panic!("{other:?}"),
    }

    // Host notifications are forwarded with their machine, in order, before the result.
    agentd.request(bash(id, "c2", "hold"));
    fake.wait_started("c2").await;
    fake.output("c2", "one");
    fake.output("c2", "two");
    fake.release("c2");
    result(agentd.response(&HostOpId::new("c2")).await);
    let outputs: Vec<String> = agentd
        .notifies()
        .into_iter()
        .filter(|n| n.machine_id == id)
        .filter_map(|n| match n.notification {
            HostNotification::ToolOutput(p) if p.call_id.as_str() == "c2" => Some(p.text),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, ["one", "two"]);

    // A withdrawn request is never answered.
    agentd.request(bash(id, "w1", "hold"));
    fake.wait_started("w1").await;
    agentd.withdraw(&HostOpId::new("w1"));
    fake.release("w1");
    agentd.request(bash(id, "c3", "true"));
    result(agentd.response(&HostOpId::new("c3")).await);
    assert!(agentd.responses(&HostOpId::new("w1")).is_empty());

    // The UI's fs.list goes to the host daemon.
    let listing = ui
        .ok(
            "fs.list",
            json!({"machine_id": id, "path": "~/src", "include_files": false}),
        )
        .await;
    assert_eq!(listing["path"], "~/src");
    assert_eq!(fake.received("fs.list").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unanswered_requests_are_resent_after_a_reconnect() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let fake = FakeHost::start(&host);
    let relay = Relay::start(host.port()).await;
    let mut ui = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let id = machine_id(&add_machine_at(&mut ui, &host, "devbox", relay.port).await);
    let first = agentd
        .wait_machines("devbox is reported connected", |m| {
            m.iter().any(|m| m.availability.is_connected())
        })
        .await[0]
        .clone();

    // A command runs when the link drops; it finishes on the host meanwhile.
    agentd.request(bash(id, "c1", "hold"));
    fake.wait_started("c1").await;
    relay.down();
    agentd
        .wait_machines("devbox is reported unreachable", |m| {
            !m[0].availability.is_connected()
        })
        .await;
    fake.release("c1");
    assert!(agentd.responses(&HostOpId::new("c1")).is_empty());

    // After the reconnect the call is sent again and answered from the host's journal: it ran
    // once.
    relay.up();
    let answer = result(agentd.response(&HostOpId::new("c1")).await);
    assert_eq!(answer["ran"], 1);
    assert_eq!(fake.executions("c1"), 1);
    assert_eq!(fake.received("tool.run").len(), 2, "resent once");
    let back = agentd
        .wait_machines("devbox is reported connected again", |m| {
            m[0].availability.is_connected()
        })
        .await;
    assert!(
        back[0].availability.since > first.availability.since,
        "a new connection"
    );
    assert_eq!(back[0].epoch, first.epoch, "the same host daemon");
    assert_eq!(agentd.responses(&HostOpId::new("c1")).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_restart_reports_a_new_epoch_and_lost_calls() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let fake = FakeHost::start(&host);
    let mut ui = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let id = machine_id(&add_machine(&mut ui, &host, "devbox").await);
    let old = fake.epoch();
    agentd
        .wait_machines("the first epoch", |m| m[0].epoch == Some(old))
        .await;

    agentd.request(bash(id, "c1", "hold"));
    fake.wait_started("c1").await;
    fake.restart();
    let new = fake.epoch();
    agentd
        .wait_machines("the new epoch", |m| {
            m[0].epoch == Some(new) && m[0].availability.is_connected()
        })
        .await;
    assert!(matches!(
        error(agentd.response(&HostOpId::new("c1")).await),
        ErrorData::Lost { may_have_run: true }
    ));
    assert_eq!(fake.executions("c1"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agentd_reconnect_keeps_calls_with_their_host() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let fake = FakeHost::start(&host);
    let mut ui = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let id = machine_id(&add_machine(&mut ui, &host, "devbox").await);

    agentd.request(bash(id, "c1", "hold"));
    fake.wait_started("c1").await;
    agentd.drop_connection();
    agentd.wait_connected(2).await;
    // After the new hello the agent daemon gets a fresh machine report.
    agentd
        .wait_machines("a report after the reconnect", |m| {
            m[0].availability.is_connected()
        })
        .await;
    fake.release("c1");
    let answer = result(agentd.response(&HostOpId::new("c1")).await);
    assert_eq!(answer["ran"], 1);
    assert_eq!(fake.received("tool.run").len(), 1, "never sent twice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn machine_names_and_the_grace_setting_reach_the_agent_daemon() {
    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let _fake = FakeHost::start(&host);
    let mut ui = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let id = add_machine(&mut ui, &host, "devbox").await;
    assert_eq!(ui.mirror.machine(&id)["grace_s"], 600);

    for bad in ["dev:box", "dev box", "a/b", ""] {
        ui.err(
            "machines.set_settings",
            json!({"machine_id": id, "name": bad}),
            "invalid_params",
        )
        .await;
    }
    ui.err(
        "machines.set_settings",
        json!({"machine_id": id, "grace_s": 8 * 24 * 3600}),
        "invalid_params",
    )
    .await;
    let machine = ui
        .ok(
            "machines.set_settings",
            json!({"machine_id": id, "name": "builder", "grace_s": 120}),
        )
        .await;
    assert_eq!(
        (machine["name"].clone(), machine["grace_s"].clone()),
        (json!("builder"), json!(120))
    );
    let reported = agentd
        .wait_machines("the new name and grace", |m| {
            m[0].name == "builder" && m[0].grace_s == 120
        })
        .await;
    assert_eq!(reported.len(), 1);
    assert_eq!(
        reported[0].availability.connectivity,
        Connectivity::Connected
    );

    // Removing the machine reports it gone.
    ui.ok("machines.remove", json!({"machine_id": id})).await;
    agentd.wait_machines("no machines", <[_]>::is_empty).await;
}

/// The router with the real host daemon (`controlroomd serve`, installed by the controller):
/// a workspace and a shell are opened, a command runs, and a command that is running when the
/// link drops finishes on the host and is answered from its journal after the reconnect (it ran
/// once).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_real_host_daemon_runs_routed_calls_across_a_link_drop() {
    use controlroom_protocol::host::{ShellOpenParams, WorkspaceOpenParams};
    use controlroom_protocol::ids::SessionId;
    use controlroom_protocol::session::{SandboxLevel, SandboxSpec};

    let agentd = FakeAgentd::start().await;
    let bundled = bundled_daemons();
    let setup = controller_with_agentd(Some(bundled.path()), agentd.socket());
    let host = RemoteHost::start().await;
    let relay = Relay::start(host.port()).await;
    let mut ui = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;
    let id = machine_id(&add_machine_at(&mut ui, &host, "devbox", relay.port).await);
    agentd
        .wait_machines("devbox is a connected host", |m| {
            m.iter()
                .any(|m| m.availability.is_connected() && m.epoch.is_some())
        })
        .await;

    let (session, workspace) = (SessionId::generate(), WorkspaceId::generate());
    let send = |call: &str, request: HostRequest| {
        agentd.request(HostOpRequest {
            id: HostOpId::new(call),
            machine_id: id,
            session_id: Some(session),
            request,
        });
    };
    send(
        "open",
        HostRequest::WorkspaceOpen(WorkspaceOpenParams {
            workspace_id: workspace,
            session_id: session,
            path: host.work().into(),
            sandbox: SandboxSpec {
                level: SandboxLevel::Off,
                grants: Vec::new(),
                domains: Vec::new(),
                mediated: Vec::new(),
            },
        }),
    );
    let opened = result(agentd.response(&HostOpId::new("open")).await);
    assert!(
        opened["root"]
            .as_str()
            .is_some_and(|r| r.ends_with("/work")),
        "{opened}"
    );
    send(
        "shell",
        HostRequest::ShellOpen(ShellOpenParams {
            workspace_id: workspace,
            name: "devbox".into(),
            cwd: None,
        }),
    );
    result(agentd.response(&HostOpId::new("shell")).await);
    let run = |call: &str, command: &str| {
        HostRequest::ToolRun(ToolRunParams {
            call_id: CallId::new(call),
            workspace_id: workspace,
            op: HostOp::Bash {
                shell: "devbox".into(),
                command: command.into(),
                timeout_s: None,
            },
        })
    };
    send("c1", run("c1", "echo hi-$((40+2))"));
    let answer = result(agentd.response(&HostOpId::new("c1")).await);
    assert_eq!(
        (answer["output"].clone(), answer["exit_code"].clone()),
        (json!("hi-42"), json!(0))
    );

    // A command held on a FIFO while the link is down.
    let work = std::path::PathBuf::from(host.work());
    let gate = work.join("gate");
    let ran = work.join("ran.txt");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&gate)
            .status()
            .unwrap()
            .success()
    );
    send(
        "c2",
        run(
            "c2",
            &format!(
                "echo x >> {}; cat {} > /dev/null; echo done",
                ran.display(),
                gate.display()
            ),
        ),
    );
    controlroom_controller::test_support::wait_until("the command started", || ran.exists()).await;
    relay.down();
    agentd
        .wait_machines("devbox is reported unreachable", |m| {
            !m[0].availability.is_connected()
        })
        .await;
    let release = gate.clone();
    tokio::task::spawn_blocking(move || std::fs::write(release, "go\n").unwrap())
        .await
        .unwrap();
    relay.up();
    let answer = result(agentd.response(&HostOpId::new("c2")).await);
    assert_eq!(answer["output"], "done", "{answer}");
    assert_eq!(std::fs::read_to_string(&ran).unwrap(), "x\n", "it ran once");
    assert_eq!(agentd.responses(&HostOpId::new("c2")).len(), 1);
}
