//! Remote tools end to end: the fake agent plays @controlroom/pi and calls `session.info`,
//! `host.call` and `shell.list` on its agent socket; the agent daemon routes the host call
//! through the host bridge to the fake controller, whose host runs it, and the result comes
//! back as the tool's result. The session's workspace and shell are on the summary.

use serde_json::{Value, json};

use crate::harness::*;

/// A tool call that calls the agent API (the fake agent's stand-in for the extension).
fn agent_tool(id: &str, name: &str, method: &str, params: Value) -> Value {
    json!({"content": [{"type": "tool_call", "id": id, "name": name, "args": params.clone(),
        "agent": {"method": method, "params": params}}]})
}

fn say(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

/// The output of the session's tool call `id`, from its timeline.
fn tool_output(client: &Client, session: &str, id: &str) -> String {
    let (result, ops) = client.session_ops(session, 0);
    client.call(
        "unsubscribe",
        json!({"subscription_id": result["subscription_id"]}),
    );
    ops.iter()
        .rev()
        .find(|op| op["item"]["tool_call_id"] == id)
        .and_then(|op| op["item"]["output"]["text"].as_str())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn the_extension_runs_bash_on_the_workspaces_host() {
    let scenario = json!({"replies": [
        agent_tool("call_info", "info", "session.info", json!({})),
        agent_tool("call_bash", "bash", "host.call", json!({
            "call_id": "call_bash",
            "machine": "devbox",
            "op": {"op": "bash", "shell": "devbox", "command": "echo from-the-host"},
        })),
        agent_tool("call_shells", "shell_list", "shell.list", json!({})),
        say("Done."),
    ]});
    let env = Env::with(scenario);
    let client = env.client();
    client.watch_sessions();
    let id = client.create(&env.workspaces());
    let summary = client.wait_idle(&id);
    assert_eq!(summary["workspaces"][0]["root"], env.work());
    assert_eq!(summary["shells"][0]["name"], "devbox");

    let mark = client.turn_mark(&id);
    client.prompt(&id, "work remotely");
    client.wait_turn(&id, mark);

    let info: Value = serde_json::from_str(&tool_output(&client, &id, "call_info")).unwrap();
    assert_eq!(info["workspaces"][0]["machine"], "devbox");
    assert_eq!(info["workspaces"][0]["shell"], "devbox");
    let bash: Value = serde_json::from_str(&tool_output(&client, &id, "call_bash")).unwrap();
    assert_eq!(bash["result"]["op"], "bash");
    assert_eq!(bash["result"]["output"], "from-the-host\n");
    assert_eq!(bash["link_drops"], 0);
    let shells: Value = serde_json::from_str(&tool_output(&client, &id, "call_shells")).unwrap();
    assert_eq!(shells["shells"][0]["machine"], "devbox");

    let methods = env.controller().methods();
    for method in ["workspace.open", "shell.open", "tool.run"] {
        assert!(methods.iter().any(|m| m == method), "{method}: {methods:?}");
    }
}
