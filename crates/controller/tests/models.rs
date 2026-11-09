//! Settings › Models through the controller: every `models.*` method passes through to the agent
//! daemon with the calling device filled in, changes are logged with that device, and an API key
//! reaches the agent daemon once without appearing in a response, an event or the log.

mod support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use controlroom_controller::test_support::FakeAgentd;
use serde_json::json;
use support::{controller_with_agentd, ui};

const KEY: &str = "sk-test-very-secret-4f2a";

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
async fn models_pass_through_with_the_device_and_keys_stay_out_of_logs() {
    let log = Log::default();
    let writer = log.clone();
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .init();

    let agentd = FakeAgentd::start().await;
    let setup = controller_with_agentd(None, agentd.socket());
    let mut laptop = ui(&setup.controller, "laptop").await;
    agentd.wait_connected(1).await;

    let result = laptop
        .ok(
            "models.set_key",
            json!({"provider": "anthropic", "key": KEY, "by": {"device_id": "evil", "label": "x"}}),
        )
        .await;
    let sent = agentd.calls("models.set_key");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["key"], KEY);
    assert_eq!(sent[0]["by"]["device_id"], "laptop");
    assert!(!result.to_string().contains(KEY));

    for (method, params) in [
        ("models.list", json!({})),
        (
            "models.set_gateway",
            json!({"provider": "gw", "key": KEY,
            "gateway": {"base_url": "https://llm.example.com/v1", "models": ["m"], "auth_header": "bearer"}}),
        ),
        ("models.remove", json!({"provider": "anthropic"})),
        ("models.sign_out", json!({"provider": "openai-codex"})),
        ("models.test", json!({"provider": "gw"})),
        (
            "models.set_defaults",
            json!({"defaults": {"provider": "gw", "model": "m", "thinking": null}}),
        ),
        ("models.login_start", json!({"provider": "anthropic"})),
        (
            "models.login_input",
            json!({"login_id": "01HZY3K6W8Q9X2V4N7B5C1D3EB", "input": "https://id.example.com/cb?code=1"}),
        ),
        (
            "models.login_wait",
            json!({"login_id": "01HZY3K6W8Q9X2V4N7B5C1D3EB", "after": 1}),
        ),
        (
            "models.login_cancel",
            json!({"login_id": "01HZY3K6W8Q9X2V4N7B5C1D3EB"}),
        ),
    ] {
        laptop.ok(method, params).await;
        let sent = agentd.calls(method);
        assert_eq!(sent.len(), 1, "{method}");
        let filled = sent[0].get("by").map(|by| by["device_id"].clone());
        let expected =
            (!matches!(method, "models.list" | "models.login_wait")).then(|| json!("laptop"));
        assert_eq!(filled, expected, "{method}");
    }
    // Malformed params never reach the agent daemon.
    laptop
        .err(
            "models.set_key",
            json!({"provider": "anthropic"}),
            "invalid_params",
        )
        .await;
    assert_eq!(agentd.calls("models.set_key").len(), 1);

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("Settings › Models change"), "{text}");
    assert!(text.contains("laptop (Firefox on Linux)"), "{text}");
    assert!(!text.contains(KEY), "the key was logged: {text}");
    assert!(
        laptop
            .notifications
            .iter()
            .all(|n| !n.to_string().contains(KEY)),
        "an event carried the key"
    );
}
