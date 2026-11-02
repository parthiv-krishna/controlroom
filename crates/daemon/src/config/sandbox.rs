//! `sandbox.toml`: machine-local inputs of sandboxed launches (spec §11).
//!
//! The *policy* (level, standing grants, domains, mediated commands) comes from the controller
//! per session; this file holds what only the execution host knows. Every key is optional:
//!
//! ```toml
//! nono = "nono"                     # program found by the login shell, or an absolute path
//! pi_agent_dir = "~/.pi/agent"      # the user's real pi agent dir (default: $PI_CODING_AGENT_DIR or ~/.pi/agent)
//! keep = ["npm:@controlroom/pi"]    # pi resources (packages, extensions, ...) kept in sessions
//! copy = ["AGENTS.md"]              # extra files copied from the real agent dir
//! npm = "/nix/store/…-bundle"       # target of the session's agent/npm link
//! tmp_seed = "/nix/store/…-jiti"    # copied into a new session's TMPDIR (warm jiti cache)
//! read_only = ["/nix/store"]        # readable system paths
//! relaxed_reads = [{ path = "~/.gitconfig", kind = "file" }]  # level relaxed only
//! approval_timeout_secs = 600
//! nono_tmp = "/tmp"                 # nono's own TMPDIR; short, because nono binds sockets in it
//! allow_loopback_upstreams = false  # tests only: see below
//!
//! [oauth_claims]
//! some-provider = ["account_id"]    # claims carried by the synthetic sandbox JWT
//!
//! [[routes]]                        # one credential route per API-key provider
//! provider = "gateway"
//! upstream = "https://llm.example.com"
//! allow = ["POST /v1/chat/completions"]
//! approve = ["GET /model/info"]
//! ```
//!
//! **Loopback upstreams** (`http://127.0.0.1:<port>`) are refused unless
//! `allow_loopback_upstreams = true`: with such a route, nono 0.80 stops excluding loopback
//! from its forward proxy, and the sandbox can then reach *every* HTTP service on the host's
//! loopback interface through it (verified on nono 0.80.0). They exist for local mock models.

use std::collections::BTreeMap;

use controlroom_sandbox::agent_dir::AgentDirSpec;
use controlroom_sandbox::policy::{Access, PathGrant, PathKind};
use controlroom_sandbox::spec::{CredentialRoute, Endpoint, EndpointPolicy, Upstream};
use controlroom_sandbox::{AbsPath, Name};
use serde::Deserialize;

use super::{Loaded, load_toml};
use crate::dirs::{Dirs, expand_home};

/// The contents of `sandbox.toml`, paths unexpanded.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// nono: a program name resolved by the login shell, or an absolute (or `~/`) path.
    pub nono: String,
    /// The user's real pi agent dir.
    pub pi_agent_dir: Option<String>,
    /// Resource sources to keep in session settings.
    pub keep: Vec<String>,
    /// Extra entries copied from the real agent dir.
    pub copy: Vec<String>,
    /// Target of `agent/npm`.
    pub npm: Option<String>,
    /// Seed for the session TMPDIR.
    pub tmp_seed: Option<String>,
    /// Readable system paths.
    pub read_only: Vec<String>,
    /// Read-only home config added at level relaxed.
    pub relaxed_reads: Vec<RelaxedRead>,
    /// How long nono waits for an approval before denying.
    pub approval_timeout_secs: u32,
    /// nono's TMPDIR.
    pub nono_tmp: String,
    /// Accept `http://127.0.0.1:<port>` route upstreams (tests with a local mock model).
    pub allow_loopback_upstreams: bool,
    /// Claims of each OAuth provider's synthetic token.
    pub oauth_claims: BTreeMap<String, Vec<String>>,
    /// Credential routes.
    pub routes: Vec<RouteConfig>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            nono: "nono".to_owned(),
            pi_agent_dir: None,
            keep: Vec::new(),
            copy: Vec::new(),
            npm: None,
            tmp_seed: None,
            read_only: vec!["/nix/store".to_owned()],
            relaxed_reads: Vec::new(),
            approval_timeout_secs: 600,
            nono_tmp: "/tmp".to_owned(),
            allow_loopback_upstreams: false,
            oauth_claims: BTreeMap::new(),
            routes: Vec::new(),
        }
    }
}

/// A read-only path for level relaxed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaxedRead {
    /// The path (`~/` allowed).
    pub path: String,
    /// Directory (recursive) or file.
    pub kind: PathKind,
}

/// A credential route for one API-key provider.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    /// The pi provider id.
    pub provider: String,
    /// `https://host[:port]`, or `http://127.0.0.1:<port>` for a local server.
    pub upstream: String,
    /// `METHOD /path-glob` rules forwarded with the credential.
    #[serde(default)]
    pub allow: Vec<String>,
    /// `METHOD /path-glob` rules that need the user's approval.
    #[serde(default)]
    pub approve: Vec<String>,
    /// Header to inject into (default `Authorization`).
    #[serde(default)]
    pub inject_header: Option<String>,
    /// Header value template (default `Bearer {}`).
    #[serde(default)]
    pub credential_format: Option<String>,
    /// How long nono caches the key.
    #[serde(default = "default_ttl")]
    pub cache_ttl_secs: u32,
}

fn default_ttl() -> u32 {
    300
}

/// [`SandboxConfig`] with paths expanded and every value validated.
#[derive(Debug, Clone)]
pub struct SandboxSettings {
    /// How to find nono.
    pub nono: String,
    /// Inputs of `build_session_dirs`.
    pub agent_dir: AgentDirSpec,
    /// Readable system paths.
    pub read_only: Vec<AbsPath>,
    /// Read-only grants at level relaxed.
    pub relaxed_reads: Vec<PathGrant>,
    /// Approval timeout.
    pub approval_timeout_secs: u32,
    /// nono's TMPDIR.
    pub nono_tmp: AbsPath,
    /// Configured credential routes (used for providers that have an API key).
    pub routes: Vec<CredentialRoute>,
}

/// Why `sandbox.toml` can't be used.
#[derive(Debug, thiserror::Error)]
#[error("sandbox.toml: {0}")]
pub struct SettingsError(String);

impl SandboxConfig {
    /// Expands and validates the configuration. `pi_agent_dir_env` is the daemon's
    /// `$PI_CODING_AGENT_DIR`, the fallback for the real agent dir.
    pub fn resolve(
        &self,
        home: &AbsPath,
        pi_agent_dir_env: Option<&str>,
    ) -> Result<SandboxSettings, SettingsError> {
        let path = |what: &str, value: &str| {
            expand_home(value, home).map_err(|e| SettingsError(format!("{what}: {e}")))
        };
        let user_agent_dir = match (&self.pi_agent_dir, pi_agent_dir_env) {
            (Some(dir), _) => path("pi_agent_dir", dir)?,
            (None, Some(dir)) if !dir.is_empty() => path("PI_CODING_AGENT_DIR", dir)?,
            (None, _) => home.join(".pi/agent"),
        };
        let optional = |what: &str, value: &Option<String>| {
            value.as_deref().map(|v| path(what, v)).transpose()
        };
        Ok(SandboxSettings {
            nono: self.nono.clone(),
            agent_dir: AgentDirSpec {
                user_agent_dir: Some(user_agent_dir),
                keep: self.keep.clone(),
                npm: optional("npm", &self.npm)?,
                copy: self.copy.clone(),
                oauth_claims: self.oauth_claims.clone(),
                tmp_seed: optional("tmp_seed", &self.tmp_seed)?,
            },
            read_only: self
                .read_only
                .iter()
                .map(|p| path("read_only", p))
                .collect::<Result<_, _>>()?,
            relaxed_reads: self
                .relaxed_reads
                .iter()
                .map(|r| {
                    Ok(PathGrant {
                        path: path("relaxed_reads", &r.path)?,
                        kind: r.kind,
                        access: Access::Read,
                    })
                })
                .collect::<Result<_, SettingsError>>()?,
            approval_timeout_secs: self.approval_timeout_secs,
            nono_tmp: path("nono_tmp", &self.nono_tmp)?,
            routes: self
                .routes
                .iter()
                .map(|r| r.resolve(self.allow_loopback_upstreams))
                .collect::<Result<_, _>>()?,
        })
    }
}

impl RouteConfig {
    fn resolve(&self, allow_loopback: bool) -> Result<CredentialRoute, SettingsError> {
        let err =
            |e: &dyn std::fmt::Display| SettingsError(format!("route {:?}: {e}", self.provider));
        let rules = |rules: &[String]| -> Result<Vec<Endpoint>, SettingsError> {
            rules
                .iter()
                .map(|rule| match rule.split_once(' ') {
                    Some((method, path)) if !method.is_empty() && path.starts_with('/') => {
                        Ok(Endpoint::new(method, path.trim()))
                    }
                    _ => Err(err(&format!("rule {rule:?} is not `METHOD /path`"))),
                })
                .collect()
        };
        let upstream = Upstream::new(&self.upstream).map_err(|e| err(&e))?;
        if upstream.allowed_host().is_none() && !allow_loopback {
            return Err(err(
                &"loopback upstreams expose every local HTTP service to the sandbox; \
                   set allow_loopback_upstreams = true only for local test servers",
            ));
        }
        Ok(CredentialRoute {
            provider: Name::new(&self.provider).map_err(|e| err(&e))?,
            upstream,
            endpoints: EndpointPolicy {
                allow: rules(&self.allow)?,
                approve: rules(&self.approve)?,
            },
            inject_header: self.inject_header.clone(),
            credential_format: self.credential_format.clone(),
            cache_ttl_secs: self.cache_ttl_secs,
        })
    }
}

/// Loads `sandbox.toml`.
pub fn load(dirs: &Dirs) -> Loaded<SandboxConfig> {
    load_toml(&dirs.sandbox_toml())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> AbsPath {
        AbsPath::new("/home/alice").unwrap()
    }

    #[test]
    fn defaults() {
        let config: SandboxConfig = toml::from_str("").unwrap();
        assert_eq!(config, SandboxConfig::default());
        let s = config.resolve(&home(), None).unwrap();
        assert_eq!(s.nono, "nono");
        assert_eq!(
            s.agent_dir.user_agent_dir.unwrap().as_str(),
            "/home/alice/.pi/agent"
        );
        assert_eq!(s.read_only, [AbsPath::new("/nix/store").unwrap()]);
        assert_eq!(s.nono_tmp.as_str(), "/tmp");
        assert!(s.routes.is_empty());

        let from_env = SandboxConfig::default()
            .resolve(&home(), Some("/srv/pi-agent"))
            .unwrap();
        assert_eq!(
            from_env.agent_dir.user_agent_dir.unwrap().as_str(),
            "/srv/pi-agent"
        );
    }

    #[test]
    fn full_file() {
        let config: SandboxConfig = toml::from_str(
            r#"
nono = "/nix/store/aaaa-nono-0.80.0/bin/nono"
pi_agent_dir = "~/agent"
keep = ["npm:@controlroom/pi"]
copy = ["AGENTS.md"]
npm = "/nix/store/bbbb-bundle"
relaxed_reads = [{ path = "~/.gitconfig", kind = "file" }]
approval_timeout_secs = 30

[oauth_claims]
sub = ["account_id"]

[[routes]]
provider = "gateway"
upstream = "https://llm.example.com"
allow = ["POST /v1/chat/completions"]
approve = ["GET /model/info"]
"#,
        )
        .unwrap();
        let s = config.resolve(&home(), Some("/ignored")).unwrap();
        assert_eq!(
            s.agent_dir.user_agent_dir.unwrap().as_str(),
            "/home/alice/agent"
        );
        assert_eq!(s.agent_dir.keep, ["npm:@controlroom/pi"]);
        assert_eq!(s.relaxed_reads[0].path.as_str(), "/home/alice/.gitconfig");
        assert_eq!(s.relaxed_reads[0].access, Access::Read);
        let route = &s.routes[0];
        assert_eq!(route.route_id(), "cr_gateway");
        assert_eq!(route.endpoints.allow[0].method, "POST");
        assert_eq!(route.endpoints.approve[0].path, "/model/info");
        assert_eq!(route.cache_ttl_secs, 300);
    }

    #[test]
    fn invalid_values() {
        let bad = |toml: &str| {
            let config: SandboxConfig = toml::from_str(toml).unwrap();
            config.resolve(&home(), None).unwrap_err()
        };
        bad("read_only = [\"relative\"]");
        bad("nono_tmp = \"tmp\"");
        bad("[[routes]]\nprovider = \"x\"\nupstream = \"http://example.com\"\n");
        bad(
            "[[routes]]\nprovider = \"x\"\nupstream = \"https://example.com\"\nallow = [\"POST\"]\n",
        );
        bad("[[routes]]\nprovider = \"../x\"\nupstream = \"https://example.com\"\n");
        bad("[[routes]]\nprovider = \"mock\"\nupstream = \"http://127.0.0.1:4000\"\n");
        let loopback: SandboxConfig = toml::from_str(
            "allow_loopback_upstreams = true\n[[routes]]\nprovider = \"mock\"\nupstream = \"http://127.0.0.1:4000\"\n",
        )
        .unwrap();
        assert!(loopback.resolve(&home(), None).is_ok());
        assert!(toml::from_str::<SandboxConfig>("unknown = 1").is_err());
    }
}
