//! The readable history export for "start fresh" (spec §6.6), built from the materialized
//! timeline: user and agent messages, and tool calls with truncated output.

use controlroom_protocol::session::ContentPart;
use controlroom_protocol::timeline::{ItemBody, MessageRole, TimelineItem, ToolOutput};

/// Tool output beyond this many characters is cut.
const MAX_OUTPUT_CHARS: usize = 2000;

/// Markdown for the conversation in `items` (ordered by `seq_from`).
pub(crate) fn export(items: &[TimelineItem]) -> String {
    let mut out = String::from("# Earlier conversation\n");
    for item in items {
        match &item.body {
            ItemBody::Message { role, content } => {
                let who = match role {
                    MessageRole::User => "User",
                    MessageRole::Agent => "Agent",
                    MessageRole::Notice | MessageRole::Unknown => continue,
                };
                out.push_str(&format!("\n## {who}\n\n{}\n", text(content)));
            }
            ItemBody::ToolCall {
                title,
                status,
                input,
                output,
                ..
            } => {
                out.push_str(&format!("\n### Tool call: {title} ({status:?})\n"));
                if let Some(input) = input {
                    out.push_str(&format!("\nInput:\n\n```\n{}\n```\n", cut(input)));
                }
                let output = match output {
                    Some(ToolOutput::Text { text } | ToolOutput::Terminal { text }) => {
                        Some(cut(text))
                    }
                    Some(ToolOutput::Diff { path, .. }) => Some(format!("(changed {path})")),
                    Some(ToolOutput::Unknown) | None => None,
                };
                if let Some(output) = output {
                    out.push_str(&format!("\nOutput:\n\n```\n{output}\n```\n"));
                }
            }
            _ => {}
        }
    }
    out
}

fn text(content: &[ContentPart]) -> String {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Image { mime_type, .. } => format!("[image: {mime_type}]"),
            ContentPart::Resource { uri, .. } => format!("[resource: {uri}]"),
            ContentPart::Unknown => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    cut.push_str("\n… (truncated)");
    cut
}
