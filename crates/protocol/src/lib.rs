//! Wire types for controlroom's JSON-RPC APIs, and the source of the generated TypeScript in
//! `web/src/lib/protocol/` (UI API) and `extensions/pi/src/protocol/` (agent API). See `docs/protocol.md` for the overview.
//!
//! pi runs only on the server, in the agent daemon (`controlroom-agentd`); execution hosts run
//! `controlroomd` in the host role, which only executes tools in workspaces. The APIs:
//!
//! - [`agentd`]: controller ↔ `controlroom-agentd` (sessions, and the `hostops` bridge through
//!   which the agent daemon reaches hosts);
//! - [`host`]: controller ↔ `controlroomd` host role (workspaces, shells, tool calls with a
//!   journal, jobs, approvals);
//! - [`agent`]: the agent's extension (`@controlroom/pi`) ↔ the agent daemon, per session;
//! - [`ui`]: browser ↔ controller;
//! - [`terminal`]: browser ↔ controller terminal streams (binary WebSockets).
//!
//! Areas:
//! - [`ids`]: ULID and string ids, `RemotePath`, `UnixMillis`, `Seq`, `Rev`;
//! - [`jsonrpc`]: envelopes, tolerant parsing, the [`Method`] / [`NotificationMethod`] traits;
//! - [`error`]: typed error kinds;
//! - [`version`]: protocol versions and `hello` capabilities;
//! - [`session`]: the agent-neutral session model (summary, detail, requests, todos, jobs,
//!   workspaces, shells);
//! - [`timeline`]: timeline items and ops;
//! - [`typescript`]: TypeScript generation.
//!
//! **Conventions:**
//! - Field and enum names are `snake_case`; tagged unions use a `kind` tag (`op` for timeline
//!   ops and tool ops, `outcome` for results, `state` for connectivity and shells, `method` for
//!   calls).
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
pub mod agentd;
pub mod error;
pub mod host;
pub mod ids;
pub mod jsonrpc;
pub mod session;
pub mod terminal;
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
    AGENT_PROTOCOL_VERSION, AGENTD_PROTOCOL_VERSION, Capability, HOST_PROTOCOL_VERSION,
    ProtocolVersion, UI_PROTOCOL_VERSION,
};
