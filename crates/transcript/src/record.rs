//! The transcript record type and its JSON form.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// Version of the record format, stored in every [`Meta`] record.
pub const FORMAT_VERSION: u32 = 1;

/// One transcript line: `{seq, ts, dir, ...}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Monotonic from 1 across the whole transcript.
    pub seq: u64,
    /// Unix time in milliseconds when the line was logged.
    pub ts: u64,
    /// The `dir`-tagged rest of the record.
    pub body: Body,
}

/// The `dir`-tagged part of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// `in`: a line written to the child's stdin.
    In(Line),
    /// `out`: a line read from the child's stdout.
    Out(Line),
    /// `stderr`: a line read from the child's stderr.
    Stderr(Line),
    /// `exit`: the child process ended.
    Exit(ExitInfo),
    /// `meta`: a supervisor started a child; the first record of every supervisor run.
    Meta(Box<Meta>),
    /// A `dir` this version doesn't know, kept verbatim so it round-trips.
    Unknown(UnknownBody),
}

/// A line of child stdio.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// The line without its `\n` terminator (possibly cut, see `truncated`).
    pub raw: String,
    /// Bytes dropped from the end of an overlong line (0 if the line is complete).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub truncated: u64,
    /// Whether the line was not valid UTF-8 and invalid bytes became U+FFFD.
    #[serde(default, skip_serializing_if = "is_false")]
    pub lossy: bool,
}

impl Line {
    /// A complete, valid UTF-8 line.
    pub fn new(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            ..Self::default()
        }
    }
}

/// How the child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Signal number, if the process was killed by a signal.
    pub signal: Option<i32>,
    /// Why the supervisor asked the process to stop, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<StopCause>,
}

/// Why a supervisor stopped its child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCause {
    /// A client sent `terminate`.
    Terminate,
    /// The spawn spec's timeout elapsed.
    Timeout,
    /// The supervisor itself received SIGTERM or SIGINT.
    Signal,
    /// A cause this version doesn't know.
    #[serde(other)]
    Unknown,
}

/// What kind of process a supervisor runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    /// A coding agent speaking a line protocol on stdio.
    Agent,
    /// A background job (a command with line output).
    Job,
    /// A terminal (reserved: not supported yet).
    Pty,
    /// A kind this version doesn't know.
    #[default]
    #[serde(other)]
    Unknown,
}

/// What the child's stdin is connected to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    /// A pipe the supervisor keeps open; clients write lines to it.
    #[default]
    Pipe,
    /// `/dev/null`.
    Null,
    /// A mode this version doesn't know.
    #[serde(other)]
    Unknown,
}

/// Start information written as the first record of every supervisor run.
///
/// Fields default when missing so that older and newer meta records still decode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    /// Record format version ([`FORMAT_VERSION`]).
    pub format: u32,
    /// Id of the supervised process (a session or job id).
    pub id: String,
    /// What kind of process this is.
    pub kind: ProcessKind,
    /// The exact argv that was executed.
    pub argv: Vec<String>,
    /// The child's working directory.
    pub cwd: String,
    /// What the child's stdin is connected to.
    pub stdin: StdinMode,
    /// The run timeout, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Longest stdio line logged in full; longer lines are truncated.
    pub max_line_bytes: u64,
    /// Opaque labels from the spawn spec (for example the driver name).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Pid of the child, which is also its process group id.
    pub child_pid: u32,
    /// Pid of the supervisor.
    pub supervisor_pid: u32,
    /// Supervisor socket protocol version.
    pub protocol_version: u32,
    /// Version of the supervisor binary.
    pub supervisor_version: String,
}

/// The fields of a record with an unknown `dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownBody {
    /// The unknown `dir` value.
    pub dir: String,
    /// All other fields except `seq` and `ts`.
    pub fields: Map<String, Value>,
}

impl Record {
    /// The line for `in`, `out` and `stderr` records.
    pub fn line(&self) -> Option<&Line> {
        match &self.body {
            Body::In(line) | Body::Out(line) | Body::Stderr(line) => Some(line),
            Body::Exit(_) | Body::Meta(_) | Body::Unknown(_) => None,
        }
    }

    /// The raw text of `in`, `out` and `stderr` records.
    pub fn raw(&self) -> Option<&str> {
        self.line().map(|line| line.raw.as_str())
    }
}

/// Current unix time in milliseconds (0 if the clock is before 1970).
pub fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Serialization view of a record: `seq`, `ts`, then the flattened body.
#[derive(Serialize)]
struct RecordRef<'a> {
    seq: u64,
    ts: u64,
    #[serde(flatten)]
    body: BodyRef<'a>,
}

#[derive(Serialize)]
#[serde(tag = "dir", rename_all = "lowercase")]
enum BodyRef<'a> {
    In(&'a Line),
    Out(&'a Line),
    Stderr(&'a Line),
    Exit(&'a ExitInfo),
    Meta(&'a Meta),
    #[serde(untagged)]
    Unknown(UnknownRef<'a>),
}

#[derive(Serialize)]
struct UnknownRef<'a> {
    dir: &'a str,
    #[serde(flatten)]
    fields: &'a Map<String, Value>,
}

impl Serialize for Record {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let body = match &self.body {
            Body::In(line) => BodyRef::In(line),
            Body::Out(line) => BodyRef::Out(line),
            Body::Stderr(line) => BodyRef::Stderr(line),
            Body::Exit(exit) => BodyRef::Exit(exit),
            Body::Meta(meta) => BodyRef::Meta(meta),
            Body::Unknown(unknown) => BodyRef::Unknown(UnknownRef {
                dir: &unknown.dir,
                fields: &unknown.fields,
            }),
        };
        RecordRef {
            seq: self.seq,
            ts: self.ts,
            body,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = Map::deserialize(deserializer)?;
        let mut take_u64 = |name: &'static str| {
            fields
                .remove(name)
                .and_then(|v| v.as_u64())
                .ok_or_else(|| D::Error::missing_field(name))
        };
        let seq = take_u64("seq")?;
        let ts = take_u64("ts")?;
        let dir = match fields.remove("dir") {
            Some(Value::String(dir)) => dir,
            _ => return Err(D::Error::missing_field("dir")),
        };
        fn decode<T: serde::de::DeserializeOwned, E: serde::de::Error>(
            fields: Map<String, Value>,
        ) -> Result<T, E> {
            serde_json::from_value(Value::Object(fields)).map_err(E::custom)
        }
        let body = match dir.as_str() {
            "in" => Body::In(decode(fields)?),
            "out" => Body::Out(decode(fields)?),
            "stderr" => Body::Stderr(decode(fields)?),
            "exit" => Body::Exit(decode(fields)?),
            "meta" => Body::Meta(Box::new(decode(fields)?)),
            _ => Body::Unknown(UnknownBody { dir, fields }),
        };
        Ok(Self { seq, ts, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(record: &Record, json: &str) {
        assert_eq!(serde_json::to_string(record).unwrap(), json);
        assert_eq!(&serde_json::from_str::<Record>(json).unwrap(), record);
    }

    #[test]
    fn line_records() {
        let rec = Record {
            seq: 3,
            ts: 10,
            body: Body::Out(Line::new(r#"{"id":1}"#)),
        };
        round_trip(&rec, r#"{"seq":3,"ts":10,"dir":"out","raw":"{\"id\":1}"}"#);

        let cut = Record {
            seq: 4,
            ts: 11,
            body: Body::Stderr(Line {
                raw: "ab\u{fffd}".into(),
                truncated: 7,
                lossy: true,
            }),
        };
        round_trip(
            &cut,
            r#"{"seq":4,"ts":11,"dir":"stderr","raw":"ab�","truncated":7,"lossy":true}"#,
        );
    }

    #[test]
    fn exit_records() {
        let exit = Record {
            seq: 4,
            ts: 11,
            body: Body::Exit(ExitInfo {
                code: None,
                signal: Some(9),
                cause: None,
            }),
        };
        round_trip(
            &exit,
            r#"{"seq":4,"ts":11,"dir":"exit","code":null,"signal":9}"#,
        );

        let timed_out = Record {
            seq: 5,
            ts: 12,
            body: Body::Exit(ExitInfo {
                code: None,
                signal: Some(15),
                cause: Some(StopCause::Timeout),
            }),
        };
        round_trip(
            &timed_out,
            r#"{"seq":5,"ts":12,"dir":"exit","code":null,"signal":15,"cause":"timeout"}"#,
        );
    }

    #[test]
    fn meta_round_trips_and_tolerates_missing_and_unknown_fields() {
        let rec = Record {
            seq: 1,
            ts: 1,
            body: Body::Meta(Box::new(Meta {
                format: FORMAT_VERSION,
                id: "s1".into(),
                kind: ProcessKind::Job,
                argv: vec!["sh".into(), "-c".into(), "true".into()],
                cwd: "/home/alice".into(),
                stdin: StdinMode::Null,
                timeout_ms: Some(1000),
                max_line_bytes: 64,
                labels: BTreeMap::from([("driver".into(), "pi".into())]),
                child_pid: 2,
                supervisor_pid: 1,
                protocol_version: 1,
                supervisor_version: "0.0.1".into(),
            })),
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains(r#""dir":"meta""#), "{json}");
        assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), rec);

        let future =
            r#"{"seq":1,"ts":1,"dir":"meta","format":9,"kind":"vm","stdin":"socket","new":1}"#;
        let Body::Meta(meta) = serde_json::from_str::<Record>(future).unwrap().body else {
            panic!("not meta");
        };
        assert_eq!(meta.format, 9);
        assert_eq!(meta.kind, ProcessKind::Unknown);
        assert_eq!(meta.stdin, StdinMode::Unknown);
    }

    #[test]
    fn unknown_dir_round_trips() {
        let json = r#"{"seq":7,"ts":2,"dir":"bytes","data":"aGk=","n":2}"#;
        let rec: Record = serde_json::from_str(json).unwrap();
        let Body::Unknown(unknown) = &rec.body else {
            panic!("not unknown: {rec:?}");
        };
        assert_eq!(unknown.dir, "bytes");
        assert_eq!(unknown.fields["n"], 2);
        assert_eq!(rec.raw(), None);
        let again: Record = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert_eq!(again, rec);
    }

    #[test]
    fn unknown_stop_cause_decodes() {
        let json = r#"{"seq":1,"ts":1,"dir":"exit","code":0,"signal":null,"cause":"oom"}"#;
        let rec: Record = serde_json::from_str(json).unwrap();
        assert!(matches!(
            rec.body,
            Body::Exit(ExitInfo {
                cause: Some(StopCause::Unknown),
                ..
            })
        ));
    }

    #[test]
    fn malformed_records_are_rejected() {
        for bad in [
            r#"{"ts":1,"dir":"out","raw":"x"}"#,
            r#"{"seq":1,"ts":1,"raw":"x"}"#,
            r#"{"seq":1,"ts":1,"dir":"out"}"#,
            r#"[1,2]"#,
        ] {
            assert!(serde_json::from_str::<Record>(bad).is_err(), "{bad}");
        }
    }
}
