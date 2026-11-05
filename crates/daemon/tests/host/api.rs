//! The API surface: hello and the single-controller rule, workspaces (context files, skills,
//! close), `fs.list`, `doctor.report`, unknown ops, and sandbox approvals via the webhook.

use std::io::{Read, Write};
use std::net::TcpStream;

use controlroom_protocol::agentd::{ClientRole, HelloParams};
use controlroom_protocol::host::{
    ApprovalRespondParams, HostApproval, ShellListParams, WorkspaceCloseParams, methods,
};
use controlroom_protocol::host::{CheckStatus, FsListParams};
use controlroom_protocol::jsonrpc::NoParams;
use controlroom_protocol::session::{RequestBody, SandboxLevel};
use controlroom_protocol::{Capability, ErrorData, ProtocolVersion};
use serde_json::{Value, json};

use crate::harness::{HostEnv, RpcClient, Session, bash_op, controller_id, write_executable};

#[tokio::test]
async fn hello_and_one_controller() {
    let env = HostEnv::start().await;
    let raw = RpcClient::connect(&env.socket()).await;
    let err = raw
        .call::<methods::ShellList>(ShellListParams { workspace_id: None })
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::HelloRequired)));
    let err = raw
        .call::<methods::Hello>(HelloParams {
            protocol_version: ProtocolVersion { major: 2, minor: 0 },
            client_name: "future".into(),
            role: ClientRole::Debug,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        err.data,
        Some(ErrorData::IncompatibleVersion { .. })
    ));

    let (_first, hello) = env
        .connect(ClientRole::Controller {
            controller_id: controller_id(),
        })
        .await;
    for cap in [Capability::SHELLS, Capability::PTY, Capability::JOBS] {
        assert!(
            hello.capabilities.contains(&cap),
            "{:?}",
            hello.capabilities
        );
    }
    assert!(hello.capabilities.contains(&Capability::SANDBOX_APPROVALS));
    let other = RpcClient::connect(&env.socket()).await;
    let err = other
        .call::<methods::Hello>(HelloParams {
            protocol_version: controlroom_protocol::HOST_PROTOCOL_VERSION,
            client_name: "other".into(),
            role: ClientRole::Controller {
                controller_id: controlroom_protocol::ids::ControllerId::generate(),
            },
        })
        .await
        .unwrap_err();
    assert!(matches!(
        err.data,
        Some(ErrorData::ControllerConflict { .. })
    ));
    // Debug clients connect alongside; approvals are the controller's.
    let (debug, again) = env.connect(ClientRole::Debug).await;
    assert_eq!(
        (again.daemon_id, again.epoch),
        (hello.daemon_id, hello.epoch)
    );
    let err = debug
        .call::<methods::ApprovalsSubscribe>(NoParams {})
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::Unsupported)));
}

#[tokio::test]
async fn workspaces_report_context_files_and_skills_and_close_their_shells() {
    let env = HostEnv::new();
    std::fs::write(env.home().join("AGENTS.md"), "home rules").unwrap();
    std::fs::write(env.project().join("CLAUDE.md"), "project rules").unwrap();
    let skill = env.project().join(".pi/skills/release");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: release\ndescription: Cut a release\n---\n",
    )
    .unwrap();
    let mut env = env;
    env.start_daemon().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let opened = &session.opened;
    let project = std::fs::canonicalize(env.project()).unwrap();
    assert_eq!(opened.root.as_str(), project.to_str().unwrap());
    assert_eq!(opened.home.as_str(), env.home().to_str().unwrap());
    let files: Vec<_> = opened
        .context_files
        .iter()
        .filter(|f| f.path.as_str().starts_with(env.home().to_str().unwrap()))
        .map(|f| f.content.as_str())
        .collect();
    assert_eq!(files, ["home rules", "project rules"]);
    assert_eq!(opened.skills.len(), 1);
    assert_eq!(opened.skills[0].description, "Cut a release");

    // Unknown ops are refused, not failed.
    let rx = client
        .send::<methods::ToolRun>(
            serde_json::from_value(json!({
                "call_id": "u1", "workspace_id": session.workspace, "op": {"op": "teleport"}
            }))
            .unwrap(),
        )
        .await;
    let err = rx.await.unwrap().unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::Unsupported)), "{err:?}");

    session.shell(&client, "s").await;
    client
        .call::<methods::WorkspaceClose>(WorkspaceCloseParams {
            workspace_id: session.workspace,
        })
        .await
        .unwrap();
    while client.note("shell.changed").await["shell"]["state"]["state"] != "gone" {}
    let list = client
        .call::<methods::ShellList>(ShellListParams { workspace_id: None })
        .await
        .unwrap();
    assert!(list.shells.is_empty());
    let err = session
        .run(&client, "c1", bash_op("s", "true", None))
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::NotFound { .. })),
        "{err:?}"
    );
    // Idempotent.
    client
        .call::<methods::WorkspaceClose>(WorkspaceCloseParams {
            workspace_id: session.workspace,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn fs_list_and_doctor() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let listed = client
        .call::<methods::FsList>(FsListParams {
            path: "~".into(),
            include_files: false,
        })
        .await
        .unwrap();
    let names: Vec<_> = listed.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["project"]);
    let report = client
        .call::<methods::DoctorReportMethod>(NoParams {})
        .await
        .unwrap();
    assert!(report.agents.is_empty());
}

/// An installed daemon uses the nono installed next to it; `sandbox.toml` overrides it. The
/// doctor says which one, with its version.
#[tokio::test]
async fn the_host_uses_the_bundled_nono_unless_configured() {
    let mut env = HostEnv::new();
    let fake = |version: &str| {
        let path = env.dir.path().join(format!("nono-{version}"));
        write_executable(
            &path,
            &format!("#!/bin/sh\n[ \"$1\" = --version ] && echo 'nono {version}'\n"),
        );
        path
    };
    let (bundled, other) = (fake("0.80.0"), fake("0.79.0"));
    let installed = env.install(&bundled, "0.80.0");
    let installed = std::fs::canonicalize(installed).unwrap();
    env.start_daemon().await;
    let client = env.controller().await;
    let report = client
        .call::<methods::DoctorReportMethod>(NoParams {})
        .await
        .unwrap();
    assert_eq!(report.nono.status, CheckStatus::Ok, "{:?}", report.nono);
    assert_eq!(report.nono.value.as_deref(), Some("0.80.0"));
    assert_eq!(
        report.nono.detail,
        format!("bundled: {}", installed.display())
    );

    // An explicit `nono` wins; another version is a warning, not a failure.
    let config = env.home().join(".config/controlroom");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("sandbox.toml"), format!("nono = {other:?}\n")).unwrap();
    let report = client
        .call::<methods::DoctorReportMethod>(NoParams {})
        .await
        .unwrap();
    assert_eq!(report.nono.status, CheckStatus::Warn, "{:?}", report.nono);
    assert_eq!(report.nono.value.as_deref(), Some("0.79.0"));
    assert!(
        report.nono.detail.starts_with(&format!(
            "configured in sandbox.toml: {}",
            std::fs::canonicalize(&other).unwrap().display()
        )),
        "{}",
        report.nono.detail
    );
}

/// The webhook's port and a workspace's secret, from the host's state dir.
fn hook(env: &HostEnv, session: &Session) -> (u16, String) {
    let port = std::fs::read_to_string(env.state().join("run/webhook.port"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let secret = std::fs::read_to_string(
        env.state()
            .join(format!("workspaces/{}/hook.secret", session.workspace)),
    )
    .unwrap();
    (port, secret)
}

/// POSTs a nono approval request; returns the HTTP status and body.
fn post(port: u16, path: &str, body: &Value) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = body.to_string();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Nono-Timeout-Secs: 30\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn sandbox_approvals_go_to_the_controller_and_back() {
    let env = HostEnv::start().await;
    let client = env.controller().await;
    let session = Session::open(&env, &client, SandboxLevel::Off).await;
    let subscribed = client
        .call::<methods::ApprovalsSubscribe>(NoParams {})
        .await
        .unwrap();
    assert!(subscribed.pending.is_empty());
    let (port, secret) = hook(&env, &session);
    let path = format!("/nono/v1/{}/{}", session.workspace, secret.trim());
    let request = json!({"backend": "controlroom", "request": {
        "capability_type": "command", "request_id": "1", "command": "git",
        "args": ["git", "push", "origin", "main"]}});

    let pending = {
        let path = path.clone();
        let request = request.clone();
        std::thread::spawn(move || post(port, &path, &request))
    };
    let raised = client.note("approval.request").await;
    let approval: HostApproval = serde_json::from_value(raised["approval"].clone()).unwrap();
    assert_eq!(approval.workspace_id, session.workspace);
    assert_eq!(approval.session_id, session.id);
    assert!(matches!(approval.body, RequestBody::GitPush { ref branch, .. } if branch == "main"));
    client
        .call::<methods::ApprovalRespond>(ApprovalRespondParams {
            approval_id: approval.id,
            option_id: "allow-once".into(),
            reason: None,
        })
        .await
        .unwrap();
    let (status, reply) = pending.join().unwrap();
    assert_eq!((status, reply["decision"].as_str()), (200, Some("granted")));
    // Answering again: it is no longer pending.
    let err = client
        .call::<methods::ApprovalRespond>(ApprovalRespondParams {
            approval_id: approval.id,
            option_id: "deny".into(),
            reason: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::NotFound { .. })));

    // A denial carries the reason.
    let pending = {
        let path = path.clone();
        std::thread::spawn(move || post(port, &path, &request))
    };
    let raised = client.note("approval.request").await;
    let approval: HostApproval = serde_json::from_value(raised["approval"].clone()).unwrap();
    client
        .call::<methods::ApprovalRespond>(ApprovalRespondParams {
            approval_id: approval.id,
            option_id: "deny".into(),
            reason: Some("not main".into()),
        })
        .await
        .unwrap();
    let (_, reply) = pending.join().unwrap();
    assert_eq!(reply["decision"], "denied");
    assert_eq!(reply["reason"], "not main");

    // A wrong secret is refused without asking.
    let wrong = format!("/nono/v1/{}/{}", session.workspace, "ab".repeat(32));
    assert_eq!(post(port, &wrong, &json!({})).0, 403);
}
