//! pi RPC record and message shapes, as recorded from pi 1.2.0
//! (`fixtures/pi-rpc/1.2.0`).

use serde_json::{Value, json};

use crate::ids::now_ms;
use crate::scenario::{ModelSpec, UsageSpec};

/// Price per million tokens used for the fake cost figures.
const INPUT_PRICE: f64 = 1.0;
const OUTPUT_PRICE: f64 = 2.0;

/// A successful command response.
pub fn ok(id: Option<&str>, command: &str, data: Option<Value>) -> Value {
    let mut response = json!({"type": "response", "command": command, "success": true});
    if let Some(id) = id {
        response["id"] = id.into();
    }
    if let Some(data) = data {
        response["data"] = data;
    }
    response
}

/// A failed command response.
pub fn error(id: Option<&str>, command: &str, message: &str) -> Value {
    let mut response = json!({
        "type": "response", "command": command, "success": false, "error": message
    });
    if let Some(id) = id {
        response["id"] = id.into();
    }
    response
}

/// The full model object of `get_state`, `set_model`, `get_available_models`.
pub fn model(spec: &ModelSpec) -> Value {
    json!({
        "id": spec.id,
        "name": spec.name,
        "api": "openai-completions",
        "provider": spec.provider,
        "baseUrl": "http://127.0.0.1:4000/v1",
        "reasoning": spec.reasoning,
        "input": ["text"],
        "cost": {"input": INPUT_PRICE, "output": OUTPUT_PRICE, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": spec.context_window,
        "maxTokens": spec.max_tokens,
    })
}

/// A usage object; zero before a response reports usage.
pub fn usage(spec: Option<UsageSpec>) -> Value {
    let UsageSpec { input, output } = spec.unwrap_or(UsageSpec {
        input: 0,
        output: 0,
    });
    let cost_in = input as f64 * INPUT_PRICE / 1e6;
    let cost_out = output as f64 * OUTPUT_PRICE / 1e6;
    json!({
        "input": input, "output": output, "cacheRead": 0, "cacheWrite": 0,
        "totalTokens": input + output,
        "cost": {"input": cost_in, "output": cost_out, "cacheRead": 0, "cacheWrite": 0,
                 "total": cost_in + cost_out},
    })
}

/// A user message.
pub fn user_message(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "text", "text": text}], "timestamp": now_ms()})
}

/// The system message pi sends before the first request of a session.
pub fn system_message(cwd: &str) -> Value {
    json!({
        "role": "system",
        "content": "",
        "sections": {
            "preamble": "You are a fake coding agent used in tests.",
            "cwd": format!("<cwd>\n{cwd}\n</cwd>"),
        },
        "timestamp": now_ms(),
    })
}

/// An assistant message.
pub fn assistant_message(
    model: &ModelSpec,
    content: Vec<Value>,
    usage_value: Value,
    stop_reason: &str,
    error_message: Option<&str>,
    timestamp: u64,
) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": content,
        "api": "openai-completions",
        "provider": model.provider,
        "model": model.id,
        "usage": usage_value,
        "stopReason": stop_reason,
        "timestamp": timestamp,
    });
    if let Some(error) = error_message {
        message["errorMessage"] = error.into();
    }
    message
}

/// A tool result message.
pub fn tool_result_message(id: &str, name: &str, text: &str, is_error: bool) -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": id,
        "toolName": name,
        "content": text_content(text),
        "isError": is_error,
        "timestamp": now_ms(),
    })
}

/// `[{"type":"text","text":...}]`, or `[]` for empty text.
pub fn text_content(text: &str) -> Value {
    if text.is_empty() {
        json!([])
    } else {
        json!([{"type": "text", "text": text}])
    }
}

/// A tool result value (`{content, details}`).
pub fn tool_result(text: &str) -> Value {
    json!({"content": text_content(text), "details": {}})
}

/// `message_update` carrying one assistant message event.
pub fn message_update(event: Value, usage_value: &Value) -> Value {
    json!({"type": "message_update", "usage": usage_value, "assistantMessageEvent": event})
}

/// `{"type": <kind>, "message": <message>}`.
pub fn message_event(kind: &str, message: &Value) -> Value {
    json!({"type": kind, "message": message})
}

/// An `extension_ui_request`.
pub fn ui_request(id: &str, method: &str, fields: Value) -> Value {
    let mut request = json!({"type": "extension_ui_request", "id": id, "method": method});
    if let (Some(target), Value::Object(fields)) = (request.as_object_mut(), fields) {
        target.extend(fields);
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_carry_ids_only_when_given() {
        assert_eq!(
            ok(Some("1"), "abort", None),
            json!({"type": "response", "command": "abort", "success": true, "id": "1"})
        );
        assert!(error(None, "parse", "bad").get("id").is_none());
    }

    #[test]
    fn ui_request_merges_fields() {
        let request = ui_request("u1", "notify", json!({"message": "hi"}));
        assert_eq!(request["method"], "notify");
        assert_eq!(request["message"], "hi");
    }
}
