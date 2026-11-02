//! controlroomd: the per-user daemon on an execution machine, as a library behind the
//! `controlroomd` binary's subcommands.
//!
//! - [`dirs`], [`config`], [`version`], [`shell`]: paths, configuration and host helpers;
//! - [`instance`], [`serve`], [`connect`]: the single daemon instance, the daemon API it serves
//!   (the session runtime behind its host and agents ports), and the stdio bridge the
//!   controller runs over SSH;
//! - [`launch`]: agent launch glue (session dirs, nono profile, supervisor spawn spec);
//! - [`doctor`]: environment checks;
//! - [`ctl`]: the debug client and its timeline renderer;
//! - [`credential`], [`supervise`]: nono's credential helper and the supervisor entry point.

pub mod config;
pub mod connect;
pub mod credential;
pub mod ctl;
pub mod dirs;
pub mod doctor;
pub mod instance;
pub mod launch;
pub mod serve;
pub mod shell;
pub mod supervise;
pub mod version;
