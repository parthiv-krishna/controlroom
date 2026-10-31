//! The fake agent's session: messages plus a pi-format session file.
//!
//! Layout and format follow pi (`docs/session-format.md`): sessions live in
//! `<agent dir>/sessions/--<cwd with / as ->--/<timestamp>_<id>.jsonl`, the
//! first line is a `session` header, every other line an entry with
//! `id`/`parentId`. The file is created lazily with the first message
//! entry; earlier entries are buffered and written with it.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::ids::{entry_id, file_stamp, iso, now_ms};

/// One session and its persistence.
#[derive(Debug)]
pub struct Session {
    pub id: String,
    /// The session file; `None` with `--no-session`.
    file: Option<PathBuf>,
    /// Whether the file has been written (header and buffered entries).
    created: bool,
    header: Value,
    entries: Vec<Value>,
    messages: Vec<Value>,
    pub name: Option<String>,
}

/// pi's directory name for a working directory.
pub fn encoded_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches('/');
    format!("--{}--", trimmed.replace(['/', '\\', ':'], "-"))
}

impl Session {
    /// A new session. With `dir` (the project's sessions directory) it is
    /// persisted there; `model` and `thinking` become its first entries.
    pub fn create(
        dir: Option<&Path>,
        id: String,
        cwd: &str,
        parent: Option<&str>,
        model: (&str, &str),
        thinking: &str,
    ) -> Self {
        let now = now_ms();
        let mut header = json!({
            "type": "session", "version": 3, "id": id, "timestamp": iso(now), "cwd": cwd
        });
        if let Some(parent) = parent {
            header["parentSession"] = parent.into();
        }
        let file = dir.map(|dir| dir.join(format!("{}_{id}.jsonl", file_stamp(now))));
        let mut session = Self {
            id,
            file,
            created: false,
            header,
            entries: Vec::new(),
            messages: Vec::new(),
            name: None,
        };
        // Buffered entries can't fail to write.
        let _ = session
            .append(json!({"type": "model_change", "provider": model.0, "modelId": model.1}));
        let _ = session.append(json!({"type": "thinking_level_change", "thinkingLevel": thinking}));
        session
    }

    /// Open an existing session file.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: Value = serde_json::from_str(lines.next().unwrap_or_default())
            .with_context(|| format!("{}: bad session header", path.display()))?;
        if header["type"] != "session" {
            bail!("{}: not a session file", path.display());
        }
        let mut session = Self {
            id: header["id"].as_str().unwrap_or_default().to_owned(),
            file: Some(path.to_owned()),
            created: true,
            header,
            entries: Vec::new(),
            messages: Vec::new(),
            name: None,
        };
        for line in lines {
            let entry: Value = serde_json::from_str(line)
                .with_context(|| format!("{}: bad entry", path.display()))?;
            session.index(&entry);
            session.entries.push(entry);
        }
        Ok(session)
    }

    /// Find `<dir>/*_<id>.jsonl`.
    pub fn find(dir: &Path, id: &str) -> Option<PathBuf> {
        let suffix = format!("_{id}.jsonl");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(&suffix))
            })
    }

    fn index(&mut self, entry: &Value) {
        match entry["type"].as_str() {
            Some("message") => self.messages.push(entry["message"].clone()),
            Some("session_info") => self.name = entry["name"].as_str().map(str::to_owned),
            _ => {}
        }
    }

    /// Append an entry (`type` plus its fields); returns it with `id`,
    /// `parentId` and `timestamp` filled in.
    pub fn append(&mut self, mut entry: Value) -> Result<Value> {
        entry["id"] = entry_id().into();
        entry["parentId"] = self.leaf_id().map_or(Value::Null, Value::from);
        entry["timestamp"] = iso(now_ms()).into();
        self.index(&entry);
        self.entries.push(entry.clone());
        if entry["type"] == "message" && !self.created {
            self.create_file()?;
        } else if self.created {
            self.write_lines(std::slice::from_ref(&entry))?;
        }
        Ok(entry)
    }

    fn create_file(&mut self) -> Result<()> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        self.created = true;
        let mut lines = vec![self.header.clone()];
        lines.extend(self.entries.iter().cloned());
        self.write_lines(&lines)
    }

    fn write_lines(&self, lines: &[Value]) -> Result<()> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let mut text = String::new();
        for line in lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        file.write_all(text.as_bytes())
            .with_context(|| format!("writing {}", path.display()))
    }

    /// The session file path (reported even before the file exists).
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// All messages, in order.
    pub fn messages(&self) -> &[Value] {
        &self.messages
    }

    /// Whether a system message was sent in this session.
    pub fn has_system(&self) -> bool {
        self.messages.iter().any(|m| m["role"] == "system")
    }

    /// The id of the latest entry.
    pub fn leaf_id(&self) -> Option<String> {
        self.entries
            .last()
            .and_then(|e| e["id"].as_str())
            .map(str::to_owned)
    }

    /// Entries after `since` (all without it); `None` if `since` is unknown.
    pub fn entries_since(&self, since: Option<&str>) -> Option<&[Value]> {
        match since {
            None => Some(&self.entries),
            Some(id) => self
                .entries
                .iter()
                .position(|e| e["id"] == id)
                .map(|i| &self.entries[i + 1..]),
        }
    }

    /// The last value of `field` among entries of `kind`.
    pub fn latest(&self, kind: &str, field: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|e| e["type"] == kind)
            .and_then(|e| e[field].as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_is_created_with_the_first_message_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(
            Some(dir.path()),
            "s1".into(),
            "/home/alice/project",
            None,
            ("mock", "mock-model"),
            "medium",
        );
        let path = session.file().unwrap().to_owned();
        assert!(!path.exists());
        session
            .append(json!({"type": "custom", "customType": "x", "data": {}}))
            .unwrap();
        assert!(!path.exists());
        session
            .append(json!({"type": "message", "message": {"role": "user", "content": "hi"}}))
            .unwrap();
        session
            .append(json!({"type": "session_info", "name": "named"}))
            .unwrap();
        assert!(path.exists());

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.id, "s1");
        assert_eq!(loaded.messages().len(), 1);
        assert_eq!(loaded.entries_since(None).unwrap().len(), 5);
        assert_eq!(loaded.name.as_deref(), Some("named"));
        assert_eq!(loaded.latest("model_change", "modelId"), Some("mock-model"));
        let first = loaded.entries_since(None).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(loaded.entries_since(Some(&first)).unwrap().len(), 4);
        assert!(loaded.entries_since(Some("nope")).is_none());
        assert_eq!(
            Session::find(dir.path(), "s1").as_deref(),
            Some(path.as_path())
        );
    }

    #[test]
    fn encodes_cwd_like_pi() {
        assert_eq!(encoded_cwd("/home/alice/project"), "--home-alice-project--");
    }
}
