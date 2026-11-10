//! The user's own terminals through the controller: `session.terminal_open` and
//! `session.terminal_close` pass through to the agent daemon with the calling device filled in
//! (a client's `by` is dropped), and every opening and closing is logged with that device.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use controlroom_controller::test_support::FakeAgentd;
use controlroom_protocol::ids::{SessionId, WorkspaceId};
use serde_json::json;
use support::{controller_with_agentd, ui};

/// The log, captured.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminals_pass_through_with_the_device_and_are_logged() {
    let log = Log::default();
    let writer = log.clone();
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .init();

    let agentd = FakeAgentd::start().await;
    let setup = controller_with_agentd(None, agentd.socket());
    let mut laptop = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;

    let session = SessionId::generate();
    laptop
        .ok(
            "session.terminal_open",
            json!({
                "op_id": "op-1",
                "id": session,
                "workspace_id": WorkspaceId::generate(),
                "sandbox": "unsandboxed",
                "confirm": true,
                "by": {"device_id": "evil", "label": "x"},
            }),
        )
        .await;
    let sent = agentd.calls("session.terminal_open");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["by"]["device_id"], "laptop");
    assert_eq!(sent[0]["confirm"], true);
    laptop
        .ok(
            "session.terminal_close",
            json!({"op_id": "op-2", "id": session, "name": "you@devbox"}),
        )
        .await;
    assert_eq!(agentd.calls("session.terminal_close").len(), 1);

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let opened = text
        .lines()
        .find(|l| l.contains("user terminal opened"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        opened.contains(&session.to_string()) && opened.contains("laptop (Firefox on Linux)"),
        "{opened}"
    );
    let closed = text
        .lines()
        .find(|l| l.contains("user terminal closed"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        closed.contains("you@devbox") && closed.contains("laptop (Firefox on Linux)"),
        "{closed}"
    );
}
