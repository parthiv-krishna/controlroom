//! Terminal stream WebSockets: the same Origin and login rules as `/api/ws`, 404 for unknown
//! shells, and the bridge's frames (a stream whose shell is on a removed machine ends at once).

use controlroom_controller::test_support::{FakeAgentd, session_summary};
use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::{Shell, ShellOwner, ShellSandbox, ShellState, WorkspaceSpec};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use super::common::*;

fn path(session: &str, shell: &str) -> String {
    format!("/api/stream/terminal/{session}/{shell}")
}

#[tokio::test]
async fn terminal_upgrades_need_our_origin_a_login_and_a_shell() {
    let s = dev_server().await;
    let cookie = dev_login(&s).await;
    let shell = path(&SessionId::generate().to_string(), "devbox");
    for origin in [None, Some("https://evil.example.com"), Some("null")] {
        let refused = ws_connect_to(&s, &shell, origin, Some(&cookie)).await.err();
        assert_eq!(refused, Some(403), "{origin:?}");
    }
    let origin = s.origin.clone();
    assert_eq!(
        ws_connect_to(&s, &shell, Some(&origin), None).await.err(),
        Some(401)
    );
    assert_eq!(
        ws_connect_to(
            &s,
            &shell,
            Some(&origin),
            Some("controlroom_session=forged")
        )
        .await
        .err(),
        Some(401)
    );
    // Signed in: no such session (or a malformed id) is a 404.
    assert_eq!(
        ws_connect_to(&s, &shell, Some(&origin), Some(&cookie))
            .await
            .err(),
        Some(404)
    );
    assert_eq!(
        ws_connect_to(&s, &path("nope", "devbox"), Some(&origin), Some(&cookie))
            .await
            .err(),
        Some(404)
    );
    s.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stream_sends_events_as_text_and_ends_with_a_normal_close() {
    let agentd = FakeAgentd::start().await;
    let extra = format!("agentd_socket = {:?}", agentd.socket());
    let s = dev_server_with(state_dir(), &extra).await;
    agentd.wait_connected(1).await;

    // A session whose shell's machine isn't (or no longer) known to the controller.
    let session = SessionId::generate();
    let mut summary = session_summary(
        session,
        &[WorkspaceSpec {
            machine_id: MachineId::generate(),
            path: "/home/alice/src".into(),
        }],
    );
    summary.shells.push(Shell {
        name: "dev box".into(),
        workspace_id: summary.workspaces[0].id,
        owner: ShellOwner::Agent,
        sandbox: ShellSandbox::Workspace,
        cwd: "/home/alice/src".into(),
        state: ShellState::Idle,
        reset_pending: false,
        foreground: None,
    });
    agentd.upsert_session(summary);

    let cookie = dev_login(&s).await;
    let origin = s.origin.clone();
    let shell = path(&session.to_string(), "dev%20box");
    // The controller learns about the session through the agent daemon's feed.
    let mut ws = loop {
        match ws_connect_to(&s, &shell, Some(&origin), Some(&cookie)).await {
            Ok(ws) => break ws,
            Err(404) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            Err(status) => panic!("{status}"),
        }
    };
    let Some(Message::Text(text)) = ws.next().await else {
        panic!("expected a text event");
    };
    let event: Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(
        event,
        json!({"kind": "ended", "reason": "its machine was removed"})
    );
    assert_eq!(ws.closed().await, 1000);
    s.server.shutdown().await.unwrap();
}
