//! The session runtime (spec §6.5, §6.6): the session registry, queue, requests, reconcile
//! loop, recovery and the materialized timeline, behind a [`Runtime`] handle the daemon serves
//! the daemon API with. Library only: no sockets; all IO goes through the [`ports`] traits.
//!
//! **Invariants** (spec §3):
//!
//! - *The transcript is the truth.* State and timeline are folds over the transcript (by the
//!   session's [`controlroom_drivers::Driver`]); SQLite holds what the transcript can't say
//!   (names, cursors, the queue, decisions, op results) plus the materialized timeline.
//! - *Exactly once.* Every mutating command is deduplicated by op id in the same transaction
//!   as its effect. Lines for the agent are staged during the transaction and handed to the
//!   supervisor only after the commit; a failed transaction discards them. Every line carries
//!   a [`controlroom_drivers::LineTag`], so after a crash the transcript says which prompts
//!   and answers the agent was given: prompts are never resent, decisions never reused.
//! - *No writes while in doubt.* After attaching to a supervisor (or a failed write) nothing
//!   is written, answers included, until a supervisor barrier has confirmed which earlier
//!   writes are logged and the transcript has been folded that far.
//! - *One reconcile loop per session* decides what to write next (`reconcile`).
//! - *Disconnecting does nothing*: nothing here knows about clients.
//!
//! **Modules:** `core` (state and transactions), `ingest` (folding records), `reconcile`,
//! `commands`, `lifecycle` and `io` (agent processes and supervisor attachments),
//! `policy` (the tick: idle-stop, retention, pruning), `timeline` (materialization), `store`
//! (SQLite with versioned migrations), `preview`, `history` (start-fresh export), `raised`
//! (requests the daemon raises itself, such as sandbox approvals).
//!
//! Agent runtime primitives (spec §8): `inject` (the single delivery path into the agent's
//! next turn), `questions` (non-blocking `ask_user`), `todos` (the shared list), `auto`
//! (auto-continue), `triggers` (run triggers from the runtime's own knowledge), `items`
//! (daemon-made timeline items) and `agent` (the agent API the daemon's agent socket serves).
//!
//! **Not yet here** (later phases add them as new modules and migrations): jobs, children, the
//! mailbox and contact policy.

mod agent;
mod auto;
mod commands;
mod core;
mod errors;
mod history;
mod ingest;
mod inject;
mod io;
mod items;
mod lifecycle;
mod model;
mod policy;
pub mod ports;
mod preview;
mod questions;
mod raised;
mod reconcile;
mod runtime;
mod session;
mod store;
mod timeline;
mod todos;
mod triggers;

#[cfg(test)]
mod tests;

pub use raised::{RaiseRequest, RaisedRequest};
pub use runtime::{MachineSubscription, OpenError, Runtime, RuntimeConfig, SessionSubscription};
pub use session::SessionUpdate;
