//! pi 1.2.0's RPC JSON, as far as the fold reads it. Tolerant: unknown fields are ignored,
//! unknown event types and content blocks decode to `Unknown`, and missing fields default.

use controlroom_protocol::timeline::TokenUsage;
use serde::Deserialize;
use serde_json::Value;

/// One line pi wrote to stdout.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Out {
    /// The answer to a command.
    Response(Response),
    AgentStart {},
    AgentSettled {
        #[serde(default)]
        aborted: bool,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        assistant_message_event: AssistantEvent,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart(ToolEvent),
    ToolExecutionUpdate(ToolEvent),
    ToolExecutionEnd(ToolEvent),
    CompactionStart {
        #[serde(default)]
        reason: String,
    },
    CompactionEnd {
        #[serde(default)]
        result: Option<CompactionResult>,
        #[serde(default)]
        aborted: bool,
        #[serde(default)]
        error_message: Option<String>,
    },
    AutoRetryStart {
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        max_attempts: Option<u32>,
        #[serde(default)]
        delay_ms: Option<u64>,
        #[serde(default)]
        error_message: Option<String>,
    },
    AutoRetryEnd {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        final_error: Option<String>,
    },
    SessionInfoChanged {
        #[serde(default)]
        name: Option<String>,
    },
    ThinkingLevelChanged {
        level: String,
    },
    ExtensionUiRequest(UiRequest),
    ExtensionError {
        #[serde(default)]
        extension_path: Option<String>,
        #[serde(default)]
        event: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    BashExecutionUpdate {
        id: String,
        #[serde(default)]
        delta: String,
    },
    EntryAppended {
        entry: Entry,
    },
    /// Everything else (`turn_start`, `turn_end`, `agent_end`, `queue_update`, and types this
    /// build doesn't know): no timeline effect.
    #[serde(other)]
    Unknown,
}

/// `{"type":"response", id?, command, success, data?, error?}`.
#[derive(Debug, Deserialize)]
pub(crate) struct Response {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub error: Option<String>,
}

/// An agent message (`message_start` / `message_end`).
#[derive(Debug, Deserialize)]
#[serde(
    tag = "role",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Message {
    User {
        #[serde(default)]
        content: UserContent,
    },
    Assistant {
        #[serde(default)]
        content: Vec<Block>,
        #[serde(default)]
        stop_reason: Option<String>,
        #[serde(default)]
        error_message: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    /// An extension message (`pi.sendMessage`); shown only when `display` is set.
    Custom {
        #[serde(default)]
        content: UserContent,
        #[serde(default)]
        display: bool,
    },
    /// A tool's result; `usage` when the tool made model calls of its own.
    ToolResult {
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// `system` (obs. 3), `bashExecution`, summaries and unknown roles.
    #[serde(other)]
    Other,
}

/// `entry_appended.entry`: a session entry pi appended outside the message stream.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Entry {
    /// Model usage outside the context (e.g. a cache warm-up).
    Usage {
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// A branch summary, with the usage of the model call that wrote it.
    BranchSummary {
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// Custom entries, context edits and entry types this build doesn't know.
    #[serde(other)]
    Other,
}

/// A user or custom message's content: a string or content parts.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum UserContent {
    Text(String),
    Parts(Vec<Part>),
}

impl Default for UserContent {
    fn default() -> Self {
        UserContent::Parts(Vec::new())
    }
}

/// A text or image part of user content and tool results.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Part {
    Text {
        #[serde(default)]
        text: String,
    },
    Image {
        #[serde(default)]
        data: String,
        #[serde(default)]
        mime_type: String,
    },
    #[serde(other)]
    Unknown,
}

/// One content block of an assistant message.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Block {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolCall(ToolCall),
    #[serde(other)]
    Unknown,
}

/// A tool call block.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Token usage of one assistant message.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub cost: Option<Cost>,
}

impl Usage {
    /// The token counts, agent-neutral.
    pub fn tokens(&self) -> TokenUsage {
        TokenUsage {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

/// Cost in USD.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Cost {
    pub total: f64,
}

/// `message_update.assistantMessageEvent`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistantEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub content_index: Option<usize>,
    #[serde(default)]
    pub delta: Option<String>,
    /// The block's final text (`text_end`, `thinking_end`).
    #[serde(default)]
    pub content: Option<String>,
    /// The call id (`toolcall_start`).
    #[serde(default)]
    pub id: Option<String>,
    /// The tool name (`toolcall_start`).
    #[serde(default)]
    pub tool_name: Option<String>,
    /// The complete call (`toolcall_end`).
    #[serde(default)]
    pub tool_call: Option<ToolCall>,
}

/// `tool_execution_start` / `_update` / `_end`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolEvent {
    pub tool_call_id: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub partial_result: Option<ToolResult>,
    #[serde(default)]
    pub result: Option<ToolResult>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// A tool's (partial) result.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ToolResult {
    pub content: Vec<Part>,
    pub structured_content: Value,
}

/// `compaction_end.result`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CompactionResult {
    pub tokens_before: Option<u64>,
    pub estimated_tokens_after: Option<u64>,
    /// The usage of the model call that wrote the summary.
    pub usage: Option<Usage>,
}

/// `extension_ui_request`: a dialog (`select`, `confirm`, `input`, `editor`) or a
/// fire-and-forget update (`notify`, `setStatus`, `setWidget`, `setTitle`, ...).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct UiRequest {
    pub id: String,
    pub method: String,
    pub title: Option<String>,
    pub message: Option<String>,
    pub options: Vec<String>,
    pub placeholder: Option<String>,
    pub prefill: Option<String>,
    pub timeout: Option<u64>,
    pub notify_type: Option<String>,
    pub status_key: Option<String>,
    pub status_text: Option<String>,
    pub widget_key: Option<String>,
    pub widget_lines: Option<Vec<String>>,
}

/// One line written to pi's stdin (by the runtime or anyone else), as far as the fold reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct InLine {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: Option<String>,
    /// The controlroom tag of an `extension_ui_response`, whose `id` is the dialog's.
    pub cr_tag: Option<String>,
    pub message: Option<String>,
    pub images: Vec<Part>,
    pub command: Option<String>,
    pub value: Option<String>,
    pub confirmed: Option<bool>,
    pub cancelled: Option<bool>,
}

/// The `get_state` response data.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct State {
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
    pub session_file: Option<String>,
    pub session_id: Option<String>,
    pub session_name: Option<String>,
}

/// A model description (`get_state`, `get_available_models`, `set_model`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Model {
    pub id: String,
    pub name: Option<String>,
    pub provider: String,
    pub context_window: Option<u64>,
}

/// The `get_session_stats` response data.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SessionStats {
    pub tokens: StatsTokens,
    pub cost: Option<f64>,
    pub context_usage: Option<StatsContext>,
}

/// `get_session_stats.tokens`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct StatsTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// `get_session_stats.contextUsage`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct StatsContext {
    pub tokens: Option<u64>,
    pub context_window: Option<u64>,
}

/// The RPC `bash` response data.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BashResult {
    pub output: Option<String>,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
}

/// Concatenates the text parts.
pub(crate) fn parts_text(parts: &[Part]) -> String {
    let mut text = String::new();
    for part in parts {
        if let Part::Text { text: t } = part {
            text.push_str(t);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_events_and_blocks_decode() {
        let out: Out = serde_json::from_str(r#"{"type":"brand_new","x":1}"#).unwrap();
        assert!(matches!(out, Out::Unknown));
        let out: Out = serde_json::from_str(r#"{"type":"agent_start","extra":true}"#).unwrap();
        assert!(matches!(out, Out::AgentStart {}));
        let out: Out = serde_json::from_str(
            r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"image_gen"},{"type":"text","text":"hi"}],"stopReason":"stop"}}"#,
        )
        .unwrap();
        let Out::MessageEnd {
            message: Message::Assistant { content, .. },
        } = out
        else {
            panic!("not an assistant message");
        };
        assert!(matches!(content[0], Block::Unknown));
        let out: Out = serde_json::from_str(
            r#"{"type":"message_start","message":{"role":"system","content":"","sections":{}}}"#,
        )
        .unwrap();
        assert!(matches!(
            out,
            Out::MessageStart {
                message: Message::Other
            }
        ));
    }

    #[test]
    fn user_content_may_be_a_string() {
        let message: Message =
            serde_json::from_str(r#"{"role":"user","content":"plain"}"#).unwrap();
        assert!(matches!(message, Message::User { content: UserContent::Text(t) } if t == "plain"));
    }
}
