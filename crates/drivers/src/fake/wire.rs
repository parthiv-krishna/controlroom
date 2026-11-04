//! The fake agent's line protocol: one JSON object per line, tagged by `type`.
//!
//! To the agent ([`Input`]): `prompt` (optionally steering), `steer`, `cancel`, `answer`,
//! `set_option`, `new_session` and `state`, each with the writer's [`WireTag`] in `tag`.
//!
//! From the agent ([`Output`]): a `response` to every input line, plus `state`, `run_start`,
//! `run_end`, `text`, `tool_start`, `tool_end`, `request`, `request_closed` and `context`.

use std::str::FromStr;

use controlroom_protocol::agentd::OptionValue;
use controlroom_protocol::ids::{OpId, QueueId};
use controlroom_protocol::session::{
    Question, RequestKind, RequestOption, Response, RunOutcome, SessionOption,
};
use serde::{Deserialize, Serialize};

use crate::{LineTag, NativeRef};

/// A [`LineTag`] as it travels in the `tag` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireTag {
    /// [`LineTag::Op`].
    Op {
        /// The op id.
        op_id: String,
        /// The line index.
        part: u16,
    },
    /// [`LineTag::Queue`].
    Queue {
        /// The queue item, as a ULID string.
        queue_id: String,
        /// The line index.
        part: u16,
    },
    /// [`LineTag::Internal`].
    Internal {
        /// The key.
        key: String,
        /// The line index.
        part: u16,
    },
}

impl From<&LineTag> for WireTag {
    fn from(tag: &LineTag) -> Self {
        match tag {
            LineTag::Op { op_id, part } => WireTag::Op {
                op_id: op_id.to_string(),
                part: *part,
            },
            LineTag::Queue { queue_id, part } => WireTag::Queue {
                queue_id: queue_id.to_string(),
                part: *part,
            },
            LineTag::Internal { key, part } => WireTag::Internal {
                key: key.clone(),
                part: *part,
            },
        }
    }
}

impl WireTag {
    /// The tag, or `None` for a malformed queue id.
    pub fn to_tag(&self) -> Option<LineTag> {
        Some(match self {
            WireTag::Op { op_id, part } => LineTag::Op {
                op_id: OpId::new(op_id.clone()),
                part: *part,
            },
            WireTag::Queue { queue_id, part } => LineTag::Queue {
                queue_id: QueueId::from_str(queue_id).ok()?,
                part: *part,
            },
            WireTag::Internal { key, part } => LineTag::Internal {
                key: key.clone(),
                part: *part,
            },
        })
    }
}

/// A line written to the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Input {
    /// The writer's tag, echoed in the `response`.
    pub tag: WireTag,
    /// The command.
    #[serde(flatten)]
    pub command: Command,
}

/// What an input line asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Start a run (refused while one is active, unless `steer`).
    Prompt {
        /// The prompt text.
        text: String,
        /// While a run is active, steer the text into it instead (pi's `prompt` with
        /// `streamingBehavior: "steer"`).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        steer: bool,
    },
    /// Add text to the active run. While idle the agent holds it and delivers it with the next
    /// prompt (like pi).
    Steer {
        /// The text.
        text: String,
    },
    /// Stop the active run; open requests are closed.
    Cancel,
    /// Answer a request.
    Answer {
        /// The agent's request id.
        request: String,
        /// The answer.
        response: Response,
    },
    /// Change an option.
    SetOption {
        /// The option id.
        id: String,
        /// The value.
        value: OptionValue,
    },
    /// Switch to a new native session.
    NewSession {
        /// The previous native session.
        parent: Option<String>,
    },
    /// Report the agent's state.
    State,
}

/// A line from the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Output {
    /// The answer to an input line.
    Response {
        /// The input's tag.
        tag: WireTag,
        /// Whether it was accepted.
        ok: bool,
        /// Why not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The agent's state.
    State {
        /// The native session.
        native: WireNative,
        /// The options.
        options: Vec<SessionOption>,
    },
    /// A run began.
    RunStart {
        /// The input that started it.
        cause: Option<WireTag>,
    },
    /// The run ended.
    RunEnd {
        /// How.
        outcome: RunOutcome,
        /// The error, for failures.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Agent text, appended to the current message.
    Text {
        /// The text.
        text: String,
    },
    /// A tool call started.
    ToolStart {
        /// The agent's call id.
        id: String,
        /// The tool.
        name: String,
        /// A one-line title.
        title: String,
    },
    /// A tool call ended.
    ToolEnd {
        /// The agent's call id.
        id: String,
        /// Whether it succeeded.
        ok: bool,
        /// Its output.
        output: String,
    },
    /// The agent asks something.
    Request {
        /// The agent's request id.
        id: String,
        /// What kind of request.
        kind: RequestKind,
        /// The title.
        title: String,
        /// Answer options.
        #[serde(default)]
        options: Vec<RequestOption>,
        /// Questions.
        #[serde(default)]
        questions: Vec<Question>,
    },
    /// A request ended without an answer.
    RequestClosed {
        /// The agent's request id.
        id: String,
    },
    /// Context usage.
    Context {
        /// Tokens used.
        used: u64,
        /// The window size.
        size: Option<u64>,
        /// Cost so far.
        cost: Option<f64>,
    },
    /// The session title changed.
    Title {
        /// The title.
        title: String,
    },
}

/// A [`NativeRef`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireNative {
    /// The id.
    pub id: String,
    /// The file, if any.
    pub path: Option<String>,
}

impl From<WireNative> for NativeRef {
    fn from(native: WireNative) -> Self {
        NativeRef {
            id: native.id,
            path: native.path,
        }
    }
}

/// Encodes a protocol value as one line (these types always serialize).
pub(crate) fn line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
