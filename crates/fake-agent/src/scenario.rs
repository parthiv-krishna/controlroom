//! The scenario file that scripts the fake agent.
//!
//! A scenario mirrors what a model and a set of extensions would do: each
//! model request (one per assistant turn) consumes the next [`Reply`], in
//! order, and extension slash commands run scripted [`Step`]s. Every field
//! has a default, so `{}` is a valid scenario that answers every prompt
//! with the text `ok`.
//!
//! ```json
//! {
//!   "session_start": [{"type": "ui", "method": "setStatus", "key": "ext", "text": "ready"}],
//!   "commands": [{"name": "ask", "steps": [
//!     {"type": "dialog", "method": "select", "title": "Pick", "options": ["a", "b"]}
//!   ]}],
//!   "replies": [
//!     {"content": [
//!       {"type": "thinking", "text": "Hmm."},
//!       {"type": "tool_call", "id": "call_1", "name": "bash", "args": {"command": "ls"},
//!        "result": "a.txt\n", "progress": [{"update": ""}, {"gate": "tool"}]}
//!     ]},
//!     {"content": [{"type": "text", "text": "Done."}, {"type": "gate", "name": "g1"}]},
//!     {"error": "503 service unavailable"}
//!   ]
//! }
//! ```

use serde::Deserialize;
use serde_json::Value;

/// The whole script.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scenario {
    /// Models offered by `get_available_models`; the first is the default.
    pub models: Vec<ModelSpec>,
    /// The initial thinking level.
    pub thinking_level: String,
    /// Agent-level retries for retryable errors (pi's `retry.maxRetries`).
    pub max_retries: u32,
    /// First retry delay; doubles per attempt (pi's `retry.baseDelayMs`).
    pub retry_base_delay_ms: u64,
    /// Steps run when the process starts (an extension's `session_start`).
    pub session_start: Vec<Step>,
    /// Steps run after `new_session` (pi re-runs `session_start` handlers).
    pub new_session: Vec<Step>,
    /// Extension slash commands, run through `prompt "/name args"`.
    pub commands: Vec<CommandSpec>,
    /// Model replies, consumed in order by successive model requests.
    pub replies: Vec<Reply>,
    /// Used once `replies` is exhausted. Defaults to the text `ok`.
    pub default_reply: Option<Reply>,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            models: vec![
                ModelSpec::named("mock-model", "Mock Model"),
                ModelSpec::named("mock-model-b", "Mock Model B"),
            ],
            thinking_level: "medium".into(),
            max_retries: 3,
            retry_base_delay_ms: 20,
            session_start: Vec::new(),
            new_session: Vec::new(),
            commands: Vec::new(),
            replies: Vec::new(),
            default_reply: None,
        }
    }
}

impl Scenario {
    /// Parse a scenario and check that every step fits where it is used.
    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let scenario: Self = serde_json::from_str(text)?;
        scenario.validate()?;
        Ok(scenario)
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.models.is_empty(),
            "a scenario needs at least one model"
        );
        let side_steps = self
            .session_start
            .iter()
            .chain(&self.new_session)
            .chain(self.commands.iter().flat_map(|c| &c.steps));
        for step in side_steps {
            anyhow::ensure!(
                !step.is_content(),
                "text, thinking and tool_call steps only fit in replies: {step:?}"
            );
        }
        Ok(())
    }
}

/// A model in the `models.json` sense.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_true")]
    pub reasoning: bool,
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
}

impl ModelSpec {
    fn named(id: &str, name: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider: default_provider(),
            reasoning: true,
            context_window: default_context_window(),
            max_tokens: default_max_tokens(),
        }
    }
}

fn default_provider() -> String {
    "mock".into()
}

fn default_true() -> bool {
    true
}

fn default_context_window() -> u64 {
    128_000
}

fn default_max_tokens() -> u64 {
    4096
}

/// An extension slash command.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One model response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reply {
    /// Streamed content and side steps, in order.
    pub content: Vec<Step>,
    /// How the response ends when it has no tool calls.
    pub stop: Stop,
    /// Fail the request with this message instead (stopReason `error`).
    /// Messages that look transient (429, 5xx, overloaded, ...) are retried.
    pub error: Option<String>,
    /// Usage reported for the response.
    pub usage: Option<UsageSpec>,
}

impl Reply {
    /// The fallback reply: the text `ok`.
    pub fn ok() -> Self {
        Self {
            content: vec![Step::Text(TextStep {
                text: "ok".into(),
                chunk: 8,
            })],
            ..Self::default()
        }
    }
}

/// Stop reasons of a reply without tool calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    #[default]
    Stop,
    Length,
}

impl Stop {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
        }
    }
}

/// Token counts of one response.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSpec {
    pub input: u64,
    pub output: u64,
}

impl Default for UsageSpec {
    fn default() -> Self {
        Self {
            input: 100,
            output: 12,
        }
    }
}

/// One scripted action.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Streamed assistant text (replies only).
    Text(TextStep),
    /// Streamed thinking (replies only).
    Thinking(TextStep),
    /// A tool call, executed after the response streams (replies only).
    ToolCall(ToolCallStep),
    /// Block until the test releases the gate (see [`crate::gates`]).
    /// Abort interrupts it. Without a gate directory the agent exits with
    /// code 2.
    Gate { name: String },
    /// Wait; abort interrupts it.
    Sleep { ms: u64 },
    /// A fire-and-forget extension UI request.
    Ui(UiStep),
    /// A dialog that blocks until `extension_ui_response` (or its timeout).
    Dialog(DialogStep),
    /// `pi.appendEntry(custom_type, data)`: a `custom` entry plus `entry_appended`.
    AppendEntry {
        custom_type: String,
        #[serde(default)]
        data: Value,
    },
    /// Exit the process at once with this code.
    Crash {
        #[serde(default = "default_crash_code")]
        code: i32,
    },
}

impl Step {
    /// Whether the step is model output (only valid inside a reply).
    pub fn is_content(&self) -> bool {
        matches!(self, Self::Text(_) | Self::Thinking(_) | Self::ToolCall(_))
    }
}

fn default_crash_code() -> i32 {
    101
}

/// Text streamed in chunks.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStep {
    pub text: String,
    /// Characters per delta.
    #[serde(default = "default_chunk")]
    pub chunk: usize,
}

fn default_chunk() -> usize {
    8
}

/// A tool call and its scripted execution.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallStep {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub args: Value,
    /// Text of the result.
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub is_error: bool,
    /// What happens while the tool runs, in order.
    #[serde(default)]
    pub progress: Vec<ToolProgress>,
    /// Ask with a select dialog before running (an extension `tool_call`
    /// hook); any answer other than `allow` blocks the call.
    #[serde(default)]
    pub permission: Option<PermissionSpec>,
    /// Tools this tool runs through `ctx.executeTool` (`parentToolCallId`).
    #[serde(default)]
    pub nested: Vec<NestedCall>,
}

/// One thing a running tool does.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolProgress {
    /// A `tool_execution_update` with this partial text; an empty string
    /// sends an empty content list, as pi's bash tool does first.
    Update(String),
    /// Block on this gate; abort ends the tool with "Command aborted".
    Gate(String),
}

/// A permission prompt before a tool runs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionSpec {
    pub title: String,
    #[serde(default = "default_permission_options")]
    pub options: Vec<String>,
    #[serde(default = "default_allow")]
    pub allow: String,
}

fn default_permission_options() -> Vec<String> {
    vec!["Allow".into(), "Block".into()]
}

fn default_allow() -> String {
    "Allow".into()
}

/// A nested tool call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NestedCall {
    pub name: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub result: String,
}

/// Fire-and-forget extension UI requests.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum UiStep {
    #[serde(rename = "notify")]
    Notify {
        message: String,
        #[serde(default = "default_notify_type")]
        notify_type: String,
    },
    #[serde(rename = "setStatus")]
    SetStatus { key: String, text: Option<String> },
    #[serde(rename = "setWidget")]
    SetWidget {
        key: String,
        lines: Option<Vec<String>>,
    },
    #[serde(rename = "setTitle")]
    SetTitle { title: String },
}

fn default_notify_type() -> String {
    "info".into()
}

/// A blocking dialog.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogStep {
    pub method: DialogMethod,
    pub title: String,
    /// `select` options.
    #[serde(default)]
    pub options: Vec<String>,
    /// `confirm` message.
    #[serde(default)]
    pub message: Option<String>,
    /// `input` placeholder.
    #[serde(default)]
    pub placeholder: Option<String>,
    /// `editor` prefill.
    #[serde(default)]
    pub prefill: Option<String>,
    /// Resolve unanswered after this many ms (sent as `timeout`).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Dialog kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogMethod {
    Select,
    Confirm,
    Input,
    Editor,
}

impl DialogMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Confirm => "confirm",
            Self::Input => "input",
            Self::Editor => "editor",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scenario_has_defaults() {
        let scenario = Scenario::from_json("{}").unwrap();
        assert_eq!(scenario.models[0].id, "mock-model");
        assert!(scenario.replies.is_empty());
    }

    #[test]
    fn documented_example_parses() {
        let doc = include_str!("scenario.rs");
        let start = doc.find("//! ```json").unwrap();
        let end = doc[start + 10..].find("//! ```").unwrap() + start + 10;
        let json: String = doc[start..end]
            .lines()
            .skip(1)
            .map(|l| l.trim_start_matches("//!"))
            .collect::<Vec<_>>()
            .join("\n");
        let scenario = Scenario::from_json(&json).unwrap();
        assert_eq!(scenario.replies.len(), 3);
        assert!(matches!(scenario.replies[0].content[1], Step::ToolCall(_)));
    }

    #[test]
    fn unknown_fields_and_misplaced_steps_are_rejected() {
        assert!(Scenario::from_json(r#"{"replys": []}"#).is_err());
        assert!(
            Scenario::from_json(
                r#"{"commands": [{"name": "x", "steps": [{"type": "text", "text": "no"}]}]}"#
            )
            .is_err()
        );
    }
}
