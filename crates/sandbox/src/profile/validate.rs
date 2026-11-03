//! Validation of generation inputs and of profile JSON.
//!
//! [`validate_spec`] rejects specs that would produce an unsafe or unlaunchable profile.
//! [`check_profile_json`] re-checks the invariants on profile JSON as it will be handed to
//! nono, so a profile read back from disk (or a future generator bug) cannot reference
//! registry packs, extend anything but built-ins, or route credentials without an endpoint
//! policy.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::paths::AbsPath;
use crate::policy::{Access, CommandDecision, SandboxLevel};
use crate::spec::{Endpoint, SessionSandboxSpec};

/// Why a spec or profile was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// Level `off` runs without nono; there is no profile to generate.
    #[error("sandbox level is off; no profile is generated")]
    SandboxOff,
    /// The working directory is `/`.
    #[error("the working directory must not be /")]
    RootWorkdir,
    /// A grant would expose a protected path (state dir, real agent dir, session root).
    #[error("grant {grant} covers protected path {protected}")]
    CoversProtected {
        /// The offending grant.
        grant: AbsPath,
        /// The protected path it covers.
        protected: AbsPath,
    },
    /// An executable directory is writable by the session (nono refuses it: the session could
    /// plant programs there).
    #[error("executable directory {dir} is writable inside the sandbox (under {writable})")]
    WritableExecDir {
        /// The executable directory.
        dir: AbsPath,
        /// The writable directory it is in.
        writable: AbsPath,
    },
    /// A credential route has no allow or approve rules.
    #[error("credential route for {0} has no endpoint policy")]
    MissingEndpointPolicy(String),
    /// A credential route allows every endpoint.
    #[error("endpoint rule {method} {path} of route {route} allows everything")]
    EndpointTooBroad {
        /// Route id.
        route: String,
        /// Method of the rule.
        method: String,
        /// Path of the rule.
        path: String,
    },
    /// An endpoint rule is malformed.
    #[error("invalid endpoint rule {method:?} {path:?} on route {route}")]
    InvalidEndpoint {
        /// Route id.
        route: String,
        /// Method of the rule.
        method: String,
        /// Path of the rule.
        path: String,
    },
    /// Two providers map to the same route id.
    #[error("duplicate credential route {0}")]
    DuplicateRoute(String),
    /// A header name is not an HTTP token.
    #[error("invalid inject header {0:?}")]
    InvalidHeader(String),
    /// nono caches credentials for at most an hour.
    #[error("cache_ttl_secs {0} exceeds nono's maximum of 3600")]
    CacheTtl(u32),
    /// An approve decision exists but no webhook is configured.
    #[error("approval rules need an approval webhook")]
    ApprovalsRequired,
    /// The approval timeout is zero.
    #[error("approval timeout must be at least one second")]
    ApprovalTimeout,
    /// nono only activates endpoint approval backends when command mediation is active.
    #[error("endpoint approvals need at least one mediated command")]
    EndpointApprovalsNeedMediation,
    /// Profile JSON violates an invariant.
    #[error("profile {field}: {problem}")]
    Invalid {
        /// The JSON field.
        field: String,
        /// What is wrong.
        problem: String,
    },
}

const MAX_CACHE_TTL_SECS: u32 = 3600;

/// Checks a spec before generation.
pub fn validate_spec(spec: &SessionSandboxSpec) -> Result<(), ProfileError> {
    if spec.policy.level == SandboxLevel::Off {
        return Err(ProfileError::SandboxOff);
    }
    if spec.cwd.as_str() == "/" {
        return Err(ProfileError::RootWorkdir);
    }
    check_protected(spec)?;
    check_exec_dirs(spec)?;
    check_routes(spec)?;
    check_approvals(spec)
}

fn check_protected(spec: &SessionSandboxSpec) -> Result<(), ProfileError> {
    let protected: Vec<&AbsPath> = spec
        .protected
        .iter()
        .chain(std::iter::once(spec.layout.root()))
        .collect();
    let relaxed = spec.active_relaxed_reads();
    let grants = std::iter::once(&spec.cwd)
        .chain(&spec.read_only)
        .chain(&spec.exec_dirs)
        .chain(spec.policy.paths.iter().map(|g| &g.path))
        .chain(relaxed.iter().map(|g| &g.path));
    for grant in grants {
        if let Some(p) = protected.iter().find(|p| grant.contains(p)) {
            return Err(ProfileError::CoversProtected {
                grant: grant.clone(),
                protected: (*p).clone(),
            });
        }
    }
    Ok(())
}

fn check_exec_dirs(spec: &SessionSandboxSpec) -> Result<(), ProfileError> {
    let writable_grants = spec
        .policy
        .paths
        .iter()
        .filter(|g| g.access == Access::ReadWrite)
        .map(|g| g.path.clone());
    let writable: Vec<AbsPath> = std::iter::once(spec.cwd.clone())
        .chain(spec.layout.sandbox_dirs())
        .chain(writable_grants)
        .collect();
    for dir in &spec.exec_dirs {
        if let Some(w) = writable.iter().find(|w| w.contains(dir)) {
            return Err(ProfileError::WritableExecDir {
                dir: dir.clone(),
                writable: w.clone(),
            });
        }
    }
    Ok(())
}

fn check_routes(spec: &SessionSandboxSpec) -> Result<(), ProfileError> {
    let mut seen = BTreeSet::new();
    for route in &spec.credential_routes {
        let id = route.route_id();
        if !seen.insert(id.clone()) {
            return Err(ProfileError::DuplicateRoute(id));
        }
        let rules: Vec<&Endpoint> = route
            .endpoints
            .allow
            .iter()
            .chain(&route.endpoints.approve)
            .collect();
        if rules.is_empty() {
            return Err(ProfileError::MissingEndpointPolicy(
                route.provider.to_string(),
            ));
        }
        for e in rules {
            check_endpoint(&id, e)?;
        }
        if let Some(h) = &route.inject_header
            && !is_http_token(h)
        {
            return Err(ProfileError::InvalidHeader(h.clone()));
        }
        if route.cache_ttl_secs > MAX_CACHE_TTL_SECS {
            return Err(ProfileError::CacheTtl(route.cache_ttl_secs));
        }
    }
    Ok(())
}

fn check_endpoint(route: &str, e: &Endpoint) -> Result<(), ProfileError> {
    let method_ok = e.method == "*"
        || (!e.method.is_empty() && e.method.bytes().all(|b| b.is_ascii_uppercase()));
    let path_ok = e.path.starts_with('/') && !e.path.chars().any(char::is_whitespace);
    if !method_ok || !path_ok {
        return Err(ProfileError::InvalidEndpoint {
            route: route.to_owned(),
            method: e.method.clone(),
            path: e.path.clone(),
        });
    }
    let catch_all = e.path.trim_start_matches('/').split('/').all(|s| s == "**");
    if catch_all {
        return Err(ProfileError::EndpointTooBroad {
            route: route.to_owned(),
            method: e.method.clone(),
            path: e.path.clone(),
        });
    }
    Ok(())
}

fn is_http_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn check_approvals(spec: &SessionSandboxSpec) -> Result<(), ProfileError> {
    let mediated = spec
        .policy
        .commands
        .iter()
        .any(|r| spec.pins.contains_key(&r.command));
    let command_approvals = spec
        .policy
        .commands
        .iter()
        .any(|r| r.decision == CommandDecision::Approve && spec.pins.contains_key(&r.command));
    let endpoint_approvals = spec
        .credential_routes
        .iter()
        .any(|r| !r.endpoints.approve.is_empty());
    match &spec.approvals {
        None if command_approvals || endpoint_approvals => Err(ProfileError::ApprovalsRequired),
        Some(a) if a.timeout_secs == 0 => Err(ProfileError::ApprovalTimeout),
        _ if endpoint_approvals && !mediated => Err(ProfileError::EndpointApprovalsNeedMediation),
        _ => Ok(()),
    }
}

/// Checks profile JSON as handed to nono.
///
/// - no `packs`, `network_profile`, hooks or other indirection;
/// - `extends` only names built-in profiles (a plain name, never `ns/name` or a path);
/// - every filesystem entry is an absolute path;
/// - `allow_domain` entries are plain hostnames (no wildcards, no endpoint objects);
/// - every custom credential has an endpoint policy whose default is `deny` and which lists
///   at least one rule.
pub fn check_profile_json(profile: &Value) -> Result<(), ProfileError> {
    let invalid = |field: &str, problem: &str| ProfileError::Invalid {
        field: field.to_owned(),
        problem: problem.to_owned(),
    };
    let obj = profile
        .as_object()
        .ok_or_else(|| invalid("(root)", "not an object"))?;

    for key in [
        "packs",
        "hooks",
        "session_hooks",
        "command_args",
        "binary",
        "platform_overrides",
        "credential_providers",
        "credential_routes",
        "env_credentials",
    ] {
        if obj.contains_key(key) {
            return Err(invalid(key, "not allowed in controlroom profiles"));
        }
    }
    if let Some(ext) = obj.get("extends") {
        let names: Vec<&Value> = match ext {
            Value::Array(a) => a.iter().collect(),
            Value::Null => Vec::new(),
            v => vec![v],
        };
        for name in names {
            let builtin = name.as_str().is_some_and(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            });
            if !builtin {
                return Err(invalid(
                    "extends",
                    "only built-in profile names are allowed",
                ));
            }
        }
    }

    let network = obj.get("network").and_then(Value::as_object);
    if network.is_some_and(|n| n.get("network_profile").is_some_and(|v| !v.is_null())) {
        return Err(invalid("network.network_profile", "not allowed"));
    }

    if let Some(fs) = obj.get("filesystem").and_then(Value::as_object) {
        for (field, entries) in fs {
            for entry in entries.as_array().into_iter().flatten() {
                let path = entry
                    .as_str()
                    .or_else(|| entry.get("path").and_then(Value::as_str));
                if !path.is_some_and(|p| AbsPath::new(p).is_ok()) {
                    return Err(invalid(
                        &format!("filesystem.{field}"),
                        "entries must be absolute paths",
                    ));
                }
            }
        }
    }

    let Some(network) = network else {
        return Ok(());
    };
    for d in network
        .get("allow_domain")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if !d
            .as_str()
            .is_some_and(|s| crate::names::Domain::new(s).is_ok())
        {
            return Err(invalid(
                "network.allow_domain",
                "entries must be plain hostnames",
            ));
        }
    }
    for (name, cred) in network
        .get("custom_credentials")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let field = format!("network.custom_credentials.{name}.endpoint_policy");
        let policy = cred
            .get("endpoint_policy")
            .ok_or_else(|| invalid(&field, "missing"))?;
        let default = policy.get("default");
        let default_deny = default.and_then(Value::as_str) == Some("deny")
            || default
                .and_then(|d| d.get("decision"))
                .and_then(Value::as_str)
                == Some("deny");
        if !default_deny {
            return Err(invalid(&field, "default must be deny"));
        }
        let has_rules = ["allow", "approve"].iter().any(|k| {
            policy
                .get(k)
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
        });
        if !has_rules {
            return Err(invalid(
                &field,
                "must list at least one allow or approve rule",
            ));
        }
    }
    Ok(())
}
