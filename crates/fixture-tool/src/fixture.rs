//! Fixture files: a transcript in the `controlroom-transcript` format plus a
//! sidecar of recorder actions.
//!
//! `<name>.jsonl` is exactly what a supervisor would have logged for the pi
//! process: a typed `meta` record first, then `in`/`out`/`stderr` lines, then
//! `exit`. What the recorder did besides writing stdin (releasing a mock
//! model gate, sending a signal, closing stdin) is not stdio, so it lives in
//! `<name>.actions.json`, each action placed after the record with
//! `after_seq`. Replays use both to reproduce the run.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use controlroom_transcript::{
    FORMAT_VERSION, MAX_RECORD_BYTES, Meta, ProcessKind, Record, StdinMode, read_all,
};
use serde::{Deserialize, Serialize};

/// `supervisor_version` of fixture `meta` records.
pub const RECORDER_VERSION: &str = "controlroom-fixture-tool";
/// Label holding the recording temp dir (scrubbed by the sanitizer).
pub const LABEL_ROOT: &str = "fixture.root";
/// Label holding the mock model's base URL.
pub const LABEL_BASE_URL: &str = "fixture.base_url";

/// Something the recorder did that pi did not see on stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// The mock model let a stream continue past `gate`.
    ReleaseGate { after_seq: u64, gate: String },
    /// A signal (`KILL`, `HUP`, `TERM`) was sent to pi.
    Signal { after_seq: u64, signal: String },
    /// pi's stdin was closed.
    CloseStdin { after_seq: u64 },
}

impl Action {
    /// The seq of the last record logged before the action.
    pub fn after_seq(&self) -> u64 {
        match self {
            Self::ReleaseGate { after_seq, .. }
            | Self::Signal { after_seq, .. }
            | Self::CloseStdin { after_seq } => *after_seq,
        }
    }
}

/// A recorded pi process: its transcript and the recorder's actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    pub records: Vec<Record>,
    pub actions: Vec<Action>,
}

/// The `meta` record of a fixture recording, before the process starts
/// (`argv`, `cwd` and the pids are filled in by the recorder).
pub fn recorder_meta(id: &str, root: &str, base_url: &str) -> Meta {
    Meta {
        format: FORMAT_VERSION,
        id: id.to_owned(),
        kind: ProcessKind::Agent,
        stdin: StdinMode::Pipe,
        max_line_bytes: MAX_RECORD_BYTES as u64,
        labels: BTreeMap::from([
            (LABEL_ROOT.to_owned(), root.to_owned()),
            (LABEL_BASE_URL.to_owned(), base_url.to_owned()),
        ]),
        supervisor_version: RECORDER_VERSION.to_owned(),
        ..Meta::default()
    }
}

/// The sidecar of `transcript` (`x.jsonl` → `x.actions.json`).
pub fn actions_path(transcript: &Path) -> PathBuf {
    transcript.with_extension("actions.json")
}

/// Read a transcript; any bad or incomplete line is an error.
pub fn read_transcript(path: &Path) -> Result<Vec<Record>> {
    let all = read_all(path, 0).with_context(|| format!("reading {}", path.display()))?;
    if let Some((offset, error)) = all.corrupt.first() {
        bail!("{}: bad record at byte {offset}: {error}", path.display());
    }
    if let Some(tail) = all.partial_tail {
        bail!(
            "{}: incomplete final line at byte {}",
            path.display(),
            tail.offset
        );
    }
    Ok(all.records)
}

/// Read a fixture (transcript plus actions).
pub fn read(path: &Path) -> Result<Fixture> {
    let actions_file = actions_path(path);
    let text = std::fs::read_to_string(&actions_file)
        .with_context(|| format!("reading {}", actions_file.display()))?;
    Ok(Fixture {
        records: read_transcript(path)?,
        actions: serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", actions_file.display()))?,
    })
}

/// Write a fixture: records keep their `seq` and `ts` and are serialized by
/// the transcript crate; actions go to the sidecar, one per line.
pub fn write(path: &Path, fixture: &Fixture) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut text = Vec::new();
    for record in &fixture.records {
        serde_json::to_writer(&mut text, record)?;
        text.push(b'\n');
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    let mut actions = String::from("[\n");
    for (i, action) in fixture.actions.iter().enumerate() {
        let sep = if i + 1 < fixture.actions.len() {
            ","
        } else {
            ""
        };
        actions.push_str(&format!("  {}{sep}\n", serde_json::to_string(action)?));
    }
    actions.push_str("]\n");
    let actions_file = actions_path(path);
    std::fs::File::create(&actions_file)
        .and_then(|mut f| f.write_all(actions.as_bytes()))
        .with_context(|| format!("writing {}", actions_file.display()))
}

#[cfg(test)]
mod tests {
    use controlroom_transcript::{Body, ExitInfo, Line};

    use super::*;

    #[test]
    fn round_trips_in_the_transcript_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/t.jsonl");
        let fixture = Fixture {
            records: vec![
                Record {
                    seq: 1,
                    ts: 5,
                    body: Body::Meta(Box::new(recorder_meta("t", "/r", "http://x/v1"))),
                },
                Record {
                    seq: 2,
                    ts: 6,
                    body: Body::In(Line::new(r#"{"type":"get_state"}"#)),
                },
                Record {
                    seq: 3,
                    ts: 7,
                    body: Body::Exit(ExitInfo {
                        code: Some(0),
                        signal: None,
                        cause: None,
                    }),
                },
            ],
            actions: vec![Action::CloseStdin { after_seq: 2 }],
        };
        write(&path, &fixture).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#"{"seq":2,"ts":6,"dir":"in","raw":"#));
        assert!(
            text.ends_with("{\"seq\":3,\"ts\":7,\"dir\":\"exit\",\"code\":0,\"signal\":null}\n")
        );
        assert_eq!(read(&path).unwrap(), fixture);
        assert_eq!(
            std::fs::read_to_string(actions_path(&path)).unwrap(),
            "[\n  {\"action\":\"close_stdin\",\"after_seq\":2}\n]\n"
        );
    }
}
