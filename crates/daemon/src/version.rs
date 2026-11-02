//! The daemon version, reported by `--version` (first line `controlroomd <semver>`, which the
//! controller's bootstrap parses), `hello` and `doctor`.

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The first line of `controlroomd --version`.
pub fn version_line() -> String {
    format!("controlroomd {VERSION}")
}
