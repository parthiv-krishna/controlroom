//! What nono 0.80 lets a grant cover, besides controlroom's own rules (spec §11.6).
//!
//! - **Built-in denials.** nono's implicit `default` profile includes required `deny_*` groups
//!   ([`DENIED_IN_HOME`]: credentials, keyrings, browser data, shell history and shell
//!   configuration). On Linux they act on grants only:
//!   - a grant **equal** to a denied path takes effect only with `filesystem.bypass_protection`
//!     (without it nono silently drops a file grant, so the path keeps raising approvals); the
//!     profile generator adds it;
//!   - a **directory** grant that **contains** a denied path, whether that exists or not, makes
//!     nono refuse to start (Landlock can't deny beneath an allowed directory), and
//!     `bypass_protection` can't lift a denial of a path that doesn't exist: such a grant is
//!     never usable (`~`, `~/.config`, `~/.local/share`, `/home`, `/`);
//!   - a grant **inside** a denied directory (`~/.ssh/config`) works as it is.
//! - **nono's state** (`~/.nono`): nono refuses a grant inside it or containing it.
//! - **Other processes' `/proc` entries** (`/proc/<pid>/…`): nono's supervisor only opens a
//!   process's own entries, whatever the profile or the approval says, so neither a standing
//!   nor a runtime grant can work. (Landlock also scopes ptrace, so `/proc/1/root` and the like
//!   fail with `EACCES` before nono is asked.)
//!
//! Hosts check grants against their home ([`limit`]); the controller, which doesn't know a
//! host's home, checks stored paths with `/home/<user>` and `/root` standing for homes
//! ([`limit_of_stored`]).

use std::fmt;

use crate::paths::AbsPath;

/// Paths under the home that nono 0.80 always denies on Linux (its required `deny_credentials`,
/// `deny_keychains_linux`, `deny_browser_data_linux`, `deny_shell_history` and
/// `deny_shell_configs` groups in `nono-cli/data/policy.json`). Re-check on every nono bump.
pub const DENIED_IN_HOME: [&str; 48] = [
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".gcloud",
    ".kube",
    ".docker",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".bunfig.toml",
    ".config/bun/bunfig.toml",
    ".vault-token",
    ".credentials",
    ".secrets",
    ".keys",
    ".pki",
    ".terraform.d",
    ".config/op",
    ".password-store",
    ".1password",
    ".op",
    ".local/share/keyrings",
    ".config/google-chrome",
    ".config/chromium",
    ".mozilla/firefox",
    ".config/microsoft-edge",
    ".config/BraveSoftware",
    ".config/vivaldi",
    ".config/opera",
    ".bash_history",
    ".zsh_history",
    ".history",
    ".python_history",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".zlogout",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".config/fish",
    ".env",
    ".envrc",
];

/// nono's own state under the home, which no grant may overlap.
pub const NONO_STATE_IN_HOME: &str = ".nono";

/// Why nono can't hold a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limit {
    /// One process's `/proc` entry.
    ProcessProcfs,
    /// It overlaps nono's own state (`~/.nono`).
    NonoState,
    /// A directory containing this path, which nono always denies.
    ContainsDenied(String),
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Limit::ProcessProcfs => f.write_str(
                "it is one process's /proc entry: it changes with every process, and nono never \
                 opens another process's entries",
            ),
            Limit::NonoState => f.write_str("it overlaps nono's own state (~/.nono)"),
            Limit::ContainsDenied(path) => write!(
                f,
                "it contains {path}, which nono always protects, so nono would refuse to start"
            ),
        }
    }
}

/// The paths nono denies on a host whose home is `home`, with the targets of those that are
/// symlinks (nono denies both forms).
pub fn denied(home: &AbsPath) -> Vec<AbsPath> {
    let mut out = Vec::with_capacity(DENIED_IN_HOME.len());
    for rel in DENIED_IN_HOME {
        let path = home.join(rel);
        let target = std::fs::canonicalize(path.as_path())
            .ok()
            .and_then(|t| AbsPath::from_path(&t).ok())
            .filter(|t| *t != path);
        out.push(path);
        out.extend(target);
    }
    out
}

/// Whether `path` is one process's `/proc` entry: `/proc/<pid>` or anything under it.
pub fn is_process_procfs(path: &AbsPath) -> bool {
    path.as_str()
        .strip_prefix("/proc/")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// Why nono can't hold a grant of `path` on a host whose home is `home`, if it can't. `file` is
/// true for an existing non-directory (it contains nothing).
pub fn limit(path: &AbsPath, file: bool, home: &AbsPath) -> Option<Limit> {
    if is_process_procfs(path) {
        return Some(Limit::ProcessProcfs);
    }
    if home.contains(path) {
        let rest = path.as_str()[home.as_str().len()..].trim_start_matches('/');
        return in_home(rest, file, |rel| home.join(rel).to_string());
    }
    // An ancestor of the home (`/`, `/home`) contains all of it.
    (!file && path.contains(home))
        .then(|| Limit::ContainsDenied(home.join(DENIED_IN_HOME[0]).to_string()))
}

/// Why nono can't hold a grant of `value`, a path as stored in a profile (`~`, `~/…` or
/// absolute, normalized), on any host. Each host's home is unknown here: `~`, `/home/<user>`
/// and `/root` count as homes, and a stored path is taken for a directory.
pub fn limit_of_stored(value: &str) -> Option<Limit> {
    if value == "~" {
        return in_home("", false, |rel| format!("~/{rel}"));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return in_home(rest, false, |rel| format!("~/{rel}"));
    }
    let path = AbsPath::new(value).ok()?;
    if is_process_procfs(&path) {
        return Some(Limit::ProcessProcfs);
    }
    let parts: Vec<&str> = path.as_str().split('/').filter(|p| !p.is_empty()).collect();
    let (home, rest) = match parts.as_slice() {
        // `/` and `/home` contain every home.
        [] | ["home"] => return Some(Limit::ContainsDenied(format!("~/{}", DENIED_IN_HOME[0]))),
        ["home", user, rest @ ..] => (format!("/home/{user}"), rest),
        ["root", rest @ ..] => ("/root".to_owned(), rest),
        _ => return None,
    };
    in_home(&rest.join("/"), false, |rel| format!("{home}/{rel}"))
}

/// The limit of a grant of `rest` under a home (`""` for the home itself); `show` renders a
/// home-relative path for the message.
fn in_home(rest: &str, file: bool, show: impl Fn(&str) -> String) -> Option<Limit> {
    let under = |dir: &str, path: &str| dir.is_empty() || path.starts_with(&format!("{dir}/"));
    if rest == NONO_STATE_IN_HOME || under(NONO_STATE_IN_HOME, rest) {
        return Some(Limit::NonoState);
    }
    if file {
        return None;
    }
    if let Some(denied) = DENIED_IN_HOME.iter().find(|d| under(rest, d)) {
        return Some(Limit::ContainsDenied(show(denied)));
    }
    under(rest, NONO_STATE_IN_HOME).then_some(Limit::NonoState)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> AbsPath {
        AbsPath::new(s).unwrap()
    }

    #[test]
    fn process_procfs_entries_are_per_pid() {
        for yes in ["/proc/1", "/proc/1/root", "/proc/42/task/43/environ"] {
            assert!(is_process_procfs(&p(yes)), "{yes}");
        }
        for no in [
            "/proc",
            "/proc/self/status",
            "/proc/sys/vm",
            "/proc/1x",
            "/srv/proc/1",
        ] {
            assert!(!is_process_procfs(&p(no)), "{no}");
        }
    }

    #[test]
    fn host_grants_equal_to_or_inside_a_denied_path_are_fine() {
        let home = p("/home/alice");
        for ok in [
            "/home/alice/.bash_history",
            "/home/alice/.ssh",
            "/home/alice/.ssh/config",
            "/home/alice/.config/git",
            "/home/alice/.cache/nix",
            "/srv/data",
        ] {
            assert_eq!(limit(&p(ok), false, &home), None, "{ok}");
        }
    }

    #[test]
    fn host_grants_containing_denied_paths_or_nono_state_are_refused() {
        let home = p("/home/alice");
        let contains = |s: &str| Some(Limit::ContainsDenied(s.into()));
        assert_eq!(
            limit(&p("/home/alice/.config"), false, &home),
            contains("/home/alice/.config/gcloud")
        );
        assert_eq!(
            limit(&p("/home/alice/.local/share"), false, &home),
            contains("/home/alice/.local/share/keyrings")
        );
        assert_eq!(
            limit(&p("/home/alice"), false, &home),
            contains("/home/alice/.ssh")
        );
        assert_eq!(limit(&p("/"), false, &home), contains("/home/alice/.ssh"));
        assert_eq!(
            limit(&p("/home"), false, &home),
            contains("/home/alice/.ssh")
        );
        // A file contains nothing.
        assert_eq!(limit(&p("/home/alice/.config"), true, &home), None);
        assert_eq!(
            limit(&p("/home/alice/.nono/x"), true, &home),
            Some(Limit::NonoState)
        );
        assert_eq!(
            limit(&p("/proc/1/root"), false, &home),
            Some(Limit::ProcessProcfs)
        );
    }

    #[test]
    fn stored_paths_count_home_and_root_as_homes() {
        let contains = |s: &str| Some(Limit::ContainsDenied(s.into()));
        assert_eq!(limit_of_stored("~"), contains("~/.ssh"));
        assert_eq!(limit_of_stored("~/.config"), contains("~/.config/gcloud"));
        assert_eq!(
            limit_of_stored("~/.mozilla"),
            contains("~/.mozilla/firefox")
        );
        assert_eq!(limit_of_stored("/"), contains("~/.ssh"));
        assert_eq!(limit_of_stored("/home"), contains("~/.ssh"));
        assert_eq!(limit_of_stored("/home/alice"), contains("/home/alice/.ssh"));
        assert_eq!(
            limit_of_stored("/root/.config"),
            contains("/root/.config/gcloud")
        );
        assert_eq!(limit_of_stored("~/.nono"), Some(Limit::NonoState));
        assert_eq!(limit_of_stored("/proc/1/root"), Some(Limit::ProcessProcfs));
        for ok in [
            "~/.bash_history",
            "~/.ssh/config",
            "~/.cache/nix",
            "/home/alice/.bash_history",
            "/proc",
            "/srv",
            "/run/dbus/system_bus_socket",
        ] {
            assert_eq!(limit_of_stored(ok), None, "{ok}");
        }
    }

    #[test]
    fn denied_paths_include_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        let home = AbsPath::from_path(&std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        std::fs::write(home.join("history").as_path(), "").unwrap();
        std::os::unix::fs::symlink(
            home.join("history").as_path(),
            home.join(".bash_history").as_path(),
        )
        .unwrap();
        let denied = denied(&home);
        assert!(denied.contains(&home.join(".bash_history")));
        assert!(denied.contains(&home.join("history")));
        assert!(denied.contains(&home.join(".ssh")));
    }
}
