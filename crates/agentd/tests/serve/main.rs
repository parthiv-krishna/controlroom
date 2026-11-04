//! End-to-end scenarios over the real sockets: `controlroom-agentd serve`, real supervisors,
//! `controlroom-fake-agent` speaking pi RPC (driver `pi`) and a fake controller carrying host
//! requests. See [`harness`] and [`controller`].

mod agent;
mod clients;
mod controller;
mod harness;
mod queue;
mod recovery;
mod remote;
mod requests;
mod sessions;
