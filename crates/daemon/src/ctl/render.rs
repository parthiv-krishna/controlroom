//! A plain-text terminal renderer for timeline ops (spec §7), for `controlroomd ctl`.
//!
//! The renderer keeps the items it has seen and prints only what changed:
//!
//! - text-bearing items (messages, thinking, tool output) stream: new text is appended to the
//!   item's open line, and continuation lines are indented;
//! - every other item has a one-line summary (several lines for a todo list), printed when the
//!   item appears and again whenever the summary changes (a tool call completing, a run
//!   settling, a request being answered).
//!
//! Output is deterministic (no colors, no clock), so it is tested with golden files.

use std::collections::HashMap;

use controlroom_protocol::ids::ItemId;
use controlroom_protocol::session::{ContentPart, Request, RequestState};
use controlroom_protocol::timeline::{
    ItemBody, MessageRole, RunState, RunTrigger, TimelineItem, TimelineOp, ToolOutput,
};
use serde::Serialize;

/// Indentation of continuation lines.
const INDENT: &str = "  ";

/// Applies timeline ops and produces the text to print.
#[derive(Debug, Default)]
pub struct Renderer {
    items: HashMap<ItemId, TimelineItem>,
    /// What was printed per item: the summary line, and how much of its text.
    printed: HashMap<ItemId, Printed>,
    out: Output,
}

#[derive(Debug, Default)]
struct Printed {
    summary: Option<String>,
    text: String,
}

/// The pending output and the streaming line, if one is open.
#[derive(Debug, Default)]
struct Output {
    text: String,
    open: Option<ItemId>,
    mid_line: bool,
    /// The prefix of the open item's continuation lines.
    continuation: &'static str,
}

impl Output {
    /// Ends an open streaming line.
    fn close(&mut self) {
        if self.open.take().is_some() && self.mid_line {
            self.text.push('\n');
        }
        self.mid_line = false;
    }

    /// Prints whole lines (each continuation line indented by [`INDENT`]).
    fn lines(&mut self, s: &str) {
        self.close();
        for (i, line) in s.lines().enumerate() {
            if i > 0 {
                self.text.push_str(INDENT);
            }
            self.text.push_str(line);
            self.text.push('\n');
        }
    }

    /// Appends streamed text to `id`'s line, opening it with `prefix.head` if needed.
    fn stream(&mut self, id: &ItemId, prefix: Prefix, chunk: &str) {
        if self.open.as_ref() != Some(id) {
            self.close();
            self.text.push_str(prefix.head);
            self.continuation = prefix.continuation;
            self.open = Some(id.clone());
            self.mid_line = true;
        }
        for c in chunk.chars() {
            if !self.mid_line {
                self.text.push_str(self.continuation);
                self.mid_line = true;
            }
            self.text.push(c);
            if c == '\n' {
                self.mid_line = false;
            }
        }
    }
}

impl Renderer {
    /// A renderer with no items.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one op.
    pub fn apply(&mut self, op: &TimelineOp) {
        match op {
            TimelineOp::Upsert { item } => {
                self.items.insert(item.id.clone(), (**item).clone());
                self.show(&item.id);
            }
            TimelineOp::AppendText { id, text, .. } => {
                if let Some(item) = self.items.get_mut(id) {
                    append_text(&mut item.body, text);
                    self.show(id);
                }
            }
            TimelineOp::End { id, .. } => {
                if let Some(item) = self.items.get_mut(id) {
                    item.streaming = false;
                }
                if self.out.open.as_ref() == Some(id) {
                    self.out.close();
                }
            }
            TimelineOp::Unknown => {}
        }
    }

    /// Takes the text rendered so far.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.out.text)
    }

    /// Ends any open line and takes the rest of the text.
    pub fn finish(&mut self) -> String {
        self.out.close();
        self.take()
    }

    /// Prints what changed about `id`: a new item's summary before its text, a changed
    /// summary (e.g. a tool call completing) after the text that led up to it.
    fn show(&mut self, id: &ItemId) {
        let Some(item) = self.items.get(id) else {
            return;
        };
        let printed = self.printed.entry(id.clone()).or_default();
        let summary = summary(&item.body);
        let changed = summary.is_some() && summary != printed.summary;
        let first = printed.summary.is_none();
        if changed && first {
            self.out.lines(summary.as_deref().unwrap_or_default());
        }
        if let Some((prefix, text)) = streamed_text(&item.body) {
            match text.strip_prefix(printed.text.as_str()) {
                Some("") => {}
                Some(new) => self.out.stream(id, prefix, new),
                // Replaced rather than extended: print it again from the start.
                None => {
                    self.out.close();
                    self.out.stream(id, prefix, &text);
                }
            }
            printed.text = text;
        }
        if changed && !first {
            self.out.lines(summary.as_deref().unwrap_or_default());
        }
        if changed {
            printed.summary = summary;
        }
    }
}

/// The protocol's `append_text` semantics: a message's last text part (added if needed), a
/// thinking item's text, or a tool call's text output.
fn append_text(body: &mut ItemBody, text: &str) {
    match body {
        ItemBody::Message { content, .. } => match content.last_mut() {
            Some(ContentPart::Text { text: last }) => last.push_str(text),
            _ => content.push(ContentPart::text(text)),
        },
        ItemBody::Thinking { text: thinking } => thinking.push_str(text),
        ItemBody::ToolCall { output, .. } => match output {
            Some(ToolOutput::Text { text: out } | ToolOutput::Terminal { text: out }) => {
                out.push_str(text)
            }
            _ => *output = Some(ToolOutput::Text { text: text.into() }),
        },
        _ => {}
    }
}

/// How a streamed item's lines start.
#[derive(Debug, Clone, Copy)]
struct Prefix {
    /// The first line.
    head: &'static str,
    /// Every following line.
    continuation: &'static str,
}

const fn prose(head: &'static str) -> Prefix {
    Prefix {
        head,
        continuation: INDENT,
    }
}

/// Tool output: every line in a `|` gutter.
const TOOL_OUTPUT: Prefix = Prefix {
    head: "  | ",
    continuation: "  | ",
};

/// The streamed text of an item, with how its lines start.
fn streamed_text(body: &ItemBody) -> Option<(Prefix, String)> {
    match body {
        ItemBody::Message { role, content } => {
            let head = match role {
                MessageRole::User => "user: ",
                MessageRole::Agent => "agent: ",
                MessageRole::Notice => "notice: ",
                MessageRole::Unknown => "message: ",
            };
            Some((prose(head), content_text(content)))
        }
        ItemBody::Thinking { text } => Some((prose("thinking: "), text.clone())),
        ItemBody::ToolCall {
            output: Some(ToolOutput::Text { text } | ToolOutput::Terminal { text }),
            ..
        } => Some((TOOL_OUTPUT, text.clone())),
        _ => None,
    }
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Image { mime_type, .. } => format!("[image {mime_type}]"),
            ContentPart::Resource { uri, .. } => format!("[{uri}]"),
            ContentPart::Unknown => "[unknown content]".to_owned(),
        })
        .collect()
}

/// The `snake_case` wire name of an enum value.
fn name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".to_owned(),
    }
}

fn trigger(trigger: &RunTrigger) -> String {
    match trigger {
        RunTrigger::User { .. } => "user".into(),
        RunTrigger::Queue { .. } => "queue".into(),
        RunTrigger::Steer { .. } => "steer".into(),
        RunTrigger::Inject { label, .. } => format!("inject: {label}"),
        RunTrigger::AutoContinue => "auto-continue".into(),
        RunTrigger::Retry => "retry".into(),
        RunTrigger::Extension { name } => format!("extension {name}"),
        RunTrigger::Schedule { name, .. } => format!("schedule: {name}"),
        RunTrigger::Unknown => "unknown".into(),
    }
}

fn request(r: &Request) -> String {
    let mut line = format!("request {} ({}", name(&r.state), name(&r.kind));
    if r.blocking {
        line.push_str(", blocking");
    }
    line.push_str(&format!("): {}", r.title));
    match (&r.state, &r.answer) {
        (RequestState::Pending, _) if !r.options.is_empty() => {
            let labels: Vec<&str> = r.options.iter().map(|o| o.label.as_str()).collect();
            line.push_str(&format!(" [{}]", labels.join(" / ")));
        }
        (_, Some(answer)) => line.push_str(&format!(" -> {}", answer.label)),
        _ => {}
    }
    line
}

/// The one-line summary of a non-text item (`None` for messages and thinking).
fn summary(body: &ItemBody) -> Option<String> {
    let s = match body {
        ItemBody::Message { .. } | ItemBody::Thinking { .. } => return None,
        ItemBody::Run {
            trigger: t,
            state: RunState::Running,
            ..
        } => format!("--- run started ({})", trigger(t)),
        ItemBody::Run { outcome, error, .. } => {
            let mut line = format!(
                "--- run {}",
                outcome.as_ref().map_or("settled".to_owned(), name)
            );
            if let Some(error) = error {
                line.push_str(&format!(": {error}"));
            }
            line
        }
        ItemBody::ToolCall {
            name: tool,
            title,
            status,
            exit,
            duration_ms,
            output,
            ..
        } => {
            let mut line = format!("tool {}: {tool}: {title}", name(status));
            let details: Vec<String> = exit
                .map(|e| format!("exit {e}"))
                .into_iter()
                .chain(duration_ms.map(|d| format!("{d} ms")))
                .collect();
            if !details.is_empty() {
                line.push_str(&format!(" ({})", details.join(", ")));
            }
            if let Some(ToolOutput::Diff { path, .. }) = output {
                line.push_str(&format!("\nedited {path}"));
            }
            line
        }
        ItemBody::Todo { items } => {
            let mut s = format!("todo ({} items)", items.len());
            for todo in items {
                s.push_str(&format!("\n[{}] {}", name(&todo.status), todo.text));
            }
            s
        }
        ItemBody::Request(r) => request(r),
        ItemBody::Subagent {
            child,
            title,
            agent,
            state,
            outcome,
            ..
        } => {
            let mut line = format!(
                "subagent {} ({agent}, {}): {}",
                child.name,
                name(state),
                title
            );
            if let Some(outcome) = outcome {
                line.push_str(&format!(" -> {}", name(outcome)));
            }
            line
        }
        ItemBody::Job(job) => {
            let mut line = format!("job {}: {} ({})", job.name, job.command, name(&job.status));
            if let Some(exit) = job.exit {
                line.push_str(&format!(", exit {exit}"));
            }
            line
        }
        ItemBody::SessionMessage {
            direction,
            peer,
            text,
            outcome,
            ..
        } => {
            let mut line = format!("message {} {}: {text}", name(direction), peer.name);
            if let Some(outcome) = outcome {
                line.push_str(&format!(" ({})", name(outcome)));
            }
            line
        }
        ItemBody::Usage {
            scope,
            tokens,
            cost,
            ..
        } => {
            let mut line = format!(
                "usage ({}): {} in, {} out",
                name(scope),
                tokens.input,
                tokens.output
            );
            if let Some(cost) = cost {
                line.push_str(&format!(", ${cost:.4}"));
            }
            line
        }
        ItemBody::Compaction {
            reason,
            phase,
            tokens_before,
            tokens_after,
            error,
        } => {
            let mut line = format!("compaction {} ({})", name(phase), name(reason));
            if let (Some(before), Some(after)) = (tokens_before, tokens_after) {
                line.push_str(&format!(": {before} -> {after} tokens"));
            }
            if let Some(error) = error {
                line.push_str(&format!(": {error}"));
            }
            line
        }
        ItemBody::Retry {
            attempt,
            max_attempts,
            phase,
            error,
            ..
        } => {
            let max = max_attempts.map_or(String::new(), |m| format!("/{m}"));
            let mut line = format!("retry {attempt}{max} {}", name(phase));
            if let Some(error) = error {
                line.push_str(&format!(": {error}"));
            }
            line
        }
        ItemBody::Notice { level, text } => format!("notice ({}): {text}", name(level)),
        ItemBody::Error { text } => format!("error: {text}"),
        ItemBody::SessionInfo {
            title,
            model,
            thinking,
        } => {
            let fields: Vec<String> = [("title", title), ("model", model), ("thinking", thinking)]
                .into_iter()
                .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}")))
                .collect();
            format!("session: {}", fields.join(", "))
        }
        ItemBody::Unknown => "(an item kind this build doesn't know)".to_owned(),
    };
    Some(s)
}
