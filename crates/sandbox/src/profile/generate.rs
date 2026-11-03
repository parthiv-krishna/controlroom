//! Session spec → nono profile.

use std::collections::BTreeMap;

use super::model::*;
use super::validate::{ProfileError, validate_spec};
use crate::names::Name;
use crate::paths::AbsPath;
use crate::policy::{Access, CommandDecision, CommandRule, PathKind};
use crate::spec::{CredentialRoute, Endpoint, SessionSandboxSpec};

/// Name of the single approval backend in generated profiles.
const BACKEND: &str = "controlroom";

/// Built-in groups excluded from the implicit `default` profile. With command mediation
/// active nono turns them into deny-only commands, which would block `rm`, `mv`, `cp`,
/// `kill`, `npm` and more inside the working directory. Landlock already confines writes.
const EXCLUDED_GROUPS: [&str; 3] = [
    "dangerous_commands",
    "dangerous_commands_linux",
    "dangerous_commands_macos",
];

/// Inherited variables stripped from the sandbox environment (defense in depth; the
/// supervisor already controls the agent's environment).
const DENY_VARS: [&str; 9] = [
    "*_API_KEY",
    "*_SECRET",
    "*_SECRET_KEY",
    "*_PASSWORD",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "SSH_AUTH_SOCK",
];

/// Commands that mediated callers may invoke: `git` may run `ssh` (fetch and push over SSH)
/// even though direct `ssh` is denied.
const CHAINS: [(&str, &str); 1] = [("git", "ssh")];

/// Timeout of the host-side credential helper: an OAuth refresh may wait up to 30 s for pi's
/// `auth.json` lock and 15 s for the provider (the helper gives up after 55 s).
const CREDENTIAL_TIMEOUT_SECS: u32 = 60;

/// How much longer than nono caches it an OAuth token must stay valid.
const TOKEN_VALIDITY_MARGIN_SECS: u32 = 300;

/// Generates the nono profile for `spec`, after validating it.
pub fn generate_profile(spec: &SessionSandboxSpec) -> Result<NonoProfile, ProfileError> {
    validate_spec(spec)?;
    let layout = &spec.layout;
    let backend = spec.approvals.as_ref().map(|a| ApprovalBackend {
        kind: "webhook",
        url: a.hook.url(),
        timeout_secs: a.timeout_secs,
    });

    let mut set_vars = BTreeMap::new();
    set_vars.insert("HOME", layout.home().to_string());
    set_vars.insert("TMPDIR", layout.tmp().to_string());
    set_vars.insert("PI_CODING_AGENT_DIR", layout.agent().to_string());

    Ok(NonoProfile {
        meta: Meta {
            name: format!("cr-{}", layout.id()),
        },
        workdir: Workdir {
            access: "readwrite",
        },
        filesystem: filesystem(spec),
        groups: Groups {
            include: vec!["linux_sysfs_read"],
            exclude: EXCLUDED_GROUPS.to_vec(),
        },
        linux: Linux {
            af_unix_mediation: "pathname",
        },
        environment: Environment {
            deny_vars: DENY_VARS.to_vec(),
            set_vars,
        },
        network: network(spec),
        credential_capture: credential_capture(spec),
        security: backend.clone().map(|backend| Security {
            capability_elevation: true,
            approval_backends: BTreeMap::from([(BACKEND, backend)]),
            approval_defaults: ApprovalDefaults { backend: BACKEND },
        }),
        command_policies: command_policies(spec, backend),
    })
}

/// Path grants of a mediated command sandbox, split by access and kind. Read-write grants
/// appear in both the read and the write lists (nono's `fs_write` is write-only).
struct Grants {
    read_dirs: Vec<String>,
    read_files: Vec<String>,
    write_dirs: Vec<String>,
    write_files: Vec<String>,
}

/// Collects standing grants plus relaxed-level home config for a command sandbox.
fn command_grants(spec: &SessionSandboxSpec) -> Grants {
    let relaxed = spec.active_relaxed_reads();
    let mut g = Grants {
        read_dirs: Vec::new(),
        read_files: Vec::new(),
        write_dirs: Vec::new(),
        write_files: Vec::new(),
    };
    for grant in spec.policy.paths.iter().chain(relaxed) {
        let p = grant.path.to_string();
        let (read, write) = match grant.kind {
            PathKind::Dir => (&mut g.read_dirs, &mut g.write_dirs),
            PathKind::File => (&mut g.read_files, &mut g.write_files),
        };
        read.push(p.clone());
        if grant.access == Access::ReadWrite {
            write.push(p);
        }
    }
    g
}

fn strings(paths: &[AbsPath]) -> Vec<String> {
    paths.iter().map(AbsPath::to_string).collect()
}

fn filesystem(spec: &SessionSandboxSpec) -> Filesystem {
    let layout = &spec.layout;
    let mut read = strings(&spec.read_only);
    let mut allow = strings(&layout.sandbox_dirs());
    let mut allow_file = Vec::new();
    // `read` takes files and directories; read-write grants go to allow/allow_file.
    for grant in spec.policy.paths.iter().chain(spec.active_relaxed_reads()) {
        let target = match (grant.access, grant.kind) {
            (Access::Read, _) => &mut read,
            (Access::ReadWrite, PathKind::Dir) => &mut allow,
            (Access::ReadWrite, PathKind::File) => &mut allow_file,
        };
        target.push(grant.path.to_string());
    }
    Filesystem {
        read,
        allow,
        allow_file,
        unix_socket: spec.daemon_socket.iter().map(AbsPath::to_string).collect(),
        // Extensions (tsx) bind IPC sockets under TMPDIR.
        unix_socket_subtree_bind: vec![layout.tmp().to_string()],
    }
}

/// Domains reachable through the proxy: the policy's domains plus the route upstreams.
fn proxy_domains(spec: &SessionSandboxSpec) -> Vec<String> {
    let mut domains: Vec<String> = spec
        .policy
        .domains
        .iter()
        .chain(
            spec.credential_routes
                .iter()
                .filter_map(|r| r.upstream.allowed_host()),
        )
        .map(|d| d.to_string())
        .collect();
    domains.sort();
    domains.dedup();
    domains
}

fn network(spec: &SessionSandboxSpec) -> Network {
    let allow_domain = proxy_domains(spec);
    if allow_domain.is_empty() && spec.credential_routes.is_empty() {
        return Network {
            block: true,
            ..Network::default()
        };
    }
    let custom_credentials: BTreeMap<String, CustomCredential> = spec
        .credential_routes
        .iter()
        .map(|r| (r.route_id(), custom_credential(r)))
        .collect();
    Network {
        block: false,
        allow_domain,
        credentials: custom_credentials.keys().cloned().collect(),
        custom_credentials,
    }
}

fn custom_credential(route: &CredentialRoute) -> CustomCredential {
    let rules = |endpoints: &[Endpoint]| {
        endpoints
            .iter()
            .map(|e| EndpointRule {
                method: e.method.clone(),
                path: e.path.clone(),
                reason: e.reason.clone(),
            })
            .collect()
    };
    CustomCredential {
        upstream: route.upstream.as_str().to_owned(),
        credential_key: format!("cmd://{}", route.route_id()),
        env_var: route.phantom_env_var(),
        inject_header: route.inject_header.clone(),
        credential_format: route.credential_format.clone(),
        endpoint_policy: EndpointPolicy {
            default: DefaultDecision { decision: "deny" },
            allow: rules(&route.endpoints.allow),
            approve: rules(&route.endpoints.approve),
        },
    }
}

fn credential_capture(spec: &SessionSandboxSpec) -> BTreeMap<String, CredentialCapture> {
    let helper = &spec.credential_helper;
    spec.credential_routes
        .iter()
        .map(|r| {
            let command = vec![
                helper.controlroomd.to_string(),
                "credential".to_owned(),
                "--state-dir".to_owned(),
                helper.state_dir.to_string(),
                "--valid-for-secs".to_owned(),
                (r.cache_ttl_secs + TOKEN_VALIDITY_MARGIN_SECS).to_string(),
                spec.layout.id().to_string(),
                r.provider.to_string(),
            ];
            (
                r.route_id(),
                CredentialCapture {
                    command,
                    timeout_secs: CREDENTIAL_TIMEOUT_SECS,
                    cache_ttl_secs: r.cache_ttl_secs,
                },
            )
        })
        .collect()
}

/// The rules of one command, split by shape.
struct CommandRules {
    default: CommandDecision,
    deny: Vec<Vec<String>>,
    approve: Vec<Vec<String>>,
    run: Vec<Vec<String>>,
}

fn group_rules(rules: &[CommandRule]) -> BTreeMap<&Name, CommandRules> {
    let mut grouped: BTreeMap<&Name, CommandRules> = BTreeMap::new();
    for rule in rules {
        let entry = grouped.entry(&rule.command).or_insert(CommandRules {
            default: CommandDecision::Run,
            deny: Vec::new(),
            approve: Vec::new(),
            run: Vec::new(),
        });
        if rule.args.is_empty() {
            entry.default = rule.decision;
            continue;
        }
        match rule.decision {
            CommandDecision::Deny => entry.deny.push(rule.args.clone()),
            CommandDecision::Approve => entry.approve.push(rule.args.clone()),
            CommandDecision::Run => entry.run.push(rule.args.clone()),
        }
    }
    grouped
}

fn invocation_policy(rules: &CommandRules) -> Option<InvocationPolicy> {
    let to_rules = |windows: &[Vec<String>]| {
        windows
            .iter()
            .map(|w| InvocationRule {
                argv: ArgvContains {
                    contains: w.clone(),
                },
            })
            .collect::<Vec<_>>()
    };
    let default = match rules.default {
        CommandDecision::Approve => "approve",
        _ => "allow",
    };
    let policy = InvocationPolicy {
        default,
        deny: to_rules(&rules.deny),
        approve: to_rules(&rules.approve),
        // Run rules only matter under an approve default.
        allow: if default == "approve" {
            to_rules(&rules.run)
        } else {
            Vec::new()
        },
    };
    (default == "approve" || !policy.deny.is_empty() || !policy.approve.is_empty())
        .then_some(policy)
}

/// The sandbox of a mediated command run by the session: the session's own grants (nono
/// command sandboxes inherit nothing) plus the proxy domains.
fn command_sandbox(spec: &SessionSandboxSpec, command: &str) -> CommandSandbox {
    let layout = &spec.layout;
    let grants = command_grants(spec);
    let session_dirs = [spec.cwd.clone(), layout.home(), layout.tmp()];
    let mut fs_read = strings(&spec.read_only);
    fs_read.extend(strings(&session_dirs));
    fs_read.extend(grants.read_dirs);
    let mut fs_write = strings(&session_dirs);
    fs_write.extend(grants.write_dirs);
    if command == "git" {
        // The object store of a linked worktree lives outside the checkout.
        fs_read.push("@git:common-dir".to_owned());
        fs_write.push("@git:common-dir".to_owned());
    }
    let domains = spec
        .policy
        .domains
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>();
    CommandSandbox {
        exec_paths: strings(&spec.read_only),
        fs_read,
        fs_read_file: grants.read_files,
        fs_write,
        fs_write_file: grants.write_files,
        network: (!domains.is_empty()).then(|| CommandNetwork {
            allow_domain: domains,
            ..CommandNetwork::default()
        }),
        environment: CommandEnvironment {
            allow_vars: vec!["*"],
        },
    }
}

/// The sandbox of `ssh` when run by `git`: SSH config and known hosts from the session HOME
/// and raw TCP to port 22.
fn git_ssh_sandbox(spec: &SessionSandboxSpec) -> CommandSandbox {
    let mut fs_read = strings(&spec.read_only);
    fs_read.extend(strings(&[spec.layout.home(), spec.layout.tmp()]));
    CommandSandbox {
        exec_paths: strings(&spec.read_only),
        fs_read,
        network: Some(CommandNetwork {
            tcp_connect_ports: vec![22],
            ..CommandNetwork::default()
        }),
        environment: CommandEnvironment {
            allow_vars: vec!["*"],
        },
        ..CommandSandbox::default()
    }
}

fn command_policies(
    spec: &SessionSandboxSpec,
    backend: Option<ApprovalBackend>,
) -> Option<CommandPolicies> {
    let grouped = group_rules(&spec.policy.commands);
    let mut commands = BTreeMap::new();
    for (name, rules) in &grouped {
        let Some(pin) = spec.pins.get(*name) else {
            continue;
        };
        let session = match rules.default {
            CommandDecision::Deny => CallerPolicy::Deny("deny"),
            _ => CallerPolicy::Edge(Box::new(CallerEdge {
                sandbox: command_sandbox(spec, name.as_str()),
                invocation_policy: invocation_policy(rules),
            })),
        };
        commands.insert(
            name.to_string(),
            CommandPolicy {
                executable: pin.executable.to_string(),
                can_use: Vec::new(),
                from: BTreeMap::from([("session".to_owned(), session)]),
            },
        );
        for (i, shadow) in pin.shadows.iter().enumerate() {
            commands.insert(
                format!("{name}.shadow{}", i + 1),
                CommandPolicy {
                    executable: shadow.to_string(),
                    can_use: Vec::new(),
                    from: BTreeMap::from([("session".to_owned(), CallerPolicy::Deny("deny"))]),
                },
            );
        }
    }
    for (caller, callee) in CHAINS {
        if commands.contains_key(caller) && commands.contains_key(callee) {
            let sandbox = git_ssh_sandbox(spec);
            commands
                .get_mut(caller)
                .expect("checked above")
                .can_use
                .push(callee.to_owned());
            commands
                .get_mut(callee)
                .expect("checked above")
                .from
                .insert(
                    caller.to_owned(),
                    CallerPolicy::Edge(Box::new(CallerEdge {
                        sandbox,
                        invocation_policy: None,
                    })),
                );
        }
    }
    if commands.is_empty() {
        return None;
    }
    Some(CommandPolicies {
        executable_dirs: strings(&spec.exec_dirs),
        approval_backends: backend
            .map(|b| BTreeMap::from([(BACKEND, b)]))
            .unwrap_or_default(),
        approval_defaults: spec
            .approvals
            .as_ref()
            .map(|_| ApprovalDefaults { backend: BACKEND }),
        commands,
    })
}
