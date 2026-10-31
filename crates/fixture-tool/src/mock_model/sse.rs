//! SSE frames in the OpenAI `chat.completion.chunk` shape.

use serde_json::{Value, json};

use super::Finish;

/// The terminating frame.
pub const DONE: &str = "data: [DONE]\n\n";

/// A fixed `created` time, so recordings stay stable.
const CREATED: u64 = 1_700_000_000;

/// Prompt tokens reported for every request.
const PROMPT_TOKENS: u64 = 100;

/// Frame builder for one completion.
pub struct Completion {
    id: String,
    model: String,
}

impl Completion {
    pub fn new(id: String, model: String) -> Self {
        Self { id, model }
    }

    fn frame(&self, choice: Value, usage: Option<Value>) -> String {
        let mut chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": CREATED,
            "model": self.model,
            "choices": [choice],
        });
        if let Some(usage) = usage {
            chunk["usage"] = usage;
        }
        format!("data: {chunk}\n\n")
    }

    fn delta(&self, delta: Value) -> String {
        self.frame(
            json!({"index": 0, "delta": delta, "finish_reason": null}),
            None,
        )
    }

    pub fn text(&self, text: &str) -> String {
        self.delta(json!({"role": "assistant", "content": text}))
    }

    pub fn reasoning(&self, text: &str) -> String {
        self.delta(json!({"role": "assistant", "reasoning_content": text}))
    }

    /// A tool call as two frames: id + name, then the arguments.
    pub fn tool_call(&self, index: usize, id: &str, name: &str, args: &Value) -> Vec<String> {
        vec![
            self.delta(json!({"role": "assistant", "tool_calls": [{
                "index": index, "id": id, "type": "function",
                "function": {"name": name, "arguments": ""}
            }]})),
            self.delta(json!({"tool_calls": [{
                "index": index, "function": {"arguments": args.to_string()}
            }]})),
        ]
    }

    pub fn finish(&self, finish: Finish, completion_tokens: u64) -> String {
        self.frame(
            json!({"index": 0, "delta": {}, "finish_reason": finish.as_str()}),
            Some(json!({
                "prompt_tokens": PROMPT_TOKENS,
                "completion_tokens": completion_tokens,
                "total_tokens": PROMPT_TOKENS + completion_tokens,
            })),
        )
    }
}
