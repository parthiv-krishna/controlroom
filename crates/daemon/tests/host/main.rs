//! End-to-end tests of the host role: a real `controlroomd host serve`, real PTY shells, job
//! supervisors and the fs worker, at sandbox level off (no nono needed; the real-nono checks are
//! in `nono.rs`, ignored by default).

mod api;
mod files;
mod harness;
mod jobs;
mod journal;
mod shells;
