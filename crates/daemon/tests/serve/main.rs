//! End-to-end scenarios over the real socket: `controlroomd serve`, real supervisors and
//! `controlroom-fake-agent` speaking pi RPC (driver `pi`). See [`harness`].

mod approvals;
mod clients;
mod harness;
mod queue;
mod recovery;
mod requests;
mod sessions;
