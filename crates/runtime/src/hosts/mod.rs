//! Hosts: where a session's tools run (design §1–§3, `docs/protocol.md` §5, §6, §8).
//!
//! pi runs on the server; a session reaches files only through its **workspaces** on execution
//! hosts. The runtime never talks to a host itself: it queues host requests on the **bridge**,
//! which the controller pulls (`hostops.subscribe`), sends to the right host daemon, and answers
//! (`hostops.respond`). The controller also forwards every host notification
//! (`hostops.notify`) and reports its machines (`hostops.machines`).
//!
//! - `bridge`: pending host requests and their subscriber;
//! - `machines`: names, availability, epochs and grace periods, and what a report changed;
//! - `records`: what sessions persist about hosts (workspaces, shells, late calls);
//! - `workspaces`: creating sessions on workspaces, adding and removing them, `session.info`;
//! - `shells`: the session's named shells;
//! - `terminals`: the user's own terminals (private shells, never the agent's);
//! - `calls`: the agent's `host.call` with link-loss pauses, grace and journal acks;
//! - `late`: results of calls that outlived their grace period;
//! - `approvals`: host sandbox approvals and waits for terminal input as session requests;
//! - `sync`: renewing subscriptions after every new host connection, and notifications.

mod approvals;
mod bridge;
mod calls;
mod late;
mod machines;
mod records;
mod shells;
mod sync;
mod terminals;
mod workspaces;

pub(crate) use bridge::Bridge;
pub use bridge::BridgeEvent;
pub(crate) use calls::{DoneCall, OpenCall, parse, session_request};
pub(crate) use late::attach;
pub(crate) use machines::Machines;
pub(crate) use records::{LateCall, ShellRecord, WorkspaceRecord};
pub(crate) use shells::open_shells;
pub(crate) use sync::{HostSync, connected, refresh_session};
pub(crate) use workspaces::{close_request, default_sandbox};
