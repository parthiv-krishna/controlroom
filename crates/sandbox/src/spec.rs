//! The typed input of profile generation: everything one session's sandbox depends on.

use std::collections::BTreeMap;

use crate::layout::SessionLayout;
use crate::names::{Domain, Name, NameError};
use crate::paths::AbsPath;
use crate::pins::CommandPin;
use crate::policy::{EffectivePolicy, PathGrant};
use crate::webhook::HookUrl;

/// Everything needed to generate one session's nono profile.
#[derive(Debug, Clone)]
pub struct SessionSandboxSpec {
    /// The session's directories (agent, home, tmp, profile).
    pub layout: SessionLayout,
    /// The working directory (read-write).
    pub cwd: AbsPath,
    /// The merged sandbox policy (level, standing grants, domains, command rules).
    pub policy: EffectivePolicy,
    /// System paths readable by the agent and executable by mediated commands
    /// (e.g. `/nix/store` and the pinned extension bundle).
    pub read_only: Vec<AbsPath>,
    /// Read-only home configuration added at [`crate::SandboxLevel::Relaxed`].
    pub relaxed_reads: Vec<PathGrant>,
    /// The daemon socket the controlroom extension connects to.
    pub daemon_socket: Option<AbsPath>,
    /// Paths no grant may cover (the daemon state dir, the user's real pi agent dir).
    /// The session root is always protected.
    pub protected: Vec<AbsPath>,
    /// Credential routes for the active providers.
    pub credential_routes: Vec<CredentialRoute>,
    /// How nono runs `controlroomd credential` host-side.
    pub credential_helper: CredentialHelper,
    /// The approval webhook; required for approve decisions and capability elevation.
    pub approvals: Option<Approvals>,
    /// Pins for the commands named in `policy.commands`. Commands without a pin are not on
    /// `PATH` and are left unmediated (with mediation active they cannot be executed).
    pub pins: BTreeMap<Name, CommandPin>,
}

impl SessionSandboxSpec {
    /// The relaxed-level reads if the level is relaxed, otherwise nothing.
    pub fn active_relaxed_reads(&self) -> &[PathGrant] {
        if self.policy.level == crate::SandboxLevel::Relaxed {
            &self.relaxed_reads
        } else {
            &[]
        }
    }
}

/// The approval webhook of a session.
#[derive(Debug, Clone)]
pub struct Approvals {
    /// The hook URL.
    pub hook: HookUrl,
    /// How long nono waits for a decision before denying. Long for human waits.
    pub timeout_secs: u32,
}

/// Host-side helper invoked by nono's `credential_capture`.
#[derive(Debug, Clone)]
pub struct CredentialHelper {
    /// Absolute path of `controlroomd`.
    pub controlroomd: AbsPath,
    /// The daemon state directory (passed as `--state-dir`).
    pub state_dir: AbsPath,
}

/// A credential route: nono intercepts traffic to `upstream` and injects the provider's real
/// key, fetched host-side with `controlroomd credential <session> <provider>`.
#[derive(Debug, Clone)]
pub struct CredentialRoute {
    /// The pi provider id.
    pub provider: Name,
    /// The upstream origin.
    pub upstream: Upstream,
    /// Which requests may use the credential. Mandatory and default-deny.
    pub endpoints: EndpointPolicy,
    /// Header to inject into (default `Authorization`).
    pub inject_header: Option<String>,
    /// Header value template (default `Bearer {}` for `Authorization`).
    pub credential_format: Option<String>,
    /// How long nono caches the key.
    pub cache_ttl_secs: u32,
}

impl CredentialRoute {
    /// The nono route name (`cr_<provider>`), distinctive so nono's auto-exported
    /// `<ROUTE>_BASE_URL` variable cannot collide with variables pi reads.
    pub fn route_id(&self) -> String {
        format!(
            "cr_{}",
            env_safe(self.provider.as_str()).to_ascii_lowercase()
        )
    }

    /// The phantom-token variable (`CR_<PROVIDER>_PHANTOM`).
    pub fn phantom_env_var(&self) -> String {
        format!(
            "CR_{}_PHANTOM",
            env_safe(self.provider.as_str()).to_ascii_uppercase()
        )
    }
}

fn env_safe(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// An upstream origin: `https://host[:port]`, or plain HTTP to a loopback port (local mock
/// servers in tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    origin: String,
    host: Option<Domain>,
}

/// Why an upstream URL was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpstreamError {
    /// Not an origin of the accepted forms.
    #[error("invalid upstream {0:?}: expected https://host[:port] or http://127.0.0.1:port")]
    Malformed(String),
    /// The host is not a valid domain.
    #[error(transparent)]
    Host(#[from] NameError),
}

impl Upstream {
    /// Parses an origin.
    pub fn new(s: &str) -> Result<Self, UpstreamError> {
        let malformed = || UpstreamError::Malformed(s.to_owned());
        if let Some(rest) = s.strip_prefix("https://") {
            let (host, port) = match rest.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (rest, None),
            };
            if port.is_some_and(|p| p.parse::<u16>().is_err()) {
                return Err(malformed());
            }
            let host = Domain::new(host)?;
            return Ok(Self {
                origin: s.to_ascii_lowercase(),
                host: Some(host),
            });
        }
        let rest = s.strip_prefix("http://").ok_or_else(malformed)?;
        let (host, port) = rest.split_once(':').ok_or_else(malformed)?;
        if !matches!(host, "127.0.0.1" | "localhost") || port.parse::<u16>().is_err() {
            return Err(malformed());
        }
        Ok(Self {
            origin: s.to_owned(),
            host: None,
        })
    }

    /// The origin string.
    pub fn as_str(&self) -> &str {
        &self.origin
    }

    /// The host to allow through the proxy; `None` for loopback upstreams.
    pub fn allowed_host(&self) -> Option<&Domain> {
        self.host.as_ref()
    }
}

/// A route's endpoint policy: everything not listed is denied.
#[derive(Debug, Clone, Default)]
pub struct EndpointPolicy {
    /// Requests forwarded with the credential.
    pub allow: Vec<Endpoint>,
    /// Requests that need user approval first.
    pub approve: Vec<Endpoint>,
}

/// One `METHOD path-glob` rule (`*` matches one segment, `**` any number).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Uppercase HTTP method or `*`.
    pub method: String,
    /// Absolute path glob.
    pub path: String,
    /// Shown in the approval request.
    pub reason: Option<String>,
}

impl Endpoint {
    /// A rule without a reason.
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            method: method.to_owned(),
            path: path.to_owned(),
            reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstreams() {
        let u = Upstream::new("https://api.example.com").unwrap();
        assert_eq!(u.allowed_host().unwrap().as_str(), "api.example.com");
        assert!(Upstream::new("https://api.example.com:8443").is_ok());
        let l = Upstream::new("http://127.0.0.1:8080").unwrap();
        assert!(l.allowed_host().is_none());
        for bad in [
            "http://api.example.com",
            "https://api.example.com/v1",
            "https://*.example.com",
            "ftp://x",
            "http://127.0.0.1",
            "https://api.example.com:x",
        ] {
            assert!(Upstream::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn route_names_are_env_safe() {
        let r = CredentialRoute {
            provider: Name::new("my-gateway.v2").unwrap(),
            upstream: Upstream::new("https://gw.example.com").unwrap(),
            endpoints: EndpointPolicy::default(),
            inject_header: None,
            credential_format: None,
            cache_ttl_secs: 300,
        };
        assert_eq!(r.route_id(), "cr_my_gateway_v2");
        assert_eq!(r.phantom_env_var(), "CR_MY_GATEWAY_V2_PHANTOM");
    }
}
