//! Sandbox policy inputs and the effective-profile merge (spec §11.6).
//!
//! The controller stores per-machine and per-project defaults ([`ScopePolicy`]) and passes
//! them to the daemon when a session is created or restarted. [`effective_policy`] combines
//! them: the strictest level wins, and standing grants, domains and command rules are the
//! union, so access is never widened beyond what some scope granted. Session-level settings
//! are added on top.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::names::{Domain, Name};
use crate::paths::AbsPath;

/// Per-session sandbox level. Ordered from least to most strict.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxLevel {
    /// No sandbox; the UI shows a warning badge.
    Off,
    /// Strict plus read-only home configuration (e.g. git config).
    Relaxed,
    /// The working directory and the session directories only (the default).
    #[default]
    Strict,
}

/// Access granted on a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Read-only.
    Read,
    /// Read and write.
    ReadWrite,
}

/// Whether a granted path is a directory (recursive) or a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// A directory, granted recursively.
    Dir,
    /// A single file.
    File,
}

/// A standing filesystem grant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PathGrant {
    /// The granted path.
    pub path: AbsPath,
    /// Directory or file.
    pub kind: PathKind,
    /// Read or read-write.
    pub access: Access,
}

/// What happens when a mediated command matches a rule. Ordered from least to most strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandDecision {
    /// Run under the command sandbox without asking.
    Run,
    /// Ask the user each time (webhook → `request{kind: sandbox}`).
    Approve,
    /// Refuse.
    Deny,
}

/// A command-mediation rule: `command` invoked with `args` as a contiguous argument window
/// (e.g. `git` + `["push"]`). Empty `args` applies to every invocation of the command.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandRule {
    /// The command name as found on `PATH` (e.g. `git`, `kubectl`).
    pub command: Name,
    /// Contiguous argument window to match; empty matches every invocation.
    #[serde(default)]
    pub args: Vec<String>,
    /// The decision for a matching invocation.
    pub decision: CommandDecision,
}

/// Sandbox defaults of one scope (a machine or a project), or a session's own settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePolicy {
    /// The level, if this scope sets one.
    #[serde(default)]
    pub level: Option<SandboxLevel>,
    /// Standing path grants.
    #[serde(default)]
    pub paths: Vec<PathGrant>,
    /// Domains allowed through the proxy.
    #[serde(default)]
    pub domains: Vec<Domain>,
    /// Command-mediation rules.
    #[serde(default)]
    pub commands: Vec<CommandRule>,
}

/// The merged policy for one session. Collections are sorted and deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    /// The effective level.
    pub level: SandboxLevel,
    /// Path grants, one per path, with the widest access any scope granted for that path.
    pub paths: Vec<PathGrant>,
    /// Allowed domains.
    pub domains: Vec<Domain>,
    /// Command rules, one per (command, args), with the strictest decision.
    pub commands: Vec<CommandRule>,
}

/// Merges scope defaults (machine and every project the session belongs to) with the
/// session's own settings.
///
/// - level: the session's explicit level if set (an affirmative user choice), otherwise the
///   strictest level among the scopes, otherwise [`SandboxLevel::Strict`];
/// - paths, domains, command rules: the union of all scopes plus the session. A path granted
///   twice keeps the wider access (both grants were made explicitly); a command rule present
///   twice keeps the stricter decision.
pub fn effective_policy<'a>(
    scopes: impl IntoIterator<Item = &'a ScopePolicy>,
    session: &ScopePolicy,
) -> EffectivePolicy {
    let scopes: Vec<&ScopePolicy> = scopes.into_iter().collect();
    let level = session
        .level
        .or_else(|| scopes.iter().filter_map(|s| s.level).max())
        .unwrap_or_default();

    let all = || scopes.iter().copied().chain(std::iter::once(session));

    let mut paths: BTreeMap<AbsPath, PathGrant> = BTreeMap::new();
    for grant in all().flat_map(|s| &s.paths) {
        paths
            .entry(grant.path.clone())
            .and_modify(|g| g.access = g.access.max(grant.access))
            .or_insert_with(|| grant.clone());
    }

    let mut domains: Vec<Domain> = all().flat_map(|s| s.domains.iter().cloned()).collect();
    domains.sort();
    domains.dedup();

    let mut commands: BTreeMap<(Name, Vec<String>), CommandDecision> = BTreeMap::new();
    for rule in all().flat_map(|s| &s.commands) {
        commands
            .entry((rule.command.clone(), rule.args.clone()))
            .and_modify(|d| *d = (*d).max(rule.decision))
            .or_insert(rule.decision);
    }

    EffectivePolicy {
        level,
        paths: paths.into_values().collect(),
        domains,
        commands: commands
            .into_iter()
            .map(|((command, args), decision)| CommandRule {
                command,
                args,
                decision,
            })
            .collect(),
    }
}

/// The default mediated commands (spec §11.5b): `git push` needs approval each time; direct
/// `ssh`, `scp` and `sftp` are denied (git may still use ssh internally); `gh` runs normally.
pub fn default_command_rules() -> Vec<CommandRule> {
    let rule = |command: &str, args: &[&str], decision| CommandRule {
        command: Name::new(command).expect("static command names are valid"),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        decision,
    };
    vec![
        rule("git", &["push"], CommandDecision::Approve),
        rule("ssh", &[], CommandDecision::Deny),
        rule("scp", &[], CommandDecision::Deny),
        rule("sftp", &[], CommandDecision::Deny),
        rule("gh", &[], CommandDecision::Run),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(path: &str, access: Access) -> PathGrant {
        PathGrant {
            path: AbsPath::new(path).unwrap(),
            kind: PathKind::Dir,
            access,
        }
    }

    fn rule(command: &str, args: &[&str], decision: CommandDecision) -> CommandRule {
        CommandRule {
            command: Name::new(command).unwrap(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            decision,
        }
    }

    #[test]
    fn strictest_level_wins_and_defaults_to_strict() {
        let relaxed = ScopePolicy {
            level: Some(SandboxLevel::Relaxed),
            ..Default::default()
        };
        let off = ScopePolicy {
            level: Some(SandboxLevel::Off),
            ..Default::default()
        };
        let unset = ScopePolicy::default();
        let session = ScopePolicy::default();
        assert_eq!(
            effective_policy([&off, &relaxed], &session).level,
            SandboxLevel::Relaxed
        );
        assert_eq!(effective_policy([&off], &session).level, SandboxLevel::Off);
        assert_eq!(
            effective_policy([&unset], &session).level,
            SandboxLevel::Strict
        );
        assert_eq!(effective_policy([], &session).level, SandboxLevel::Strict);
    }

    #[test]
    fn explicit_session_level_overrides_scopes() {
        let strict = ScopePolicy {
            level: Some(SandboxLevel::Strict),
            ..Default::default()
        };
        let session = ScopePolicy {
            level: Some(SandboxLevel::Relaxed),
            ..Default::default()
        };
        assert_eq!(
            effective_policy([&strict], &session).level,
            SandboxLevel::Relaxed
        );
    }

    #[test]
    fn grants_and_domains_are_a_sorted_union() {
        let machine = ScopePolicy {
            paths: vec![
                grant("/data/b", Access::Read),
                grant("/data/a", Access::Read),
            ],
            domains: vec![Domain::new("b.example.com").unwrap()],
            ..Default::default()
        };
        let project = ScopePolicy {
            paths: vec![grant("/data/a", Access::ReadWrite)],
            domains: vec![
                Domain::new("a.example.com").unwrap(),
                Domain::new("b.example.com").unwrap(),
            ],
            ..Default::default()
        };
        let session = ScopePolicy {
            paths: vec![grant("/data/c", Access::Read)],
            ..Default::default()
        };
        let eff = effective_policy([&machine, &project], &session);
        assert_eq!(
            eff.paths,
            vec![
                grant("/data/a", Access::ReadWrite),
                grant("/data/b", Access::Read),
                grant("/data/c", Access::Read),
            ]
        );
        let domains: Vec<&str> = eff.domains.iter().map(Domain::as_str).collect();
        assert_eq!(domains, ["a.example.com", "b.example.com"]);
    }

    #[test]
    fn duplicate_command_rules_keep_the_strictest_decision() {
        let machine = ScopePolicy {
            commands: vec![
                rule("kubectl", &["apply"], CommandDecision::Approve),
                rule("gh", &[], CommandDecision::Run),
            ],
            ..Default::default()
        };
        let project = ScopePolicy {
            commands: vec![rule("kubectl", &["apply"], CommandDecision::Deny)],
            ..Default::default()
        };
        let eff = effective_policy([&machine, &project], &ScopePolicy::default());
        assert_eq!(
            eff.commands,
            vec![
                rule("gh", &[], CommandDecision::Run),
                rule("kubectl", &["apply"], CommandDecision::Deny),
            ]
        );
    }

    #[test]
    fn default_rules_cover_the_spec_table() {
        let rules = default_command_rules();
        assert!(rules.contains(&rule("git", &["push"], CommandDecision::Approve)));
        for cmd in ["ssh", "scp", "sftp"] {
            assert!(rules.contains(&rule(cmd, &[], CommandDecision::Deny)));
        }
    }
}
