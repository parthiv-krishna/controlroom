//! Terminal output records: `{"seq":..,"ts":..,"dir":"pty","data":"<base64>"}`.
//!
//! A PTY supervisor logs the raw bytes it reads from the terminal's master side, in chunks as
//! they arrive (escape sequences and UTF-8 characters may straddle chunks). The records are
//! not a [`Body`] variant of their own: readers that predate terminals (the agent drivers)
//! decode them as [`Body::Unknown`] and skip them, as the format's tolerance rules intend.
//! [`PtyChunk`] builds and recognizes them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use crate::record::{Body, Record, UnknownBody};

/// The `dir` of a terminal output record.
pub const PTY_DIR: &str = "pty";

/// Bytes read from a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyChunk(pub Vec<u8>);

impl PtyChunk {
    /// The record body for these bytes.
    pub fn into_body(self) -> Body {
        let mut fields = Map::new();
        fields.insert("data".to_owned(), Value::String(STANDARD.encode(&self.0)));
        Body::Unknown(UnknownBody {
            dir: PTY_DIR.to_owned(),
            fields,
        })
    }

    /// The bytes of a terminal output record; `None` for any other record (or a malformed
    /// one).
    pub fn from_body(body: &Body) -> Option<Self> {
        let Body::Unknown(unknown) = body else {
            return None;
        };
        if unknown.dir != PTY_DIR {
            return None;
        }
        let data = unknown.fields.get("data")?.as_str()?;
        STANDARD.decode(data).ok().map(Self)
    }

    /// [`from_body`](Self::from_body) of a record.
    pub fn from_record(record: &Record) -> Option<Self> {
        Self::from_body(&record.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_round_trip_as_records() {
        let bytes = b"\x1b]133;D;0\x07\xff\xfeprompt$ ".to_vec();
        let record = Record {
            seq: 4,
            ts: 9,
            body: PtyChunk(bytes.clone()).into_body(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            json.starts_with(r#"{"seq":4,"ts":9,"dir":"pty","data":""#),
            "{json}"
        );
        let back: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(PtyChunk::from_record(&back), Some(PtyChunk(bytes)));
        assert_eq!(back.raw(), None);
    }

    #[test]
    fn other_records_are_not_chunks() {
        let out = Body::Out(crate::Line::new("x"));
        assert_eq!(PtyChunk::from_body(&out), None);
        let other: Record =
            serde_json::from_str(r#"{"seq":1,"ts":1,"dir":"bytes","data":"aGk="}"#).unwrap();
        assert_eq!(PtyChunk::from_record(&other), None);
        let bad: Record =
            serde_json::from_str(r#"{"seq":1,"ts":1,"dir":"pty","data":"%%"}"#).unwrap();
        assert_eq!(PtyChunk::from_record(&bad), None);
    }
}
