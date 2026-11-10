//! Which nono the host sandboxes workspaces with (spec §11.5).
//!
//! By default the one the controller installed with this `controlroomd`: `versions/<ver>/nono`
//! next to the binary, a link to `nono/<nono version>-<sha>/nono` (see client-core's bootstrap).
//! Each daemon version keeps the nono it was installed with, so a rollback restores both. A `nono`
//! key in `sandbox.toml` overrides it with a program found by the login shell or a path.
//!
//! The result is canonical (the versioned binary, not the link), so the process nono runs as
//! (and copies into its shim directories) is the immutable file.

use std::fmt;

use controlroom_sandbox::AbsPath;

use crate::shell::{find_program, login_shell};

/// Where the nono in use comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonoOrigin {
    /// Installed by the controller next to this `controlroomd`.
    Bundled,
    /// Named by `nono = …` in `sandbox.toml`.
    Configured,
}

impl fmt::Display for NonoOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NonoOrigin::Bundled => "bundled",
            NonoOrigin::Configured => "configured in sandbox.toml",
        })
    }
}

/// A located nono binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nono {
    /// The canonical path of the binary.
    pub path: AbsPath,
    /// Where it comes from.
    pub origin: NonoOrigin,
}

/// Why no nono could be located.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct NonoMissing {
    /// Which nono was looked for.
    pub origin: NonoOrigin,
    /// Why it wasn't found.
    pub detail: String,
}

/// The bundled nono's (unresolved) path for the daemon at `exe`: `nono` in its directory.
pub fn bundled_path(exe: &AbsPath) -> AbsPath {
    exe.parent().unwrap_or_else(|| exe.clone()).join("nono")
}

/// Locates nono: `setting` (the `nono` key of `sandbox.toml`) if set, else the bundled one next
/// to `exe` (this `controlroomd`, canonical).
pub fn locate(setting: Option<&str>, exe: &AbsPath, home: &AbsPath) -> Result<Nono, NonoMissing> {
    let (origin, found) = match setting {
        Some(setting) => (
            NonoOrigin::Configured,
            find_program(&login_shell(), setting, home),
        ),
        None => (NonoOrigin::Bundled, bundled(exe)),
    };
    found
        .map(|path| Nono { path, origin })
        .map_err(|detail| NonoMissing { origin, detail })
}

fn bundled(exe: &AbsPath) -> Result<AbsPath, String> {
    let link = bundled_path(exe);
    let real = std::fs::canonicalize(link.as_path()).map_err(|e| {
        format!(
            "the bundled nono {link} is missing ({e}); controlroom installs it with controlroomd"
        )
    })?;
    AbsPath::from_path(&real).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_nono_is_the_link_next_to_the_daemon_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let base = AbsPath::from_path(&tmp.path().canonicalize().unwrap()).unwrap();
        let exe = base.join("versions/0.2.0/controlroomd");
        std::fs::create_dir_all(exe.parent().unwrap().as_path()).unwrap();
        std::fs::write(exe.as_path(), "").unwrap();
        let missing = locate(None, &exe, &base).unwrap_err();
        assert_eq!(missing.origin, NonoOrigin::Bundled);
        assert!(
            missing.detail.contains("versions/0.2.0/nono is missing"),
            "{missing}"
        );

        let real = base.join("nono/0.80.0/nono");
        std::fs::create_dir_all(real.parent().unwrap().as_path()).unwrap();
        std::fs::write(real.as_path(), "").unwrap();
        std::os::unix::fs::symlink("../../nono/0.80.0/nono", bundled_path(&exe).as_path()).unwrap();
        assert_eq!(
            locate(None, &exe, &base).unwrap(),
            Nono {
                path: real.clone(),
                origin: NonoOrigin::Bundled
            }
        );

        // `sandbox.toml` overrides it.
        let other = base.join("bin/nono");
        std::fs::create_dir_all(other.parent().unwrap().as_path()).unwrap();
        std::fs::write(other.as_path(), "").unwrap();
        assert_eq!(
            locate(Some(other.as_str()), &exe, &base).unwrap(),
            Nono {
                path: other,
                origin: NonoOrigin::Configured
            }
        );
        let missing = locate(Some("/nonexistent/nono"), &exe, &base).unwrap_err();
        assert_eq!(missing.origin, NonoOrigin::Configured);
    }
}
