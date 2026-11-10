//! Spec builders shared by the golden and nono integration tests.

#![allow(dead_code)]

use std::collections::BTreeMap;

use controlroom_sandbox::pins::CommandPin;
use controlroom_sandbox::policy::{
    Access, CommandDecision, CommandRule, PathGrant, PathKind, ScopePolicy, default_command_rules,
};
use controlroom_sandbox::spec::{
    Approvals, CredentialHelper, CredentialRoute, Endpoint, EndpointPolicy, SessionSandboxSpec,
    Upstream,
};
use controlroom_sandbox::webhook::{HookSecret, HookUrl};
use controlroom_sandbox::workspace::WorkspaceSandboxSpec;
use controlroom_sandbox::{AbsPath, Domain, Name, SessionId, SessionLayout, effective_policy};

pub const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

pub fn p(s: &str) -> AbsPath {
    AbsPath::new(s).expect("valid test path")
}

pub fn name(s: &str) -> Name {
    Name::new(s).expect("valid test name")
}

pub fn session() -> SessionId {
    SessionId::new(SESSION).expect("valid test session id")
}

pub fn rule(command: &str, args: &[&str], decision: CommandDecision) -> CommandRule {
    CommandRule {
        command: name(command),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        decision,
    }
}

pub fn route(provider: &str, upstream: &str, endpoints: EndpointPolicy) -> CredentialRoute {
    CredentialRoute {
        provider: name(provider),
        upstream: Upstream::new(upstream).expect("valid upstream"),
        endpoints,
        inject_header: None,
        credential_format: None,
        cache_ttl_secs: 300,
    }
}

/// A fixed secret so golden files are stable.
pub fn fixed_secret() -> HookSecret {
    HookSecret::parse(&"ab".repeat(32)).expect("valid secret")
}

/// The smallest strict spec: no network, no approvals, no mediated commands.
pub fn minimal(state: &AbsPath, cwd: &AbsPath) -> SessionSandboxSpec {
    let layout = SessionLayout::new(state, &session());
    SessionSandboxSpec {
        layout,
        cwd: cwd.clone(),
        policy: effective_policy([], &ScopePolicy::default()),
        read_only: vec![p("/nix/store")],
        relaxed_reads: Vec::new(),
        exec_dirs: Vec::new(),
        daemon_socket: None,
        protected: vec![state.clone()],
        credential_routes: Vec::new(),
        credential_helper: CredentialHelper {
            controlroomd: p("/nix/store/aaaa-controlroomd/bin/controlroomd"),
            state_dir: state.clone(),
        },
        approvals: None,
        pins: BTreeMap::new(),
    }
}

/// A strict spec exercising every feature, with fake store paths.
pub fn full() -> SessionSandboxSpec {
    let state = p("/home/alice/.local/share/controlroom");
    let mut spec = minimal(&state, &p("/home/alice/project"));
    let machine = ScopePolicy {
        level: None,
        paths: vec![
            PathGrant {
                path: p("/home/alice/datasets"),
                kind: PathKind::Dir,
                access: Access::Read,
            },
            PathGrant {
                path: p("/home/alice/notes.md"),
                kind: PathKind::File,
                access: Access::ReadWrite,
            },
        ],
        domains: vec![Domain::new("registry.example.com").expect("domain")],
        sockets: Vec::new(),
        commands: default_command_rules(),
    };
    let project = ScopePolicy {
        level: None,
        paths: vec![PathGrant {
            path: p("/home/alice/shared"),
            kind: PathKind::Dir,
            access: Access::ReadWrite,
        }],
        domains: vec![Domain::new("git.example.com").expect("domain")],
        sockets: Vec::new(),
        commands: vec![
            rule("kubectl", &["apply"], CommandDecision::Approve),
            rule("kubectl", &["delete"], CommandDecision::Deny),
        ],
    };
    spec.policy = effective_policy([&machine, &project], &ScopePolicy::default());
    spec.read_only
        .push(p("/nix/store/bbbb-controlroom-pi-bundle"));
    spec.exec_dirs = vec![p("/usr/bin"), p("/bin")];
    spec.daemon_socket = Some(state.join("daemon.sock"));
    spec.protected.push(p("/home/alice/.pi/agent"));
    spec.credential_routes = vec![
        route(
            "gateway",
            "https://llm.example.com",
            EndpointPolicy {
                allow: vec![Endpoint::new("POST", "/v1/chat/completions")],
                approve: vec![Endpoint {
                    method: "GET".into(),
                    path: "/model/info".into(),
                    reason: Some("model discovery".into()),
                }],
            },
        ),
        route(
            "sub",
            "https://api.example.com",
            EndpointPolicy {
                allow: vec![Endpoint::new("POST", "/v1/responses")],
                approve: Vec::new(),
            },
        ),
    ];
    spec.approvals = Some(Approvals {
        hook: HookUrl {
            port: 47100,
            session: session(),
            secret: fixed_secret(),
        },
        timeout_secs: 600,
    });
    let pin = |exe: &str, shadows: &[&str]| CommandPin {
        executable: p(exe),
        shadows: shadows.iter().map(|s| p(s)).collect(),
    };
    spec.pins = BTreeMap::from([
        (
            name("git"),
            pin(
                "/nix/store/cccc-git-2.55.0/bin/git",
                &["/nix/store/dddd-git-minimal-2.55.0/bin/git"],
            ),
        ),
        (
            name("ssh"),
            pin("/nix/store/eeee-openssh-10.5/bin/ssh", &[]),
        ),
        (
            name("scp"),
            pin("/nix/store/eeee-openssh-10.5/bin/scp", &[]),
        ),
        (name("gh"), pin("/nix/store/ffff-gh-2.80/bin/gh", &[])),
        (
            name("kubectl"),
            pin("/nix/store/gggg-kubectl-1.34/bin/kubectl", &[]),
        ),
    ]);
    spec
}

/// A workspace on a host at level strict, with the default mediated commands and approvals.
pub fn workspace() -> WorkspaceSandboxSpec {
    let agent = full();
    let dir = p("/home/alice/.local/state/controlroom/workspaces").join(SESSION);
    WorkspaceSandboxSpec {
        id: session(),
        root: p("/home/alice/src/api"),
        tmp: dir.join("tmp"),
        read_only: vec![p("/nix/store"), dir.join("view")],
        dir,
        policy: agent.policy,
        relaxed_reads: Vec::new(),
        exec_dirs: vec![p("/usr/bin"), p("/bin")],
        ssh_config: vec![p("/home/alice/.ssh")],
        protected: vec![
            p("/home/alice/.local/state/controlroom"),
            p("/home/alice/.local/share/controlroom"),
        ],
        nono_denied: controlroom_sandbox::limits::denied(&p("/home/alice")),
        approvals: agent.approvals,
        pins: agent.pins,
    }
}
