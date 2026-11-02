//! How pi's tool calls are presented: title, kind, input text and final output.

use controlroom_protocol::timeline::{ToolKind, ToolOutput};
use serde_json::Value;

const TITLE_MAX: usize = 120;

/// The neutral kind of a pi tool.
pub(crate) fn kind(name: &str) -> ToolKind {
    match name {
        "read" => ToolKind::Read,
        "edit" | "write" => ToolKind::Edit,
        "bash" => ToolKind::Execute,
        "grep" | "find" | "ls" => ToolKind::Search,
        _ => ToolKind::Other,
    }
}

/// A one-line title, e.g. `Read notes.txt` or the bash command's first line.
pub(crate) fn title(name: &str, args: &Value) -> String {
    let arg = |key: &str| args.get(key).and_then(Value::as_str);
    let title = match (name, arg("path"), arg("pattern"), arg("command")) {
        ("bash", _, _, Some(command)) => command.lines().next().unwrap_or("").to_owned(),
        ("read", Some(path), ..) => format!("Read {path}"),
        ("edit", Some(path), ..) => format!("Edit {path}"),
        ("write", Some(path), ..) => format!("Write {path}"),
        ("grep", _, Some(pattern), _) => format!("Search {pattern}"),
        ("find", _, Some(pattern), _) => format!("Find {pattern}"),
        ("ls", path, ..) => format!("List {}", path.unwrap_or(".")),
        _ => name.to_owned(),
    };
    truncate(title)
}

/// The input as display text: the command for bash, else pretty-printed arguments.
pub(crate) fn input(name: &str, args: &Value) -> Option<String> {
    if name == "bash"
        && let Some(command) = args.get("command").and_then(Value::as_str)
    {
        return Some(command.to_owned());
    }
    match args {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        args => serde_json::to_string_pretty(args).ok(),
    }
}

/// The output for streamed or final result text.
pub(crate) fn text_output(name: &str, text: String) -> ToolOutput {
    if name == "bash" {
        ToolOutput::Terminal { text }
    } else {
        ToolOutput::Text { text }
    }
}

/// The final output: a diff for successful `edit`/`write` calls, else the result text.
pub(crate) fn final_output(name: &str, args: &Value, is_error: bool, text: String) -> ToolOutput {
    if !is_error && let Some(diff) = diff(name, args) {
        return diff;
    }
    text_output(name, text)
}

/// The exit code a bash result reports.
pub(crate) fn exit_code(structured: &Value) -> Option<i32> {
    structured
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

/// `write {path, content}` is a new file; `edit {path, edits: [{oldText, newText}]}` joins its
/// replaced blocks with a blank line.
fn diff(name: &str, args: &Value) -> Option<ToolOutput> {
    let path = args.get("path")?.as_str()?.to_owned();
    match name {
        "write" => Some(ToolOutput::Diff {
            path,
            old: String::new(),
            new: args.get("content")?.as_str()?.to_owned(),
        }),
        "edit" => {
            let edits = args.get("edits")?.as_array()?;
            let side = |key: &str| -> Option<String> {
                let blocks: Option<Vec<&str>> =
                    edits.iter().map(|e| e.get(key)?.as_str()).collect();
                Some(blocks?.join("\n\n"))
            };
            if edits.is_empty() {
                return None;
            }
            Some(ToolOutput::Diff {
                path,
                old: side("oldText")?,
                new: side("newText")?,
            })
        }
        _ => None,
    }
}

fn truncate(mut text: String) -> String {
    if text.chars().count() > TITLE_MAX {
        text = text.chars().take(TITLE_MAX - 1).collect();
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn titles_and_kinds() {
        assert_eq!(title("bash", &json!({"command": "ls\npwd"})), "ls");
        assert_eq!(
            title("read", &json!({"path": "notes.txt"})),
            "Read notes.txt"
        );
        assert_eq!(title("ls", &json!({})), "List .");
        assert_eq!(
            title("fixture_nested", &json!({"path": "x"})),
            "fixture_nested"
        );
        assert_eq!(
            title("bash", &json!({"command": "x".repeat(300)}))
                .chars()
                .count(),
            TITLE_MAX
        );
        assert_eq!(kind("edit"), ToolKind::Edit);
        assert_eq!(kind("mystery"), ToolKind::Other);
    }

    #[test]
    fn edits_become_diffs() {
        let args = json!({"path": "a.txt", "edits": [
            {"oldText": "b", "newText": "B"}, {"oldText": "c", "newText": "C"}
        ]});
        assert_eq!(
            final_output("edit", &args, false, "ok".into()),
            ToolOutput::Diff {
                path: "a.txt".into(),
                old: "b\n\nc".into(),
                new: "B\n\nC".into()
            }
        );
        assert_eq!(
            final_output("edit", &args, true, "no match".into()),
            ToolOutput::Text {
                text: "no match".into()
            }
        );
        assert_eq!(
            final_output("edit", &json!({"path": "a"}), false, "ok".into()),
            ToolOutput::Text { text: "ok".into() }
        );
    }
}
