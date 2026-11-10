//! A workspace's file-access decisions that are applied without asking.
//!
//! - **Never** (silent deny): the sandbox spec's never-paths, plus every path answered "Never"
//!   while the workspace is open. A request for one of them, or for anything under one, is
//!   denied at once: no approval, no notification. nono 0.80 can't express this itself on
//!   Linux: its `filesystem.deny` doesn't reach the seccomp-notify supervisor (which asks the
//!   webhook about every path outside the initial capabilities), and refuses to start when a
//!   deny lies under an allowed directory.
//! - **Always** (answered "Always allow" while the workspace is open): granted at once when the
//!   access asked for is covered. The controller adds the standing grant to the machine's
//!   defaults; this applies it to the running workspace's other processes too (nono itself
//!   remembers a grant only for the process tree that asked).
//!
//! Paths are compared as nono reports them and canonicalized, so a symlinked file (a
//! home-manager link) matches either way.

use std::path::Path;

use controlroom_protocol::session::PathAccess;
use controlroom_sandbox::AbsPath;

/// What to do with a request without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auto {
    /// Grant it.
    Grant,
    /// Deny it.
    Deny,
}

/// The rules of one workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathRules {
    never: Vec<AbsPath>,
    always: Vec<(AbsPath, PathAccess)>,
}

impl PathRules {
    /// Rules that deny `never` without asking.
    pub fn new(never: Vec<AbsPath>) -> Self {
        Self {
            never,
            always: Vec::new(),
        }
    }

    /// Denies `path` (and everything under it) from now on.
    pub fn add_never(&mut self, path: AbsPath) {
        if !self.never.contains(&path) {
            self.never.push(path);
        }
    }

    /// Grants `access` on `path` (and everything under it) from now on.
    pub fn add_always(&mut self, path: AbsPath, access: PathAccess) {
        self.always.push((path, access));
    }

    /// The decision for a request of `access` on `path`, if one applies. Never wins over always.
    pub fn decide(&self, path: &str, access: PathAccess) -> Option<Auto> {
        let forms = forms(path);
        if forms.is_empty() {
            return None;
        }
        let covers = |rule: &AbsPath| {
            let rule_forms = forms_of(rule);
            rule_forms
                .iter()
                .any(|r| forms.iter().any(|p| r.contains(p)))
        };
        if self.never.iter().any(covers) {
            return Some(Auto::Deny);
        }
        self.always
            .iter()
            .any(|(rule, granted)| covers(rule) && grants(*granted, access))
            .then_some(Auto::Grant)
    }
}

/// Whether `granted` covers a request for `asked`.
fn grants(granted: PathAccess, asked: PathAccess) -> bool {
    match asked {
        PathAccess::Read => matches!(granted, PathAccess::Read | PathAccess::ReadWrite),
        PathAccess::Write => matches!(granted, PathAccess::Write | PathAccess::ReadWrite),
        PathAccess::ReadWrite => granted == PathAccess::ReadWrite,
        PathAccess::Unknown => false,
    }
}

/// `path` as given and canonicalized (when it is absolute).
fn forms(path: &str) -> Vec<AbsPath> {
    AbsPath::new(path).map(|p| forms_of(&p)).unwrap_or_default()
}

fn forms_of(path: &AbsPath) -> Vec<AbsPath> {
    let mut out = vec![path.clone()];
    if let Some(real) = canonical(path.as_path()).filter(|r| r != path) {
        out.push(real);
    }
    out
}

fn canonical(path: &Path) -> Option<AbsPath> {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|p| AbsPath::from_path(&p).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> AbsPath {
        AbsPath::new(s).unwrap()
    }

    #[test]
    fn never_denies_the_path_and_everything_under_it() {
        let rules = PathRules::new(vec![p("/home/alice/.bash_history"), p("/srv/secret")]);
        let read = PathAccess::Read;
        assert_eq!(
            rules.decide("/home/alice/.bash_history", read),
            Some(Auto::Deny)
        );
        assert_eq!(
            rules.decide("/srv/secret/a/b", PathAccess::Write),
            Some(Auto::Deny)
        );
        assert_eq!(rules.decide("/home/alice/.bash_profile", read), None);
        assert_eq!(rules.decide("relative/path", read), None);
    }

    #[test]
    fn always_grants_covered_access_only_and_never_wins() {
        let mut rules = PathRules::default();
        rules.add_always(p("/home/alice/.cache/nix"), PathAccess::ReadWrite);
        rules.add_always(p("/data/ro"), PathAccess::Read);
        assert_eq!(
            rules.decide("/home/alice/.cache/nix/fetcher.sqlite", PathAccess::Write),
            Some(Auto::Grant)
        );
        assert_eq!(
            rules.decide("/data/ro/x", PathAccess::Read),
            Some(Auto::Grant)
        );
        assert_eq!(rules.decide("/data/ro/x", PathAccess::ReadWrite), None);
        rules.add_never(p("/home/alice/.cache/nix/secret"));
        assert_eq!(
            rules.decide("/home/alice/.cache/nix/secret", PathAccess::Read),
            Some(Auto::Deny)
        );
    }

    #[test]
    fn symlinks_match_by_their_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("history");
        std::fs::write(&target, "").unwrap();
        let link = dir.path().join(".bash_history");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let rules = PathRules::new(vec![AbsPath::from_path(&link).unwrap()]);
        let real = std::fs::canonicalize(&target).unwrap();
        assert_eq!(
            rules.decide(real.to_str().unwrap(), PathAccess::Read),
            Some(Auto::Deny)
        );
    }
}
