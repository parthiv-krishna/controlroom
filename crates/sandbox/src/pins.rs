//! Pinning mediated commands to exact executables (spec §11.5b).
//!
//! nono binds a mediated command name to one canonical file. On NixOS the same command often
//! exists as several store copies reachable through different `PATH` directories (system
//! profile, user profile, nix-shell), and a copy that is not the pinned one runs *without*
//! mediation when called by absolute path. [`resolve_pin`] therefore returns the first `PATH`
//! match as the pin and every other distinct copy on `PATH` as a shadow; the profile denies
//! shadows outright.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::paths::AbsPath;

/// The resolved executables of one command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPin {
    /// Canonical path of the first match on `PATH`.
    pub executable: AbsPath,
    /// Canonical paths of other, different copies further down `PATH`.
    pub shadows: Vec<AbsPath>,
}

/// Resolves `name` against a `PATH` value (`dir:dir:...`), following symlinks to the real
/// file. Returns `None` if no executable regular file is found. Relative and non-UTF-8 `PATH`
/// entries are ignored, as nono only trusts absolute directories.
pub fn resolve_pin(name: &str, path_var: &str) -> Option<CommandPin> {
    let mut found: Vec<AbsPath> = Vec::new();
    for dir in path_var.split(':').filter(|d| d.starts_with('/')) {
        let candidate = Path::new(dir).join(name);
        let Ok(real) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&real) else {
            continue;
        };
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            continue;
        }
        let Ok(real) = AbsPath::from_path(&real) else {
            continue;
        };
        if !found.contains(&real) {
            found.push(real);
        }
    }
    let mut found = found.into_iter();
    Some(CommandPin {
        executable: found.next()?,
        shadows: found.collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn exe(path: &Path) {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn first_match_is_pinned_and_other_copies_are_shadows() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        for d in [
            "store-a/bin",
            "store-b/bin",
            "profile/bin",
            "other/bin",
            "empty",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        exe(&root.join("store-a/bin/git"));
        exe(&root.join("store-b/bin/git"));
        symlink(root.join("store-a/bin/git"), root.join("profile/bin/git")).unwrap();
        symlink(root.join("store-b/bin/git"), root.join("other/bin/git")).unwrap();
        // Not executable: ignored.
        std::fs::write(root.join("empty/git"), "").unwrap();

        let path_var = format!(
            "relative/bin:{e}:{p}:{a}:{o}",
            e = root.join("empty").display(),
            p = root.join("profile/bin").display(),
            a = root.join("store-a/bin").display(),
            o = root.join("other/bin").display(),
        );
        let pin = resolve_pin("git", &path_var).unwrap();
        let canon =
            |p: &str| AbsPath::from_path(&std::fs::canonicalize(root.join(p)).unwrap()).unwrap();
        assert_eq!(pin.executable, canon("store-a/bin/git"));
        assert_eq!(pin.shadows, vec![canon("store-b/bin/git")]);
        assert!(resolve_pin("ssh", &path_var).is_none());
    }
}
