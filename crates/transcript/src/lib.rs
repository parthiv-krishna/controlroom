//! The transcript: an append-only JSONL log of one supervised process's stdio.
//!
//! A supervisor (`controlroom-supervisor`) writes `transcript.jsonl`; every line is
//! one [`Record`]:
//!
//! ```text
//! {"seq":1,"ts":1700000000000,"dir":"meta","format":1,"id":"s1","kind":"agent","argv":[..],...}
//! {"seq":2,"ts":1700000000001,"dir":"in","raw":"{\"id\":\"1\",\"type\":\"get_state\"}"}
//! {"seq":3,"ts":1700000000005,"dir":"out","raw":"{\"id\":\"1\",\"type\":\"response\",...}"}
//! {"seq":4,"ts":1700000000006,"dir":"stderr","raw":"a very long li","truncated":81234}
//! {"seq":5,"ts":1700000000100,"dir":"exit","code":null,"signal":15,"cause":"timeout"}
//! ```
//!
//! - `seq` is monotonic from 1 across the whole file. A later supervisor for the same
//!   transcript (after a resume) appends and continues the numbering.
//! - `ts` is unix time in milliseconds.
//! - `dir` is `in` (written to the child's stdin), `out` (child stdout), `stderr`,
//!   `exit` (see [`ExitInfo`]), `meta` (a supervisor started the child; see
//!   [`Meta`], which carries [`FORMAT_VERSION`]) or `pty` (raw terminal output, see
//!   [`PtyChunk`]). Unknown `dir`s decode as
//!   [`Body::Unknown`] and round-trip unchanged.
//! - `raw` is the line without its `\n`. A line cut at the supervisor's line cap has
//!   `truncated` (bytes dropped); one that wasn't valid UTF-8 has `lossy: true`.
//!
//! # API
//!
//! - [`Writer`]: the single writer. Holds an exclusive `flock`, truncates a partial
//!   final line left by a crash, appends one complete line per `write(2)`, keeps the
//!   seq index current, and fsyncs on request ([`Writer::sync`], or a [`Syncer`]
//!   from another thread without blocking appends).
//! - [`Reader`]: reads records from any seq and keeps following the file as it
//!   grows. It starts from the seq index (`transcript.idx`, see [`index_path`])
//!   instead of scanning from the beginning. [`read_all`] reads everything at once.
//!
//! The crate knows nothing about agent protocols; drivers interpret `raw`.

mod index;
mod pty;
mod reader;
mod record;
mod scan;
mod writer;

pub use index::index_path;
pub use pty::{PTY_DIR, PtyChunk};
pub use reader::{PartialTail, ReadAll, ReadError, Reader, read_all};
pub use record::{
    Body, ExitInfo, FORMAT_VERSION, Line, Meta, ProcessKind, Record, StdinMode, StopCause,
    UnknownBody, unix_ms_now,
};
pub use scan::MAX_RECORD_BYTES;
pub use writer::{OpenReport, Syncer, Writer};
