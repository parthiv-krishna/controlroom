//! A session's agent restart through the controller: `session.restart` passes through to the
//! agent daemon with the session's refreshed sandbox defaults filled in (a client's value is
//! replaced), and every restart is logged with the device that asked for it.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use controlroom_controller::test_support::{FakeAgentd, op, session_summary};
use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::WorkspaceSpec;
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
async fn restarts_pass_through_with_the_defaults_and_are_logged() {
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
    let id = SessionId::generate();
    let workspace = WorkspaceSpec {
        machine_id: MachineId::generate(),
        path: "~/src".into(),
    };
    agentd.upsert_session(session_summary(id, &[workspace]));
    let session = id.to_string();
    laptop
        .wait("the session is in the feed", |m| {
            m.sessions.contains_key(&session)
        })
        .await;

    // A client's defaults are replaced by the controller's (strict: no profile set).
    let off = json!({"level": "off", "grants": [], "domains": [], "mediated": []});
    laptop
        .ok(
            "session.restart",
            json!({"op_id": op(), "id": id, "sandbox": off}),
        )
        .await;
    let sent = agentd.calls("session.restart");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["id"], session);
    assert_eq!(sent[0]["sandbox"]["level"], "strict");

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let restarts: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("session agent restarted"))
        .collect();
    assert_eq!(restarts.len(), 1, "{text}");
    assert!(
        restarts[0].contains(&session) && restarts[0].contains("laptop (Firefox on Linux)"),
        "{}",
        restarts[0]
    );
}
