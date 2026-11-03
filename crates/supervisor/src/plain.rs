//! Plain text for humans and agents: output lines with terminal escape sequences removed and
//! carriage-return progress updates collapsed to what a terminal would finally show.
//!
//! A job's log file (`<cwd>/.controlroom/jobs/<id>.log`) holds one [`plain_line`] per output
//! record, and the daemon matches follow patterns against the same text, so line numbers and
//! matches agree with what the agent reads.

use std::borrow::Cow;

/// The plain text of one output line (without its newline):
///
/// - a trailing `\r` (CRLF line endings) is dropped;
/// - `\r` inside the line rewinds it: of the segments between carriage returns, the last one
///   that is non-empty after stripping wins (`"10%\r50%\r100%"` → `"100%"`);
/// - escape sequences (CSI such as colors and cursor moves, OSC such as titles and links,
///   two-byte escapes) and other control characters except tab are removed.
pub fn plain_line(raw: &str) -> Cow<'_, str> {
    let line = raw.strip_suffix('\r').unwrap_or(raw);
    if !line.chars().any(|c| c.is_control() && c != '\t') {
        return Cow::Borrowed(line);
    }
    let mut last = String::new();
    for segment in line.split('\r') {
        let text = strip_escapes(segment);
        if !text.is_empty() {
            last = text;
        }
    }
    Cow::Owned(last)
}

/// Where the escape-sequence scanner is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    /// After ESC.
    Escape,
    /// Inside `ESC [` (or C1 CSI) until a final byte.
    Csi,
    /// Inside `ESC ]` (or `ESC P`, `ESC _`, `ESC ^`, C1 OSC) until BEL or ST.
    String,
    /// ESC seen inside a string: `ESC \` ends it.
    StringEscape,
}

/// Removes escape sequences and control characters other than tab.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut state = State::Text;
    for c in text.chars() {
        state = match (state, c) {
            (State::Text, '\u{1b}') => State::Escape,
            (State::Text, '\u{9b}') => State::Csi,
            (State::Text, '\u{9d}') => State::String,
            (State::Text, '\t') => {
                out.push(c);
                State::Text
            }
            (State::Text, c) if c.is_control() => State::Text,
            (State::Text, c) => {
                out.push(c);
                State::Text
            }
            (State::Escape, '[') => State::Csi,
            (State::Escape, ']' | 'P' | '_' | '^') => State::String,
            // Intermediate bytes (e.g. `ESC ( B`): the next byte ends the sequence.
            (State::Escape, ' '..='/') => State::Escape,
            (State::Escape, _) => State::Text,
            (State::Csi, '@'..='~') => State::Text,
            (State::Csi, _) => State::Csi,
            (State::String, '\u{7}' | '\u{9c}') => State::Text,
            (State::String, '\u{1b}') => State::StringEscape,
            (State::String, _) => State::String,
            (State::StringEscape, '\\') => State::Text,
            (State::StringEscape, _) => State::String,
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_borrowed() {
        assert!(matches!(plain_line("ok\ttab"), Cow::Borrowed("ok\ttab")));
        assert_eq!(plain_line("crlf\r"), "crlf");
    }

    #[test]
    fn colors_cursor_moves_and_titles_are_removed() {
        assert_eq!(
            plain_line("\u{1b}[1;31merror\u{1b}[0m: boom"),
            "error: boom"
        );
        assert_eq!(plain_line("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(
            plain_line("\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(plain_line("\u{1b}(Bplain\u{1b}=x"), "plainx");
        assert_eq!(plain_line("a\u{8}b\u{7}c"), "abc");
        assert_eq!(plain_line("\u{9b}2Kc1"), "c1");
    }

    #[test]
    fn carriage_returns_collapse_to_the_last_visible_segment() {
        assert_eq!(plain_line("10%\r50%\r100%"), "100%");
        assert_eq!(plain_line("building\r\u{1b}[K"), "building");
        assert_eq!(plain_line("\r\u{1b}[2K\r"), "");
        assert_eq!(plain_line("done\r\n".trim_end_matches('\n')), "done");
    }
}
