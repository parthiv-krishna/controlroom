//! End-to-end harness for the remote-tools model (test-only crate).
//!
//! It wires the real pieces together: the controller (in process, speaking the UI API to
//! one browser tab), the real `controlroom-agentd` binary connected through its socket, and two
//! real host daemons (`controlroomd`, installed by the controller from the bundled daemons)
//! on "devbox" and "buildbox", each a separate `$HOME` behind client-core's in-process SSH
//! server and a [`Relay`](controlroom_controller::test_support::Relay) that can cut the link.
//! The agent is `controlroom-fake-agent` (pi RPC), whose scripted tool calls use the agent API
//! like `@controlroom/pi` (see [`script`]).
//!
//! - [`world::World`] starts and owns everything; dropping it kills every process it started,
//!   pass or fail.
//! - [`session`] creates, prompts and watches sessions through the UI API.
//!
//! **Binaries.** `controlroom-agentd`, `controlroomd` and `controlroom-fake-agent` are taken
//! from `target/<profile>/` (built by `cargo test --workspace` and crane's `cargoTest`; a
//! missing one is built once). After changing one of those crates, build it before running
//! only this crate's tests (`cargo build --workspace --bins`).
//!
//! **No sleeps.** Remote commands wait on FIFOs the tests open, agents on gates the tests
//! release; waits poll for a condition with a deadline that only turns a hang into a failure.

#![allow(clippy::unwrap_used, clippy::missing_panics_doc)]

pub mod agentd;
pub mod held;
pub mod procs;
pub mod script;
pub mod session;
pub mod world;
