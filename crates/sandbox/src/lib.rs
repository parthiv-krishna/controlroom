//! Sandboxing for controlroom sessions with nono 0.80 (spec §11).
//!
//! - [`policy`]: sandbox levels, standing grants and the effective-profile merge (§11.6);
//! - [`profile`]: absolute-path nono profile generation and validation (§11.2);
//! - [`pins`]: pinning mediated commands to exact store paths (§11.5b);
//! - [`agent_dir`]: the per-session pi agent dir with fake credentials (§11.1, §11.3);
//! - [`credential`]: the host-side key lookup behind `controlroomd credential` (§11.3);
//! - [`webhook`]: approval webhook URL, request and reply types (§11.4);
//! - [`workspace`]: the per-workspace sandbox of a host (§11, host side);
//! - [`launch`]: the `nono run` argv and environment (§11.5);
//! - [`probe`]: Landlock ABI and nono availability checks for `doctor`;
//! - [`tempdirs`]: cleanup of leaked `/tmp/nono-*` directories.

pub mod agent_dir;
pub mod credential;
mod fsutil;
pub mod launch;
pub mod layout;
pub mod names;
pub mod paths;
pub mod pins;
pub mod policy;
pub mod probe;
pub mod profile;
pub mod spec;
pub mod tempdirs;
pub mod webhook;
pub mod workspace;

pub use layout::SessionLayout;
pub use names::{Domain, Name, SessionId};
pub use paths::AbsPath;
pub use policy::{EffectivePolicy, SandboxLevel, ScopePolicy, effective_policy};
pub use profile::{NonoProfile, ProfileError, generate_profile, generate_workspace_profile};
pub use spec::SessionSandboxSpec;
pub use workspace::WorkspaceSandboxSpec;
