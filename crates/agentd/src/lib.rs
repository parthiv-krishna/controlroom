//! controlroom-agentd: the agent daemon on the server (design §1), as a library behind the
//! `controlroom-agentd` binary's subcommands. It runs the session runtime behind a local unix
//! socket (the agent daemon API, served to the controller), launches pi under nono locally
//! through detached supervisors, serves each session's agent socket (@controlroom/pi with
//! remote tools), and reaches hosts only through the controller's host bridge.
//!
//! - [`dirs`], [`config`], [`version`], [`shell`]: paths, configuration and helpers;
//! - [`instance`], [`serve`]: the single instance and the APIs it serves;
//! - [`launch`]: pi launch glue (session dirs, nono profile, supervisor spawn spec);
//! - [`login`], [`credential`]: model credentials (OAuth sign-in, nono's credential helper);
//! - [`doctor`], [`ctl`]: environment checks and the debug client;
//! - [`supervise`]: the supervisor entry point.

pub mod config;
pub mod credential;
pub mod ctl;
pub mod dirs;
pub mod doctor;
pub mod instance;
pub mod launch;
pub mod login;
pub mod serve;
pub mod shell;
pub mod supervise;
pub mod version;
