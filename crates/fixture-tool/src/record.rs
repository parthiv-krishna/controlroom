//! A minimal local copy of the transcript record shape.
//!
//! Every line of a fixture is one record:
//! `{"seq":1,"ts":1700000000000,"dir":"in","raw":"{...}"}`.
//!
//! - `in`: a line written to pi's stdin; `out`: a line read from its stdout;
//!   `stderr`: a line read from its stderr. `raw` is the exact line without
//!   its trailing LF.
//! - `exit`: pi exited; `raw` is `{"code":<i32|null>,"signal":<i32|null>}`.
//! - `meta`: an annotation by the recorder (never seen by pi); `raw` is a
//!   JSON object with a `fixture` key, e.g. a mock-model gate release.
//!
//! The `transcript` crate owns the authoritative format; this module is
//! swapped for it at merge.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Direction of a record, relative to pi.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
    Stderr,
    Exit,
    Meta,
}

/// One transcript record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Monotonic from 1 within one transcript.
    pub seq: u64,
    /// Unix milliseconds when the line was observed.
    pub ts: u64,
    pub dir: Dir,
    pub raw: String,
}

/// Current unix time in milliseconds.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Read a JSONL transcript.
pub fn read_jsonl(path: &Path) -> Result<Vec<Record>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("{}:{}: parsing record", path.display(), i + 1))
        })
        .collect()
}

/// Write a JSONL transcript, creating parent directories.
pub fn write_jsonl(path: &Path, records: &[Record]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record)?);
        text.push('\n');
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/t.jsonl");
        let records = vec![
            Record {
                seq: 1,
                ts: 5,
                dir: Dir::In,
                raw: r#"{"type":"get_state"}"#.into(),
            },
            Record {
                seq: 2,
                ts: 6,
                dir: Dir::Exit,
                raw: r#"{"code":0,"signal":null}"#.into(),
            },
        ];
        write_jsonl(&path, &records).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(r#"{"seq":1,"ts":5,"dir":"in","raw":"#));
        assert_eq!(read_jsonl(&path).unwrap(), records);
    }
}
