//! The agent daemon version, reported by `--version`, `hello` and `doctor`.

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
