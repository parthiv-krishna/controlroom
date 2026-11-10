//! Workspace sandboxes: the effective policy from the agent daemon's [`SandboxSpec`], the
//! generated nono profile, and wrapping a process (shell, job, fs worker) in `nono run`.
//!
//! - The policy merges the spec §11.5b default mediated commands (as a baseline) with the
//!   spec's grants (`once` grants skipped), domains and mediated commands, at the spec's level.
//!   Path and socket grants and never-paths may start with `~` (this host's home). A path grant
//!   is a file grant if the path is a file when the profile is written, else a directory grant
//!   (nono skips a path that doesn't exist yet; it takes effect in the next profile written
//!   after it appears). Grants that can't take effect are dropped (which only narrows access):
//!   invalid paths and the ones [`GrantLimits`] names; an invalid mediated command is refused
//!   (dropping it would widen access).
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
    effective_policy, generate_workspace_profile, limits,
};

use crate::config::sandbox::{self as sandbox_config, SandboxSettings};
use crate::dirs::{Dirs, WorkspacePaths, expand_home, write_private_file};
use crate::nono::{Nono, NonoMissing, NonoOrigin};

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

/// The `sandbox_unavailable` error for a workspace that needs nono when there is none.
fn nono_unavailable(missing: &NonoMissing) -> RpcError {
    let fix = match missing.origin {
        NonoOrigin::Bundled => "reinstall controlroomd from controlroom",
        NonoOrigin::Configured => "fix or remove `nono` in sandbox.toml",
    };
    unavailable(format!(
        "nono: {}; {fix}, or use sandbox level off",
        missing.detail
    ))
}

/// What the host needs to sandbox processes, resolved once at startup.
#[derive(Debug, Clone)]
pub struct SandboxEnv {
    /// `sandbox.toml`, resolved (its defaults if the file is unusable).
    pub settings: SandboxSettings,
    /// The nono binary, or why there is none.
    pub nono: Result<Nono, NonoMissing>,
    /// The daemon's `PATH`, which mediated commands are pinned against.
    pub path_var: String,
    /// This `controlroomd`: the fs worker runs it inside the sandbox, so profiles may read it.
    pub exe: AbsPath,
}

impl SandboxEnv {
    /// Reads `sandbox.toml` and locates nono (see [`crate::nono`]).
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
        let nono = crate::nono::locate(settings.nono.as_deref(), &exe, dirs.home())
            .inspect(|n| tracing::info!(path = %n.path, source = %n.origin, "nono"))
            .inspect_err(|e| tracing::warn!("nono: {e}; only level off workspaces can run"));
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

/// The effective policy of a workspace (`~` is `home`); see the module docs.
pub fn policy(spec: &SandboxSpec, home: &AbsPath) -> Result<EffectivePolicy, RpcError> {
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
                let Ok(path) = expand_home(&grant.value, home) else {
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
            GrantKind::Socket => match expand_home(&grant.value, home) {
                Ok(socket) => defaults.sockets.push(socket),
                Err(e) => tracing::warn!("ignoring the socket grant {:?}: {e}", grant.value),
            },
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
    /// Where to write the profile (the agent's or the user's terminals').
    pub path: AbsPath,
    /// Its policy.
    pub policy: EffectivePolicy,
    /// The approval webhook, if it runs.
    pub hook: Option<HookUrl>,
}

/// Writes a nono profile of the workspace to `input.path`; `None` (and no file) at level off.
pub fn write_profile(
    env: &SandboxEnv,
    dirs: &Dirs,
    input: ProfileInput<'_>,
) -> Result<Option<AbsPath>, RpcError> {
    let profile = input.path.clone();
    if input.policy.level == SandboxLevel::Off {
        let _ = std::fs::remove_file(profile.as_path());
        return Ok(None);
    }
    if let Err(missing) = &env.nono {
        return Err(nono_unavailable(missing));
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
    let grant_limits = GrantLimits::new(env, dirs);
    grant_limits.drop_unusable(&mut policy);
    let mut read_only = env.settings.read_only.clone();
    read_only.push(input.files.view());
    // The fs worker runs this controlroomd inside the sandbox. Its directory, not the file:
    // nono's sandboxes of mediated commands (git push) take directories only, and refuse to
    // run the approved command otherwise. Installed, that is `versions/<version>/`.
    let exe_dir = env.exe.parent().unwrap_or_else(|| env.exe.clone());
    if !read_only.iter().any(|r| r.contains(&exe_dir)) {
        read_only.push(exe_dir);
    }
    let GrantLimits {
        home,
        protected,
        exec_dirs,
    } = grant_limits;
    let ssh = home.join(".ssh");
    let spec = WorkspaceSandboxSpec {
        id: SessionId::new(input.id).map_err(|e| unavailable(e.to_string()))?,
        root: input.root.clone(),
        dir: input.files.dir().clone(),
        tmp: input.files.tmp(),
        policy,
        read_only,
        relaxed_reads: env.settings.relaxed_reads.clone(),
        exec_dirs,
        ssh_config: ssh.as_path().is_dir().then_some(ssh).into_iter().collect(),
        protected,
        nono_denied: limits::denied(&home),
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

/// What a standing grant can't cover on this host (spec §11.6): the profile drops such grants,
/// and file requests for such paths don't offer "Always allow".
#[derive(Debug, Clone)]
pub struct GrantLimits {
    /// The host user's home, which nono's built-in denials are relative to.
    home: AbsPath,
    /// The daemon's state, data and config dirs (webhook secrets, profiles, its socket).
    protected: Vec<AbsPath>,
    /// The existing executable directories besides `PATH`'s (nono refuses a missing one).
    exec_dirs: Vec<AbsPath>,
}

impl GrantLimits {
    /// The limits of this host.
    pub fn new(env: &SandboxEnv, dirs: &Dirs) -> Self {
        Self {
            home: dirs.home().clone(),
            protected: vec![dirs.state().clone(), dirs.data(), dirs.config()],
            exec_dirs: env
                .settings
                .exec_dirs
                .iter()
                .filter(|d| d.as_path().is_dir())
                .cloned()
                .collect(),
        }
    }

    /// Why a standing grant of `access` on `path` can't take effect here, if it can't: a
    /// read-write `/`, controlroom's own state (inside or covering it), a read-write grant
    /// covering an executable directory, or what nono can't hold ([`limits::limit`]).
    pub fn path_problem(&self, path: &AbsPath, access: Access) -> Option<String> {
        let writable = access == Access::ReadWrite;
        if writable && path.parent().is_none() {
            return Some("/ can't be read-write".into());
        }
        if self.overlaps_protected(path) {
            return Some("it overlaps controlroom's own state".into());
        }
        if writable && self.exec_dirs.iter().any(|d| path.contains(d)) {
            return Some("it would make an executable directory writable".into());
        }
        let file = std::fs::metadata(path.as_path()).is_ok_and(|m| !m.is_dir());
        limits::limit(path, file, &self.home).map(|limit| limit.to_string())
    }

    fn overlaps_protected(&self, path: &AbsPath) -> bool {
        self.protected
            .iter()
            .any(|p| path.contains(p) || p.contains(path))
    }

    /// Drops the grants a profile can't hold (which only narrows access) and logs each.
    fn drop_unusable(&self, policy: &mut EffectivePolicy) {
        policy.paths.retain(|grant| {
            let problem = self.path_problem(&grant.path, grant.access);
            if let Some(problem) = &problem {
                tracing::warn!("ignoring the path grant {}: {problem}", grant.path);
            }
            problem.is_none()
        });
        policy.sockets.retain(|socket| {
            let own = self.overlaps_protected(socket);
            if own {
                tracing::warn!("ignoring the socket grant {socket}: it is controlroom's own");
            }
            !own
        });
    }
}

/// The never-paths of a workspace (`~` is `home`); invalid ones are skipped.
pub fn never_paths(spec: &SandboxSpec, home: &AbsPath) -> Vec<AbsPath> {
    spec.never
        .iter()
        .filter_map(|path| {
            expand_home(path, home)
                .inspect_err(|e| tracing::warn!("ignoring the never-path {path:?}: {e}"))
                .ok()
        })
        .collect()
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
    let nono = &env.nono.as_ref().map_err(nono_unavailable)?.path;
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

    fn home() -> AbsPath {
        AbsPath::new("/home/alice").unwrap()
    }

    fn path_grant(value: &str, access: PathAccess) -> SandboxGrant {
        SandboxGrant {
            id: GrantId::generate(),
            kind: GrantKind::Path,
            value: value.into(),
            access: Some(access),
            scope: GrantScope::Standing,
            granted_at: UnixMillis(1),
            by: None,
        }
    }

    /// Path grants and never-paths expand `~`; invalid ones are skipped, and so are grants a
    /// profile can't hold.
    #[test]
    fn path_grants_expand_home_and_unusable_ones_are_dropped() {
        let spec = SandboxSpec {
            level: WireLevel::Strict,
            grants: vec![
                path_grant("~/.cache/nix", PathAccess::ReadWrite),
                path_grant("/data/sets", PathAccess::Read),
                path_grant("relative", PathAccess::Read),
                path_grant("/a/../b", PathAccess::Read),
            ],
            domains: Vec::new(),
            mediated: Vec::new(),
            never: vec!["~/.bash_history".into(), "nope".into()],
        };
        let mut policy = policy(&spec, &home()).unwrap();
        let granted: Vec<(&str, Access)> = policy
            .paths
            .iter()
            .map(|g| (g.path.as_str(), g.access))
            .collect();
        assert_eq!(
            granted,
            [
                ("/data/sets", Access::Read),
                ("/home/alice/.cache/nix", Access::ReadWrite)
            ]
        );
        assert_eq!(
            never_paths(&spec, &home()),
            [AbsPath::new("/home/alice/.bash_history").unwrap()]
        );

        let grant = |path: &str, access| PathGrant {
            path: AbsPath::new(path).unwrap(),
            kind: PathKind::Dir,
            access,
        };
        policy.paths = vec![
            grant("/", Access::ReadWrite),
            grant("/home/alice", Access::Read),
            grant("/usr", Access::ReadWrite),
            grant("/usr", Access::Read),
            grant("/home/alice/.cache/nix", Access::ReadWrite),
            grant(
                "/home/alice/.local/state/controlroom/host.sqlite",
                Access::Read,
            ),
            grant("/home/alice/.config", Access::Read),
            grant("/home/alice/.bash_history", Access::Read),
            grant("/proc/1/root", Access::Read),
            grant("/proc", Access::Read),
        ];
        policy.sockets = vec![
            AbsPath::new("/run/dbus/system_bus_socket").unwrap(),
            AbsPath::new("/home/alice/.local/state/controlroom/run/daemon.sock").unwrap(),
        ];
        limits().drop_unusable(&mut policy);
        let kept: Vec<&str> = policy.paths.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(
            kept,
            [
                "/usr",
                "/home/alice/.cache/nix",
                "/home/alice/.bash_history",
                "/proc"
            ]
        );
        assert_eq!(
            policy.sockets,
            [AbsPath::new("/run/dbus/system_bus_socket").unwrap()]
        );
    }

    /// The limits of a host whose home is `/home/alice`.
    fn limits() -> GrantLimits {
        GrantLimits {
            home: home(),
            protected: vec![AbsPath::new("/home/alice/.local/state/controlroom").unwrap()],
            exec_dirs: vec![AbsPath::new("/usr/bin").unwrap()],
        }
    }

    #[test]
    fn grant_limits_explain_why_a_grant_cant_stand() {
        let problem =
            |path: &str, access| limits().path_problem(&AbsPath::new(path).unwrap(), access);
        assert_eq!(problem("/home/alice/.bash_history", Access::Read), None);
        assert_eq!(problem("/srv/data", Access::ReadWrite), None);
        assert!(
            problem("/", Access::ReadWrite)
                .unwrap()
                .contains("read-write")
        );
        assert!(
            problem("/home/alice/.config", Access::Read)
                .unwrap()
                .contains("/home/alice/.config/gcloud")
        );
        assert!(
            problem("/proc/1/root", Access::Read)
                .unwrap()
                .contains("/proc entry")
        );
        assert!(
            problem("/home/alice/.local/state/controlroom/x", Access::Read)
                .unwrap()
                .contains("controlroom's own state")
        );
        assert!(
            problem("/usr", Access::ReadWrite)
                .unwrap()
                .contains("executable directory")
        );
    }

    #[test]
    fn socket_grants_expand_home() {
        let spec = SandboxSpec {
            level: WireLevel::Strict,
            grants: vec![SandboxGrant {
                id: GrantId::generate(),
                kind: GrantKind::Socket,
                value: "~/run/app.sock".into(),
                access: None,
                scope: GrantScope::Standing,
                granted_at: UnixMillis(1),
                by: None,
            }],
            domains: Vec::new(),
            mediated: Vec::new(),
            never: Vec::new(),
        };
        let policy = policy(&spec, &home()).unwrap();
        assert_eq!(
            policy.sockets,
            [AbsPath::new("/home/alice/run/app.sock").unwrap()]
        );
        assert!(policy.paths.is_empty());
    }

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
            never: Vec::new(),
        };
        let policy = policy(&spec, &home()).unwrap();
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
        assert!(super::policy(&bad, &home()).is_err());
    }

    /// nono refuses to run an approved command (git push) whose sandbox lists a file among its
    /// readable or executable directories, such as the daemon's own binary.
    #[test]
    fn mediated_command_sandboxes_list_directories_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = AbsPath::from_path(tmp.path()).unwrap();
        let dirs = Dirs::under_home(home.clone());
        let make_dir = |p: &AbsPath| std::fs::create_dir_all(p.as_path()).unwrap();
        let make_file = |p: &AbsPath| {
            make_dir(&p.parent().unwrap());
            std::fs::write(p.as_path(), "").unwrap();
            let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(p.as_path(), mode).unwrap();
        };
        let bin = home.join("bin");
        make_file(&bin.join("git"));
        let exe = dirs.data().join("versions/1.0.0/controlroomd");
        make_file(&exe);
        let root = home.join("work");
        make_dir(&root);
        let files = dirs.workspace("01JAW000000000000000000001");
        make_dir(files.dir());
        make_dir(&files.view());

        let env = SandboxEnv {
            settings: sandbox_config::SandboxConfig::default()
                .resolve(&home)
                .unwrap(),
            nono: Ok(Nono {
                path: AbsPath::new("/run/current-system/sw/bin/nono").unwrap(),
                origin: NonoOrigin::Bundled,
            }),
            path_var: bin.to_string(),
            exe: exe.clone(),
        };
        let spec = SandboxSpec {
            level: WireLevel::Strict,
            grants: Vec::new(),
            domains: Vec::new(),
            mediated: Vec::new(),
            never: Vec::new(),
        };
        let hook = HookUrl {
            port: 1,
            session: controlroom_sandbox::SessionId::new("01JAW000000000000000000001").unwrap(),
            secret: controlroom_sandbox::webhook::HookSecret::generate().unwrap(),
        };
        let input = ProfileInput {
            id: "01JAW000000000000000000001",
            root: &root,
            files: &files,
            path: files.profile(),
            policy: policy(&spec, &home).unwrap(),
            hook: Some(hook),
        };
        let profile = write_profile(&env, &dirs, input).unwrap().unwrap();
        let profile: serde_json::Value =
            serde_json::from_slice(&std::fs::read(profile.as_path()).unwrap()).unwrap();

        let mut listed = Vec::new();
        collect_dirs(&profile, &mut listed);
        assert!(
            listed.iter().any(|p| *p == exe.parent().unwrap().as_str()),
            "git's sandbox can read the daemon's directory: {listed:?}"
        );
        for path in listed {
            let path = std::path::Path::new(&path);
            assert!(
                !path.exists() || path.is_dir(),
                "{} is listed as a directory",
                path.display()
            );
        }
    }

    /// Every entry of the `fs_read`, `fs_write` and `exec_paths` lists in `value`.
    fn collect_dirs(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    if matches!(key.as_str(), "fs_read" | "fs_write" | "exec_paths") {
                        let items = value.as_array().into_iter().flatten();
                        out.extend(items.filter_map(|v| v.as_str().map(str::to_owned)));
                    } else {
                        collect_dirs(value, out);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| collect_dirs(v, out)),
            _ => {}
        }
    }
}
