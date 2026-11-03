//! Wire types for controlroom's JSON-RPC APIs, and the source of the generated TypeScript in
//! `web/src/lib/protocol/` (daemon and UI APIs) and `extensions/pi/src/protocol/` (agent API).
//! See `docs/protocol.md` for the overview.
//!
//! Areas:
//! - [`ids`]: ULID and string ids, `RemotePath`, `UnixMillis`, `Seq`, `Rev`;
//! - [`jsonrpc`]: envelopes, tolerant parsing, the [`Method`] / [`NotificationMethod`] traits;
//! - [`error`]: typed error kinds;
//! - [`version`]: protocol versions and `hello` capabilities;
//! - [`session`]: the agent-neutral session model (summary, detail, requests, todos, jobs);
//! - [`timeline`]: timeline items and ops;
//! - [`daemon`]: controller ↔ `controlroomd`;
//! - [`agent`]: the agent's extension (`@controlroom/pi`) ↔ `controlroomd`, per session;
//! - [`ui`]: browser ↔ controller;
//! - [`typescript`]: TypeScript generation.
//!
//! **Conventions:**
//! - Field and enum names are `snake_case`; tagged unions use a `kind` tag (`op` for timeline
//!   ops, `outcome` for results, `state` for connectivity, `method` for calls).
//! - Every enum a peer receives has an `unknown` fallback, so a newer peer's additions decode
//!   instead of failing the message. Params-only enums have none: an unknown value there is an
//!   `invalid_params` error for that call.
//! - Unknown fields are ignored.
//! - Optional *params* fields may be omitted; optional *result* fields are present as `null`.
//! - No agent-specific payloads: drivers translate agent formats into these types.

#![warn(missing_docs)]

#[macro_use]
mod macros;

pub mod agent;
pub mod daemon;
pub mod error;
pub mod ids;
pub mod jsonrpc;
pub mod session;
pub mod timeline;
pub mod typescript;
pub mod ui;
pub mod version;

pub use error::{ErrorData, Resource, RpcError};
pub use ids::{RemotePath, Rev, Seq, UnixMillis};
pub use jsonrpc::{
    Ack, Message, MessageError, Method, NoParams, NotificationMethod, RpcId, RpcNotification,
    RpcOutcome, RpcRequest, RpcResponse,
};
pub use version::{
    AGENT_PROTOCOL_VERSION, Capability, DAEMON_PROTOCOL_VERSION, ProtocolVersion,
    UI_PROTOCOL_VERSION,
};
