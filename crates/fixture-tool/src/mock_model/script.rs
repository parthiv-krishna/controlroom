//! What the mock model answers: one scripted [`Reply`] per request, in order.

use serde_json::Value;

/// One scripted answer to a chat-completions request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A streamed (SSE) completion.
    Stream(StreamReply),
    /// An HTTP error with this status and JSON error message.
    Error { status: u16, message: String },
}

/// A streamed completion: `parts` in order, then a final chunk with
/// `finish_reason` and usage.
#[derive(Debug, Clone)]
pub struct StreamReply {
    pub parts: Vec<Part>,
    pub finish: Finish,
    /// `completion_tokens` reported in the final usage.
    pub completion_tokens: u64,
}

/// One piece of a streamed completion.
#[derive(Debug, Clone)]
pub enum Part {
    /// `delta.reasoning_content` chunks.
    Reasoning(Vec<String>),
    /// `delta.content` chunks.
    Text(Vec<String>),
    /// One complete tool call (id, function name, JSON arguments) streamed
    /// as a name chunk plus one arguments chunk.
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// Stop streaming until the recorder releases this gate. If the client
    /// disconnects first (abort), the stream ends there.
    Gate(String),
}

/// `finish_reason` of the final chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Stop,
    Length,
    ToolCalls,
}

impl Finish {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
        }
    }
}

/// Split `text` into chunks of at most `size` characters.
pub fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(size.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

impl Reply {
    /// A plain text answer, streamed in small chunks.
    pub fn text(text: &str) -> Self {
        Self::stream(vec![Part::Text(chunks(text, 8))], Finish::Stop)
    }

    /// Thinking followed by text.
    pub fn thinking(thinking: &str, text: &str) -> Self {
        Self::stream(
            vec![
                Part::Reasoning(chunks(thinking, 8)),
                Part::Text(chunks(text, 8)),
            ],
            Finish::Stop,
        )
    }

    /// A single tool call.
    pub fn tool_call(id: &str, name: &str, args: Value) -> Self {
        Self::stream(
            vec![Part::ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                args,
            }],
            Finish::ToolCalls,
        )
    }

    /// Text up to `gate` (as one chunk, so "the first delta arrived" means
    /// "the stream is at the gate"), then (once released) the rest.
    pub fn gated_text(before: &str, gate: &str, after: &str) -> Self {
        Self::stream(
            vec![
                Part::Text(vec![before.to_owned()]),
                Part::Gate(gate.to_owned()),
                Part::Text(chunks(after, 8)),
            ],
            Finish::Stop,
        )
    }

    /// A stream with the given parts.
    pub fn stream(parts: Vec<Part>, finish: Finish) -> Self {
        Self::Stream(StreamReply {
            parts,
            finish,
            completion_tokens: 12,
        })
    }

    /// An HTTP error.
    pub fn error(status: u16, message: &str) -> Self {
        Self::Error {
            status,
            message: message.to_owned(),
        }
    }

    /// Override the reported completion tokens (e.g. to hit `maxTokens`).
    #[must_use]
    pub fn with_completion_tokens(mut self, tokens: u64) -> Self {
        if let Self::Stream(stream) = &mut self {
            stream.completion_tokens = tokens;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_split_on_chars() {
        assert_eq!(chunks("héllo", 2), ["hé", "ll", "o"]);
        assert!(chunks("", 3).is_empty());
    }
}
