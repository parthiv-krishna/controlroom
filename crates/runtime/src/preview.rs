//! The summary's one-line preview (spec §5.2): plain text, at most 200 characters.
//!
//! [`PreviewSource`] follows the timeline ops for the parts the preview needs (the running
//! tools and the last agent text); the session picks among them by priority.

use std::collections::BTreeMap;

use controlroom_protocol::ids::ItemId;
use controlroom_protocol::session::ContentPart;
use controlroom_protocol::timeline::{ItemBody, MessageRole, TimelineOp, ToolStatus};

/// The longest preview, in characters.
pub(crate) const PREVIEW_CHARS: usize = 200;

/// What the timeline offers the preview.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewSource {
    /// Running tool calls: item id → title.
    tools: BTreeMap<ItemId, String>,
    /// The last agent message: its id and text.
    agent_text: Option<(ItemId, String)>,
}

impl PreviewSource {
    /// Follows one op.
    pub(crate) fn apply(&mut self, op: &TimelineOp) {
        match op {
            TimelineOp::Upsert { item } => match &item.body {
                ItemBody::ToolCall { title, status, .. } => {
                    if matches!(status, ToolStatus::Pending | ToolStatus::Running) {
                        self.tools.insert(item.id.clone(), title.clone());
                    } else {
                        self.tools.remove(&item.id);
                    }
                }
                ItemBody::Message {
                    role: MessageRole::Agent,
                    content,
                } => {
                    let text = content
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.agent_text = Some((item.id.clone(), text));
                }
                _ => {}
            },
            TimelineOp::AppendText { id, text, .. } => {
                if let Some((last, buf)) = &mut self.agent_text
                    && last == id
                {
                    buf.push_str(text);
                }
            }
            TimelineOp::End { .. } | TimelineOp::Unknown => {}
        }
    }

    /// Forgets running tools (the agent process ended).
    pub(crate) fn clear_tools(&mut self) {
        self.tools.clear();
    }

    /// The most recent running tool's title.
    pub(crate) fn running_tool(&self) -> Option<&str> {
        self.tools.values().next_back().map(String::as_str)
    }

    /// The last non-empty line of the last agent message.
    pub(crate) fn last_agent_line(&self) -> Option<String> {
        let (_, text) = self.agent_text.as_ref()?;
        let plain = strip_markdown(text);
        plain
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(str::to_owned)
    }
}

/// One line: Markdown stripped, whitespace collapsed, cut to [`PREVIEW_CHARS`].
pub(crate) fn one_line(text: &str) -> String {
    let plain = strip_markdown(text);
    let collapsed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PREVIEW_CHARS {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(PREVIEW_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Plain text from Markdown: links and images become their label; code backticks, emphasis
/// markers, heading, quote and list markers are dropped. Line structure is kept.
pub(crate) fn strip_markdown(text: &str) -> String {
    text.lines()
        .map(|line| strip_inline(strip_block_marker(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_block_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    let without_heading = trimmed.trim_start_matches('#');
    if without_heading.len() < trimmed.len() && without_heading.starts_with(' ') {
        return without_heading.trim_start();
    }
    for marker in ["> ", "- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest;
        }
    }
    trimmed
}

fn strip_inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // `[label](url)` and `![alt](url)` keep the label.
        if c == '[' || (c == '!' && chars.get(i + 1) == Some(&'[')) {
            let open = if c == '!' { i + 1 } else { i };
            if let Some((label, end)) = link(&chars, open) {
                out.push_str(&strip_inline(&label));
                i = end;
                continue;
            }
        }
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let boundary = |ch: Option<char>| ch.is_none_or(|ch| !ch.is_alphanumeric());
        match c {
            '`' => {}
            '*' | '~' => {}
            // Underscores mark emphasis only at word edges (`_this_`), not in `snake_case`.
            '_' if boundary(prev) || boundary(next) => {}
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Parses `[label](target)` starting at `open` (the `[`); returns the label and the index
/// after the `)`.
fn link(chars: &[char], open: usize) -> Option<(String, usize)> {
    let close = open + chars[open..].iter().position(|&c| c == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = close + 1 + chars[close + 1..].iter().position(|&c| c == ')')?;
    Some((chars[open + 1..close].iter().collect(), end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown() {
        assert_eq!(
            strip_markdown("## Done: see [the docs](https://example.com) and `cargo test`"),
            "Done: see the docs and cargo test"
        );
        assert_eq!(
            strip_markdown("**bold** _em_ ~~gone~~ snake_case"),
            "bold em gone snake_case"
        );
        assert_eq!(
            strip_markdown("> quoted\n- item\n![logo](x.png)"),
            "quoted\nitem\nlogo"
        );
    }

    #[test]
    fn one_line_collapses_and_cuts() {
        assert_eq!(one_line("a\n\n  b\tc"), "a b c");
        let long = "x".repeat(500);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), PREVIEW_CHARS);
        assert!(cut.ends_with('…'));
    }
}
