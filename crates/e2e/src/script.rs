//! Building blocks for the fake agent's scenario: model replies whose tool calls use the agent
//! API the way `@controlroom/pi` does.

use serde_json::{Value, json};

/// A reply that says `text`.
pub fn say(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

/// A reply that waits at gate `name` (released by the test) and then says `text`.
pub fn gate(name: &str, text: &str) -> Value {
    json!({"content": [{"type": "gate", "name": name}, {"type": "text", "text": text}]})
}

/// A reply with one tool call `name` (id `id`) whose execution is the agent API `calls`
/// (`(method, params)`, in order; see the fake agent's `agent` field).
pub fn tool(id: &str, name: &str, calls: &[(&str, Value)]) -> Value {
    let calls: Vec<Value> = calls
        .iter()
        .map(|(method, params)| json!({"method": method, "params": params}))
        .collect();
    json!({"content": [{"type": "tool_call", "id": id, "name": name,
        "args": calls.first().map_or(Value::Null, |c| c["params"].clone()), "agent": calls}]})
}

/// A reply with one `host.call` of `op` on `machine`, as tool `name` with id `id`.
pub fn host_call(id: &str, name: &str, machine: &str, op: Value) -> Value {
    tool(
        id,
        name,
        &[("host.call", json!({"machine": machine, "op": op}))],
    )
}

/// `bash` in shell `shell` on `machine`.
pub fn bash(id: &str, machine: &str, shell: &str, command: &str) -> Value {
    host_call(
        id,
        "bash",
        machine,
        json!({"op": "bash", "shell": shell, "command": command}),
    )
}

/// An edit as the extension does it: `read_bytes` (`<id>/read`), then `write` with the hash it
/// read (`<id>/write`). The test knows the file, so the hash is given.
pub fn edit(id: &str, machine: &str, path: &str, new_content: &str, old_sha256: &str) -> Value {
    tool(
        id,
        "edit",
        &[
            (
                "host.call",
                json!({"call_id": format!("{id}/read"), "machine": machine,
                       "op": {"op": "read_bytes", "path": path}}),
            ),
            (
                "host.call",
                json!({"call_id": format!("{id}/write"), "machine": machine,
                       "op": {"op": "write", "path": path, "content": new_content,
                              "expect_sha256": old_sha256}}),
            ),
        ],
    )
}

/// A scenario with these replies (anything after them answers `ok`).
pub fn scenario(replies: Vec<Value>) -> Value {
    json!({"replies": replies})
}
