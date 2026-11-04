//! controlroomd: the per-user daemon on an execution machine, as a library behind the
//! `controlroomd` binary's subcommands. Sessions (and pi) moved to `controlroom-agentd` on the
//! server; this daemon becomes the host role (workspaces, shells, tools, jobs).
//!
//! - [`dirs`], [`config`], [`version`], [`shell`]: paths, configuration and host helpers;
//! - [`instance`], [`serve`], [`connect`]: the single daemon instance, the API it serves, and
//!   the stdio bridge the controller runs over SSH;
//! - [`host`]: the host role (`controlroomd host serve|connect`, `workspace-exec`, `pty`): the
//!   host API with workspaces, shells, the tool journal, jobs and approvals;
//! - [`doctor`]: environment checks;
//! - [`supervise`]: the supervisor entry point.

pub mod config;
pub mod connect;
pub mod dirs;
pub mod doctor;
pub mod host;
pub mod instance;
pub mod serve;
pub mod shell;
pub mod supervise;
pub mod version;
