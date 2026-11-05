//! `sandbox.toml`: machine-local inputs of workspace sandboxes (spec §11, host side).
//!
//! The *policy* (level, standing grants, domains, mediated commands) comes from the agent
//! daemon per workspace (`workspace.open`); this file holds what only the execution host
//! knows. Every key is optional:
//!
//! ```toml
//! nono = "nono"                     # default: the nono controlroom installs with controlroomd;
//!                                   # else a program found by the login shell, or a path
//! read_only = ["/nix/store"]        # readable system paths
//! exec_dirs = ["/usr/bin", "/bin"]  # besides PATH, where sandboxed programs may run from
//! relaxed_reads = [{ path = "~/.gitconfig", kind = "file" }]  # level relaxed only
//! approval_timeout_secs = 600
//! nono_tmp = "/tmp"                 # nono's own TMPDIR; short, because nono binds sockets in it
//! ```
//!
//! **Executables.** With command mediation, nono 0.80 lets a sandbox run only the programs in
//! its `PATH` directories, in `exec_dirs`, and under its writable directories (the workspace
//! root included, so `./script.sh`, `node_modules/.bin/*` and `target/*` builds run), plus
//! their ELF and shebang interpreters. The defaults make `#!/usr/bin/env` and `#!/bin/sh`
//! scripts work on NixOS, where `/usr/bin/env` is not the coreutils on `PATH`. Directories
//! that don't exist are skipped; writable ones are refused.

use controlroom_sandbox::AbsPath;
use controlroom_sandbox::policy::{Access, PathGrant, PathKind};
use serde::Deserialize;

use super::{Loaded, load_toml};
use crate::dirs::{Dirs, expand_home};

/// The contents of `sandbox.toml`, paths unexpanded.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// nono: a program name resolved by the login shell, or an absolute (or `~/`) path;
    /// unset, the nono installed with this `controlroomd` (see [`crate::nono`]).
    pub nono: Option<String>,
    /// Readable system paths.
    pub read_only: Vec<String>,
    /// Directories, besides `PATH`'s, whose programs sandboxed processes may run.
    pub exec_dirs: Vec<String>,
    /// Read-only home config added at level relaxed.
    pub relaxed_reads: Vec<RelaxedRead>,
    /// How long nono waits for an approval before denying.
    pub approval_timeout_secs: u32,
    /// nono's TMPDIR.
    pub nono_tmp: String,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            nono: None,
            read_only: vec!["/nix/store".to_owned()],
            exec_dirs: vec!["/usr/bin".to_owned(), "/bin".to_owned()],
            relaxed_reads: Vec::new(),
            approval_timeout_secs: 600,
            nono_tmp: "/tmp".to_owned(),
        }
    }
}

/// A read-only path for level relaxed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaxedRead {
    /// The path (`~/` allowed).
    pub path: String,
    /// Directory (recursive) or file.
    pub kind: PathKind,
}

/// [`SandboxConfig`] with paths expanded and every value validated.
#[derive(Debug, Clone)]
pub struct SandboxSettings {
    /// How to find nono; `None` for the bundled one.
    pub nono: Option<String>,
    /// Readable system paths.
    pub read_only: Vec<AbsPath>,
    /// Directories, besides `PATH`'s, whose programs sandboxed processes may run (as
    /// configured; profiles skip missing ones).
    pub exec_dirs: Vec<AbsPath>,
    /// Read-only grants at level relaxed.
    pub relaxed_reads: Vec<PathGrant>,
    /// Approval timeout.
    pub approval_timeout_secs: u32,
    /// nono's TMPDIR.
    pub nono_tmp: AbsPath,
}

/// Why `sandbox.toml` can't be used.
#[derive(Debug, thiserror::Error)]
#[error("sandbox.toml: {0}")]
pub struct SettingsError(String);

impl SandboxConfig {
    /// Expands and validates the configuration.
    pub fn resolve(&self, home: &AbsPath) -> Result<SandboxSettings, SettingsError> {
        let path = |what: &str, value: &str| {
            expand_home(value, home).map_err(|e| SettingsError(format!("{what}: {e}")))
        };
        let paths = |what: &str, values: &[String]| {
            values
                .iter()
                .map(|p| path(what, p))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(SandboxSettings {
            nono: self.nono.clone(),
            read_only: paths("read_only", &self.read_only)?,
            exec_dirs: paths("exec_dirs", &self.exec_dirs)?,
            relaxed_reads: self
                .relaxed_reads
                .iter()
                .map(|r| {
                    Ok(PathGrant {
                        path: path("relaxed_reads", &r.path)?,
                        kind: r.kind,
                        access: Access::Read,
                    })
                })
                .collect::<Result<_, SettingsError>>()?,
            approval_timeout_secs: self.approval_timeout_secs,
            nono_tmp: short_nono_tmp(path("nono_tmp", &self.nono_tmp)?)?,
        })
    }
}

/// Longest usable `nono_tmp`: nono 0.80 binds
/// `<TMPDIR>/nono-tool-sandbox-<pid>-<19 digits>-<16 hex>/supervisor.sock` (78 bytes after the
/// directory) and fails beyond the 107-byte `sun_path` limit.
pub const NONO_TMP_MAX: usize = 28;

fn short_nono_tmp(dir: AbsPath) -> Result<AbsPath, SettingsError> {
    if dir.as_str().len() > NONO_TMP_MAX {
        return Err(SettingsError(format!(
            "nono_tmp {dir:?} is longer than {NONO_TMP_MAX} bytes; nono binds unix sockets in it"
        )));
    }
    Ok(dir)
}

/// Loads `sandbox.toml`.
pub fn load(dirs: &Dirs) -> Loaded<SandboxConfig> {
    load_toml(&dirs.sandbox_toml())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> AbsPath {
        AbsPath::new("/home/alice").unwrap()
    }

    #[test]
    fn defaults() {
        let config: SandboxConfig = toml::from_str("").unwrap();
        assert_eq!(config, SandboxConfig::default());
        let s = config.resolve(&home()).unwrap();
        assert_eq!(s.nono, None);
        assert_eq!(s.read_only, [AbsPath::new("/nix/store").unwrap()]);
        assert_eq!(
            s.exec_dirs,
            [
                AbsPath::new("/usr/bin").unwrap(),
                AbsPath::new("/bin").unwrap()
            ]
        );
        assert_eq!(s.nono_tmp.as_str(), "/tmp");
        assert_eq!(s.approval_timeout_secs, 600);
    }

    #[test]
    fn full_file() {
        let config: SandboxConfig = toml::from_str(
            r#"
nono = "/nix/store/aaaa-nono-0.80.0/bin/nono"
read_only = ["/nix/store", "~/datasets"]
relaxed_reads = [{ path = "~/.gitconfig", kind = "file" }]
approval_timeout_secs = 30
"#,
        )
        .unwrap();
        let s = config.resolve(&home()).unwrap();
        assert_eq!(
            s.nono.as_deref(),
            Some("/nix/store/aaaa-nono-0.80.0/bin/nono")
        );
        assert_eq!(s.read_only[1].as_str(), "/home/alice/datasets");
        assert_eq!(s.relaxed_reads[0].path.as_str(), "/home/alice/.gitconfig");
        assert_eq!(s.relaxed_reads[0].access, Access::Read);
        assert_eq!(s.approval_timeout_secs, 30);
    }

    #[test]
    fn invalid_values() {
        let bad = |toml: &str| {
            let config: SandboxConfig = toml::from_str(toml).unwrap();
            config.resolve(&home()).unwrap_err()
        };
        bad("read_only = [\"relative\"]");
        bad("nono_tmp = \"tmp\"");
        bad("nono_tmp = \"/var/tmp/a-rather-long-directory-name\"");
        // Typos (and the agent keys that moved to the server) are reported.
        assert!(toml::from_str::<SandboxConfig>("unknown = 1").is_err());
        assert!(toml::from_str::<SandboxConfig>("keep = []").is_err());
    }
}
