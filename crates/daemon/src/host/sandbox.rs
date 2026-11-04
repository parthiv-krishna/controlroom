//! Workspace sandboxes: the effective policy from the agent daemon's [`SandboxSpec`], the
//! generated nono profile, and wrapping a process (shell, job, fs worker) in `nono run`.
//!
//! - The policy merges the spec §11.5b default mediated commands (as a baseline) with the
//!   spec's grants (`once` grants skipped), domains and mediated commands, at the spec's level.
//!   Grants that can't be expressed are dropped (which only narrows access); an invalid
//!   mediated command is refused (dropping it would widen access).
//! - Without a running approval webhook, approve rules become deny (fail closed).
//! - Level `off` has no profile: processes run directly, unsandboxed (the UI shows a badge).

use std::collections::BTreeMap;

use controlroom_protocol::session::{
    GrantKind, GrantScope, MediationAction, PathAccess, SandboxLevel as WireLevel, SandboxSpec,
};
use controlroom_protocol::{ErrorData, RpcError};
use controlroom_sandbox::launch::{NONO_ENV_PREFIX, NonoDirs};
use controlroom_sandbox::pins::resolve_pin;
use controlroom_sandbox::policy::{
    Access, CommandDecision, CommandRule, PathGrant, PathKind, ScopePolicy, default_command_rules,
};
use controlroom_sandbox::spec::Approvals;
use controlroom_sandbox::webhook::HookUrl;
use controlroom_sandbox::{
    AbsPath, Domain, EffectivePolicy, Name, SandboxLevel, SessionId, WorkspaceSandboxSpec,
    effective_policy, generate_workspace_profile,
};

use crate::config::sandbox::{self as sandbox_config, SandboxSettings};
use crate::dirs::write_private_file;
use crate::dirs::{Dirs, WorkspacePaths};
use crate::shell::{find_program, login_shell};

/// A `sandbox_unavailable` error.
pub fn unavailable(detail: impl Into<String>) -> RpcError {
    let detail = detail.into();
    RpcError::new(
        ErrorData::SandboxUnavailable {
            detail: detail.clone(),
        },
        detail,
    )
}

/// What the host needs to sandbox processes, resolved once at startup.
#[derive(Debug, Clone)]
pub struct SandboxEnv {
    /// `sandbox.toml`, resolved (its defaults if the file is unusable).
    pub settings: SandboxSettings,
    /// The nono binary, if found.
    pub nono: Option<AbsPath>,
    /// The daemon's `PATH`, which mediated commands are pinned against.
    pub path_var: String,
    /// This `controlroomd`: the fs worker runs it inside the sandbox, so profiles may read it.
    pub exe: AbsPath,
}

impl SandboxEnv {
    /// Reads `sandbox.toml` and looks for nono through the login shell.
    pub fn load(dirs: &Dirs, exe: AbsPath) -> Self {
        let loaded = sandbox_config::load(dirs);
        if let Some(e) = &loaded.error {
            tracing::warn!("{e}; using the defaults");
        }
        let settings = loaded
            .value
            .resolve(dirs.home())
            .or_else(|e| {
                tracing::warn!("{e}; using the defaults");
                sandbox_config::SandboxConfig::default().resolve(dirs.home())
            })
            .expect("the default sandbox settings resolve");
        let nono = find_program(&login_shell(), &settings.nono, dirs.home())
            .inspect_err(|e| tracing::warn!("nono: {e}; only level off workspaces can run"))
            .ok();
        Self {
            settings,
            nono,
            path_var: std::env::var("PATH").unwrap_or_default(),
            exe,
        }
    }
}

/// The level of a spec (an unknown level is treated as the strictest).
pub fn level(spec: &SandboxSpec) -> SandboxLevel {
    match spec.level {
        WireLevel::Off => SandboxLevel::Off,
        WireLevel::Relaxed => SandboxLevel::Relaxed,
        WireLevel::Strict | WireLevel::Unknown => SandboxLevel::Strict,
    }
}

/// The effective policy of a workspace; see the module docs.
pub fn policy(spec: &SandboxSpec) -> Result<EffectivePolicy, RpcError> {
    let baseline = ScopePolicy {
        commands: default_command_rules(),
        ..ScopePolicy::default()
    };
    let mut defaults = ScopePolicy::default();
    for grant in &spec.grants {
        if grant.scope == GrantScope::Once {
            continue;
        }
        match grant.kind {
            GrantKind::Path => {
                let Ok(path) = AbsPath::new(&grant.value) else {
                    tracing::warn!("ignoring the path grant {:?}", grant.value);
                    continue;
                };
                let kind = if path.as_path().is_file() {
                    PathKind::File
                } else {
                    PathKind::Dir
                };
                let access = match grant.access {
                    Some(PathAccess::Write | PathAccess::ReadWrite) => Access::ReadWrite,
                    _ => Access::Read,
                };
                defaults.paths.push(PathGrant { path, kind, access });
            }
            GrantKind::Domain => match Domain::new(&grant.value) {
                Ok(domain) => defaults.domains.push(domain),
                Err(e) => tracing::warn!("ignoring the domain grant {:?}: {e}", grant.value),
            },
            GrantKind::Command | GrantKind::Unknown => {}
        }
    }
    for domain in &spec.domains {
        match Domain::new(domain) {
            Ok(domain) => defaults.domains.push(domain),
            Err(e) => tracing::warn!("ignoring the domain {domain:?}: {e}"),
        }
    }
    for mediated in &spec.mediated {
        let mut words = mediated.command.split_whitespace();
        let command = words
            .next()
            .and_then(|c| Name::new(c).ok())
            .ok_or_else(|| {
                unavailable(format!("invalid mediated command {:?}", mediated.command))
            })?;
        defaults.commands.push(CommandRule {
            command,
            args: words.map(str::to_owned).collect(),
            decision: match mediated.action {
                MediationAction::Approve => CommandDecision::Approve,
                MediationAction::Deny | MediationAction::Unknown => CommandDecision::Deny,
            },
        });
    }
    let session = ScopePolicy {
        level: Some(level(spec)),
        ..ScopePolicy::default()
    };
    Ok(effective_policy([&baseline, &defaults], &session))
}

/// Where a workspace's profile comes from.
pub struct ProfileInput<'a> {
    /// The workspace id.
    pub id: &'a str,
    /// Its resolved root.
    pub root: &'a AbsPath,
    /// Its files.
    pub files: &'a WorkspacePaths,
    /// Its policy.
    pub policy: EffectivePolicy,
    /// The approval webhook, if it runs.
    pub hook: Option<HookUrl>,
}

/// Writes the workspace's nono profile; `None` (and no file) at level off.
pub fn write_profile(
    env: &SandboxEnv,
    dirs: &Dirs,
    input: ProfileInput<'_>,
) -> Result<Option<AbsPath>, RpcError> {
    let profile = input.files.profile();
    if input.policy.level == SandboxLevel::Off {
        let _ = std::fs::remove_file(profile.as_path());
        return Ok(None);
    }
    if env.nono.is_none() {
        return Err(unavailable(format!(
            "nono ({}) was not found on this host; install it or use sandbox level off",
            env.settings.nono
        )));
    }
    let mut policy = input.policy;
    if input.hook.is_none() {
        for rule in &mut policy.commands {
            if rule.decision == CommandDecision::Approve {
                rule.decision = CommandDecision::Deny;
            }
        }
    }
    let pins = policy
        .commands
        .iter()
        .filter_map(|rule| {
            let pin = resolve_pin(rule.command.as_str(), &env.path_var)?;
            Some((rule.command.clone(), pin))
        })
        .collect::<BTreeMap<_, _>>();
    let mut read_only = env.settings.read_only.clone();
    read_only.push(input.files.view());
    if !read_only.iter().any(|r| r.contains(&env.exe)) {
        read_only.push(env.exe.clone());
    }
    let ssh = dirs.home().join(".ssh");
    let spec = WorkspaceSandboxSpec {
        id: SessionId::new(input.id).map_err(|e| unavailable(e.to_string()))?,
        root: input.root.clone(),
        dir: input.files.dir().clone(),
        tmp: input.files.tmp(),
        policy,
        read_only,
        relaxed_reads: env.settings.relaxed_reads.clone(),
        // nono refuses a missing executable directory.
        exec_dirs: env
            .settings
            .exec_dirs
            .iter()
            .filter(|d| d.as_path().is_dir())
            .cloned()
            .collect(),
        ssh_config: ssh.as_path().is_dir().then_some(ssh).into_iter().collect(),
        protected: vec![dirs.state().clone(), dirs.data(), dirs.config()],
        approvals: input.hook.map(|hook| Approvals {
            hook,
            timeout_secs: env.settings.approval_timeout_secs,
        }),
        pins,
    };
    let generated = generate_workspace_profile(&spec).map_err(|e| unavailable(e.to_string()))?;
    write_private_file(&profile, generated.to_json_pretty().as_bytes())
        .map_err(|e| unavailable(format!("writing {profile}: {e}")))?;
    Ok(Some(profile))
}

/// A process launch, possibly wrapped in nono.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    /// The argv to execute.
    pub argv: Vec<String>,
    /// Variables for the launched process (nono's own when sandboxed).
    pub env: Vec<(String, String)>,
}

/// Wraps `argv` in `nono run` with `profile` (working directory `root`); unchanged without a
/// profile (level off). Fails if the daemon inherited a `NONO_*` variable, which would widen
/// the sandbox.
pub fn wrap(
    env: &SandboxEnv,
    dirs: &Dirs,
    profile: Option<&AbsPath>,
    root: &AbsPath,
    argv: Vec<String>,
) -> Result<Wrapped, RpcError> {
    let Some(profile) = profile else {
        return Ok(Wrapped {
            argv,
            env: Vec::new(),
        });
    };
    if let Some((key, _)) =
        std::env::vars_os().find(|(k, _)| k.to_string_lossy().starts_with(NONO_ENV_PREFIX))
    {
        return Err(unavailable(format!(
            "the host daemon inherited {}; restart it without NONO_* variables",
            key.to_string_lossy()
        )));
    }
    let nono = env
        .nono
        .as_ref()
        .ok_or_else(|| unavailable("nono was not found on this host"))?;
    let mut wrapped = vec![
        nono.to_string(),
        "run".to_owned(),
        "-s".to_owned(),
        "--no-rollback".to_owned(),
        "--profile".to_owned(),
        profile.to_string(),
        "--workdir".to_owned(),
        root.to_string(),
        "--allow-cwd".to_owned(),
        "--".to_owned(),
    ];
    wrapped.extend(argv);
    let mut vars = NonoDirs::under(dirs.state()).env();
    vars.push(("TMPDIR".to_owned(), env.settings.nono_tmp.to_string()));
    Ok(Wrapped {
        argv: wrapped,
        env: vars,
    })
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::ids::GrantId;
    use controlroom_protocol::session::{MediatedCommand, SandboxGrant};

    use super::*;

    #[test]
    fn policies_merge_defaults_grants_and_level() {
        let spec = SandboxSpec {
            level: WireLevel::Relaxed,
            grants: vec![
                SandboxGrant {
                    id: GrantId::generate(),
                    kind: GrantKind::Domain,
                    value: "registry.example.com".into(),
                    access: None,
                    scope: GrantScope::Standing,
                    granted_at: UnixMillis(1),
                    by: None,
                },
                SandboxGrant {
                    id: GrantId::generate(),
                    kind: GrantKind::Domain,
                    value: "once.example.com".into(),
                    access: None,
                    scope: GrantScope::Once,
                    granted_at: UnixMillis(1),
                    by: None,
                },
            ],
            domains: vec!["git.example.com".into()],
            mediated: vec![MediatedCommand {
                command: "kubectl apply".into(),
                action: MediationAction::Approve,
            }],
        };
        let policy = policy(&spec).unwrap();
        assert_eq!(policy.level, SandboxLevel::Relaxed);
        let domains: Vec<_> = policy.domains.iter().map(|d| d.as_str()).collect();
        assert_eq!(domains, ["git.example.com", "registry.example.com"]);
        assert!(policy.commands.iter().any(|r| r.command.as_str() == "git"));
        assert!(
            policy
                .commands
                .iter()
                .any(|r| r.command.as_str() == "kubectl" && r.args == ["apply"])
        );

        let bad = SandboxSpec {
            mediated: vec![MediatedCommand {
                command: "../x".into(),
                action: MediationAction::Deny,
            }],
            ..spec
        };
        assert!(super::policy(&bad).is_err());
    }
}
