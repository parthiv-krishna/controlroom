//! controlroomd: the per-user daemon on an execution machine, as a library behind the
//! `controlroomd` binary's subcommands: the host daemon of an execution machine (workspaces,
//! shells, tool calls, jobs). Sessions and pi run on the server (`controlroom-agentd`).
//!
//! - [`dirs`], [`config`], [`version`], [`shell`], [`nono`]: paths, configuration, host helpers
//!   and which nono sandboxes workspaces;
//! - [`instance`], [`serve`], [`connect`]: the single daemon instance, starting it, and the
//!   stdio bridge the controller runs over SSH;
//! - [`host`]: the host API (workspaces, shells, the tool journal, jobs, approvals), the fs
//!   worker (`workspace-exec`) and terminal attach (`pty`);
//! - [`doctor`]: environment checks;
//! - [`supervise`]: the supervisor entry point.

pub mod config;
pub mod connect;
pub mod dirs;
pub mod doctor;
pub mod host;
pub mod instance;
pub mod nono;
pub mod serve;
pub mod shell;
pub mod supervise;
pub mod version;
