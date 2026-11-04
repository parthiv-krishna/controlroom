//! Golden tests of generated profiles, plus spec and profile validation.
//!
//! Regenerate the golden files with `UPDATE_GOLDEN=1 cargo test -p controlroom-sandbox`.

mod common;

use std::path::PathBuf;

use common::*;
use controlroom_sandbox::policy::{Access, CommandDecision, PathGrant, PathKind, SandboxLevel};
use controlroom_sandbox::profile::{
    check_profile_json, generate_profile, generate_workspace_profile,
};
use controlroom_sandbox::spec::{Endpoint, EndpointPolicy};
use controlroom_sandbox::{ProfileError, SessionSandboxSpec};
use serde_json::json;

fn golden(name: &str, spec: &SessionSandboxSpec) {
    let profile = generate_profile(spec).expect("profile generates");
    check_profile_json(&profile.to_value()).expect("generated profile passes the check");
    let text = profile.to_json_pretty();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &text).expect("write golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display()));
    assert_eq!(text, expected, "{name} differs from {}", path.display());
}

#[test]
fn golden_strict_minimal() {
    golden(
        "strict-minimal",
        &minimal(
            &p("/home/alice/.local/share/controlroom"),
            &p("/home/alice/project"),
        ),
    );
}

#[test]
fn golden_strict_full() {
    golden("strict-full", &full());
}

#[test]
fn golden_relaxed() {
    let mut spec = full();
    spec.policy.level = SandboxLevel::Relaxed;
    spec.relaxed_reads = vec![
        PathGrant {
            path: p("/home/alice/.gitconfig"),
            kind: PathKind::File,
            access: Access::Read,
        },
        PathGrant {
            path: p("/home/alice/.config/git"),
            kind: PathKind::Dir,
            access: Access::Read,
        },
    ];
    spec.credential_routes.truncate(1);
    spec.pins.retain(|n, _| n.as_str() == "git");
    golden("relaxed", &spec);
}

#[test]
fn relaxed_reads_are_ignored_when_strict() {
    let mut spec = full();
    spec.relaxed_reads = vec![PathGrant {
        path: p("/home/alice/.gitconfig"),
        kind: PathKind::File,
        access: Access::Read,
    }];
    let text = generate_profile(&spec).unwrap().to_json_pretty();
    assert!(!text.contains(".gitconfig"));
}

#[test]
fn level_off_has_no_profile() {
    let mut spec = full();
    spec.policy.level = SandboxLevel::Off;
    assert_eq!(generate_profile(&spec), Err(ProfileError::SandboxOff));
}

#[test]
fn grants_must_not_cover_protected_paths() {
    type Mutation = fn(&mut SessionSandboxSpec);
    let cases: [(&str, Mutation); 4] = [
        ("cwd above the state dir", |s| s.cwd = p("/home/alice")),
        ("read-only home", |s| s.read_only.push(p("/home/alice"))),
        ("grant on the real agent dir", |s| {
            s.policy.paths.push(PathGrant {
                path: p("/home/alice/.pi"),
                kind: PathKind::Dir,
                access: Access::Read,
            })
        }),
        ("grant on the session root", |s| {
            s.policy.paths.push(PathGrant {
                path: s.layout.root().clone(),
                kind: PathKind::Dir,
                access: Access::Read,
            })
        }),
    ];
    for (what, mutate) in cases {
        let mut spec = full();
        mutate(&mut spec);
        assert!(
            matches!(
                generate_profile(&spec),
                Err(ProfileError::CoversProtected { .. })
            ),
            "{what}"
        );
    }
    let mut spec = full();
    spec.cwd = p("/");
    assert_eq!(generate_profile(&spec), Err(ProfileError::RootWorkdir));
}

#[test]
fn exec_dirs_must_not_be_writable_in_the_sandbox() {
    type Mutation = fn(&mut SessionSandboxSpec);
    let cases: [(&str, Mutation); 3] = [
        ("a tool dir in cwd", |s| {
            s.exec_dirs.push(p("/home/alice/project/node_modules/.bin"))
        }),
        ("the session home", |s| {
            let home = s.layout.home();
            s.exec_dirs.push(home.join("bin"))
        }),
        ("a read-write grant", |s| {
            s.exec_dirs.push(p("/home/alice/shared/bin"))
        }),
    ];
    for (what, mutate) in cases {
        let mut spec = full();
        mutate(&mut spec);
        assert!(
            matches!(
                generate_profile(&spec),
                Err(ProfileError::WritableExecDir { .. })
            ),
            "{what}"
        );
    }
    // A read-only grant is fine.
    let mut spec = full();
    spec.exec_dirs.push(p("/home/alice/datasets/bin"));
    let profile = generate_profile(&spec)
        .expect("read-only exec dir")
        .to_value();
    assert_eq!(
        profile["command_policies"]["executable_dirs"],
        json!(["/usr/bin", "/bin", "/home/alice/datasets/bin"])
    );
}

#[test]
fn endpoint_policies_are_mandatory_and_narrow() {
    let mut spec = full();
    spec.credential_routes[0].endpoints = EndpointPolicy::default();
    assert!(matches!(
        generate_profile(&spec),
        Err(ProfileError::MissingEndpointPolicy(_))
    ));

    for (method, path) in [("*", "/**"), ("GET", "/**/**")] {
        let mut spec = full();
        spec.credential_routes[0].endpoints.allow = vec![Endpoint::new(method, path)];
        assert!(
            matches!(
                generate_profile(&spec),
                Err(ProfileError::EndpointTooBroad { .. })
            ),
            "{method} {path}"
        );
    }
    for (method, path) in [("get", "/x"), ("GET", "x"), ("", "/x"), ("GET", "/a b")] {
        let mut spec = full();
        spec.credential_routes[0].endpoints.allow = vec![Endpoint::new(method, path)];
        assert!(
            matches!(
                generate_profile(&spec),
                Err(ProfileError::InvalidEndpoint { .. })
            ),
            "{method} {path}"
        );
    }

    let mut spec = full();
    spec.credential_routes.push(route(
        "sub",
        "https://other.example.com",
        EndpointPolicy {
            allow: vec![Endpoint::new("GET", "/x")],
            approve: Vec::new(),
        },
    ));
    assert!(matches!(
        generate_profile(&spec),
        Err(ProfileError::DuplicateRoute(_))
    ));

    let mut spec = full();
    spec.credential_routes[0].inject_header = Some("Bad Header".into());
    assert!(matches!(
        generate_profile(&spec),
        Err(ProfileError::InvalidHeader(_))
    ));

    let mut spec = full();
    spec.credential_routes[0].cache_ttl_secs = 3601;
    assert_eq!(generate_profile(&spec), Err(ProfileError::CacheTtl(3601)));
}

#[test]
fn approvals_need_a_webhook_and_endpoint_approvals_need_mediation() {
    let mut spec = full();
    spec.approvals = None;
    assert_eq!(
        generate_profile(&spec),
        Err(ProfileError::ApprovalsRequired)
    );

    // Without approve rules, no webhook is fine.
    spec.credential_routes[0].endpoints.approve.clear();
    spec.policy
        .commands
        .retain(|r| r.decision != CommandDecision::Approve);
    let profile = generate_profile(&spec).unwrap().to_value();
    assert!(profile.get("security").is_none());
    assert!(
        profile["command_policies"]
            .get("approval_backends")
            .is_none()
    );

    let mut spec = full();
    spec.pins.clear();
    assert_eq!(
        generate_profile(&spec),
        Err(ProfileError::EndpointApprovalsNeedMediation)
    );

    let mut spec = full();
    spec.approvals.as_mut().unwrap().timeout_secs = 0;
    assert_eq!(generate_profile(&spec), Err(ProfileError::ApprovalTimeout));
}

#[test]
fn unpinned_commands_are_not_mediated() {
    let mut spec = full();
    spec.pins.retain(|n, _| n.as_str() != "kubectl");
    let profile = generate_profile(&spec).unwrap().to_value();
    assert!(
        profile["command_policies"]["commands"]
            .get("kubectl")
            .is_none()
    );
}

#[test]
fn profile_json_check_rejects_indirection() {
    let ok = generate_profile(&full()).unwrap().to_value();
    check_profile_json(&ok).unwrap();

    let with = |patch: serde_json::Value| {
        let mut v = ok.clone();
        for (k, val) in patch.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    };
    for bad in [
        json!({"extends": "acme/pi"}),
        json!({"extends": ["default", "./other.json"]}),
        json!({"extends": "/abs/profile.json"}),
        json!({"packs": ["acme/pi"]}),
        json!({"hooks": {}}),
        json!({"filesystem": {"read": ["relative/dir"]}}),
        json!({"filesystem": {"allow": ["~/x"]}}),
        json!({"network": {"network_profile": "dev"}}),
        json!({"network": {"allow_domain": ["*.example.com"]}}),
        json!({"network": {"custom_credentials": {"cr_x": {"upstream": "https://x.example.com"}}}}),
        json!({"network": {"custom_credentials": {"cr_x": {"endpoint_policy": {"default": "allow", "allow": [{"method": "GET", "path": "/"}]}}}}}),
        json!({"network": {"custom_credentials": {"cr_x": {"endpoint_policy": {"default": {"decision": "deny"}}}}}}),
    ] {
        assert!(check_profile_json(&with(bad.clone())).is_err(), "{bad}");
    }
    assert!(check_profile_json(&with(json!({"extends": "default"}))).is_ok());
}

#[test]
fn golden_workspace() {
    let spec = workspace();
    let profile = generate_workspace_profile(&spec).expect("profile generates");
    check_profile_json(&profile.to_value()).expect("generated profile passes the check");
    let text = profile.to_json_pretty();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/workspace.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &text).expect("write golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display()));
    assert_eq!(text, expected);
}

#[test]
fn a_workspace_keeps_home_and_has_no_agent_dirs_or_credentials() {
    let profile = generate_workspace_profile(&workspace()).unwrap().to_value();
    let vars = &profile["environment"]["set_vars"];
    assert!(vars.get("HOME").is_none(), "{vars}");
    assert!(vars.get("PI_CODING_AGENT_DIR").is_none());
    assert!(vars["TMPDIR"].as_str().unwrap().ends_with("/tmp"));
    assert!(profile.get("credential_capture").is_none());
    assert!(profile["filesystem"].get("unix_socket").is_none());
    // The workspace's own dir (profile, webhook secret) can't be granted.
    let mut spec = workspace();
    spec.policy.paths.push(PathGrant {
        path: spec.dir.clone(),
        kind: PathKind::Dir,
        access: Access::Read,
    });
    assert!(matches!(
        generate_workspace_profile(&spec),
        Err(ProfileError::CoversProtected { .. })
    ));
    let mut spec = workspace();
    spec.policy.level = SandboxLevel::Off;
    assert_eq!(
        generate_workspace_profile(&spec),
        Err(ProfileError::SandboxOff)
    );
}
