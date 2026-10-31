//! Records real `pi --mode rpc` transcripts against a local mock model and
//! sanitizes them into committed fixtures (`fixtures/pi-rpc/<version>/`).

pub mod env;
pub mod fixture;
pub mod mock_model;
pub mod rig;
pub mod sanitize;
pub mod scenario;
pub mod scenarios;
