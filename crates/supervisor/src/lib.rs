//! The supervisor: one detached process per agent or job, a deliberately dumb pipe
//! with a durable log.
//!
//! A supervisor spawns its child in a new process group with piped stdio, logs every
//! line in both directions to the child's transcript (`controlroom-transcript`), and
//! serves a unix socket through which the daemon reads the log and writes to the
//! child's stdin. It understands no agent protocol. It keeps the child's stdin open
//! and keeps draining stdout and stderr whether or not a client is attached, so
//! nothing is lost while the daemon is away. It never restarts the child or itself:
//! when the child exits it kills what is left of the child's process group, appends
//! an `exit` record, fsyncs, removes its socket and exits.
//!
//! **Write path invariant:** a line is in the transcript, and fsynced, before the
//! child can read it.
//!
//! # API (for the daemon)
//!
//! - [`SpawnSpec`]: `{kind: agent|job (pty reserved), exec: {argv}|{shell}, cwd,
//!   env_file?, stdin: pipe|null, timeout_ms?, max_line_bytes?, labels}`. The
//!   supervisor is sandbox-agnostic: a sandboxed launch is a plain argv such as
//!   `nono run ... -- $SHELL -lc '<cmd>'`.
//! - [`Paths`]: the process directory and the socket path (mind [`SUN_PATH_MAX`]).
//! - [`launch`] starts a detached supervisor and returns its [`SupervisorInfo`];
//!   [`read_info`] finds the one a running supervisor left in its directory.
//! - [`Client`] talks to a supervisor: [`read`](Client::read),
//!   [`write`](Client::write), [`barrier`](Client::barrier), [`sync`](Client::sync),
//!   [`status`](Client::status), [`terminate`](Client::terminate).
//! - [`cli`] is the `controlroomd supervise` subcommand.
//!
//! # On-disk layout
//!
//! ```text
//! <dir>/transcript.jsonl   records; 0600 (see controlroom-transcript)
//! <dir>/transcript.idx     seq index (a rebuildable cache)
//! <dir>/supervisor.json    SupervisorInfo of the running supervisor; written atomically
//! <dir>/supervisor.log     stderr of a detached supervisor
//! <socket>                 0600; the daemon picks a short path, e.g. <state>/run/<id>.sock
//! ```
//!
//! Each supervisor run starts with a `meta` record and appends to an existing
//! transcript, continuing its seq. The supervisor holds the transcript's `flock`
//! while it runs, so a second supervisor for the same directory refuses to start. A
//! leftover socket is replaced only if nothing answers on it.
//!
//! # Socket protocol (version [`PROTOCOL_VERSION`])
//!
//! Newline-delimited JSON in both directions. Requests are
//! `{"id":<any JSON>,"method":"...","params":{...}}` (`params` optional); responses
//! echo the id as `{"id":..,"result":{..}}` or
//! `{"id":..,"error":{"code":"<snake_case>","message":".."}}`, and streamed records
//! arrive as `{"id":<read id>,"record":{..}}`. Unknown fields are ignored; unknown
//! methods get `unknown_method`. Error codes: see [`ErrorCode`].
//!
//! - `hello {protocol_version, writer?}` must come first. The result is [`Hello`].
//!   The supervisor speaks one version and always answers with it; clients accept
//!   N-1..=N ([`MIN_PROTOCOL_VERSION`]), since supervisors outlive daemon upgrades.
//!   `writer: true` claims the single writer role (`writer_busy` while another
//!   connection holds it). The role is released when that connection closes, but
//!   only after every request it sent has been handled.
//! - `read {from_seq?}` answers `{last_seq}`, then streams every record with
//!   `seq >= from_seq` (found through the seq index), then live records, ending
//!   after this run's `exit` record. A new `read` replaces the connection's stream.
//!   Records are streamed from the file: a slow reader falls behind, never loses
//!   records.
//! - `write {line}` (writer only): queues the line; the logger appends an `in`
//!   record, fsyncs, answers `{seq}`, then hands `line + "\n"` to the child's stdin.
//!   Writes from all connections apply in arrival order. Refused with
//!   `line_too_long` above `max_line_bytes`, `no_stdin` for `stdin: null`, and
//!   `child_exited` once the child is reaped, so no `in` record follows `exit`.
//! - `barrier`: answers `{durable_seq}` once every write accepted before it is
//!   logged and fsynced. A daemon that just took over the writer role uses it to
//!   end its in-doubt state.
//! - `sync`: fsyncs what is logged now; `{durable_seq}`. The supervisor also fsyncs
//!   output records within a second, and on exit.
//! - `status`: [`Status`] (`alive` turns false as soon as the child is reaped,
//!   `exit` is set once the exit record is written).
//! - `terminate {grace_ms?}` (writer only, default 5000): SIGTERM to the child's
//!   process group, SIGKILL after the grace. `{already_exited}`; the exit itself
//!   arrives as the `exit` record, with `cause: "terminate"`.
//!
//! When the child exits, the supervisor stops accepting connections, lets every
//! active `read` deliver the `exit` record (for a few seconds), then exits.
//!
//! # Process handling
//!
//! - fsyncs run on blocking threads, never on the async worker; pumps, the stdin
//!   logger and writer, and the reaper are plain threads.
//! - Output lines longer than `max_line_bytes` are cut and marked `truncated`.
//! - `timeout_ms` terminates the group like `terminate` and records
//!   `cause: "timeout"`.
//! - When the child exits, the rest of its process group is SIGKILLed before the
//!   exit record is written (processes that left the group are not tracked).
//! - SIGHUP is ignored (pi exits on HUP, so it is never forwarded). SIGTERM/SIGINT
//!   terminate the child with a 5 s grace and record `cause: "signal"`; a second
//!   one kills the group at once. SIGKILL leaves the transcript valid up to its
//!   last complete line.
//!
//! A login shell may print non-protocol bytes to stdout before the agent starts;
//! they are logged like any other line and drivers must tolerate them.
//!
//! PTYs (`kind: pty`) are reserved: they will need byte-chunk records, raw writes
//! and resize, added as a new protocol version.

pub mod cli;
mod client;
mod launch;
mod paths;
mod proto;
mod server;
mod spec;

pub use client::{Client, ClientError, DEFAULT_CALL_TIMEOUT, RecordStream};
pub use launch::{LaunchError, LaunchRequest, SUBCOMMAND, launch};
pub use paths::{PathError, Paths, SUN_PATH_MAX, read_info, validate_id};
pub use proto::{
    ErrorBody, ErrorCode, Hello, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, ReadyLine, Status,
    SupervisorInfo, VERSION,
};
pub use spec::{
    DEFAULT_MAX_LINE_BYTES, Exec, MAX_LINE_BYTES, SpawnSpec, SpecError, parse_env, read_env_file,
};
