//! The fake driver: a deterministic in-process agent for runtime unit tests (spec §6.4).
//!
//! Two halves speak a small JSON line protocol of their own (see [`wire`]); it does not
//! resemble pi on purpose, so runtime tests can't come to depend on pi's quirks:
//!
//! - [`FakeDriver`] is the [`crate::Driver`]: it encodes intents into stdin lines and folds the
//!   transcript into timeline ops and events, like a real driver;
//! - [`FakeAgent`] is the scripted agent behind the stdin pipe. A [`FakeScript`] picks a turn
//!   per prompt; turns are [`FakeStep`]s, and [`FakeStep::Gate`] holds the agent until the test
//!   calls [`FakeAgent::release`], so tests are ordered by gates rather than sleeps.
//!
//! Neither half does IO: the test's in-memory supervisor logs the lines and feeds the agent.

mod agent;
mod fold;
pub mod wire;

pub use agent::{AgentOutput, FakeAgent, FakeScript, FakeStep, FakeTurn};
pub use fold::FakeDriver;

#[cfg(test)]
mod tests;
