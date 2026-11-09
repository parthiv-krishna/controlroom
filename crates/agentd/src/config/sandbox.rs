//! `sandbox.toml`: inputs of the agents' sandboxed launches on the server (spec §11, design §4).
//!
//! The *policy* (level, standing grants, domains) comes from the controller per session; this
//! file holds what only the server knows. Every key is optional:
//!
//! ```toml
//! nono = "nono"                     # program found by the login shell, or an absolute path
//! oauth_helper = "controlroom-pi-oauth"  # likewise; refreshes OAuth tokens (controlroom-pi bundle)
//! models_helper = "controlroom-pi-models"  # likewise; lists models, signs in (Settings › Models)
//! pi_agent_dir = "/var/lib/controlroom-agent/agent"  # default: <state>/agent
//! keep = ["npm:@controlroom/pi"]    # pi resources (packages, extensions, ...) kept in sessions
//! copy = ["AGENTS.md"]              # extra files copied from the real agent dir
//! npm = "/var/lib/controlroom-agent/agent/npm"  # target of the session's agent/npm link
//! tmp_seed = "/nix/store/…-jiti"    # copied into a new session's TMPDIR (warm jiti cache)
//! read_only = ["/nix/store"]        # readable system paths
//! nono_tmp = "/tmp"                 # nono's own TMPDIR; short, because nono binds sockets in it
//! allow_loopback_upstreams = false  # tests only: see below
//!
//! [oauth_claims]
//! some-provider = ["account_id"]    # claims carried by the synthetic sandbox JWT
//!
//! [[routes]]                        # one credential route per model provider
//! provider = "gateway"
//! upstream = "https://llm.example.com"
//! allow = ["POST /v1/chat/completions"]
//! ```
//!
//! pi runs here with no project files and no tools of its own (`--no-builtin-tools`): its sandbox
//! reaches only its scratch directory, its agent socket and the model routes. **OAuth
//! providers** (in the agent dir's `auth.json`, from `controlroom-agentd login`) get credential
//! routes like API-key providers once `oauth_helper` is found: `controlroom-agentd credential`
//! then prints fresh access tokens that the helper refreshes with pi's own locked refresh, so
//! the agent daemon is the only refresher.
//!
//! **Loopback upstreams** (`http://127.0.0.1:<port>`) are refused unless
//! `allow_loopback_upstreams = true`: with such a route, nono 0.80 stops excluding loopback
//! from its forward proxy, and the sandbox can then reach *every* HTTP service on the server's
//! loopback interface through it. They exist for local mock models.

use std::collections::BTreeMap;

use controlroom_sandbox::agent_dir::AgentDirSpec;
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
    /// The OAuth helper (`controlroom-pi-oauth`), found like `nono`.
    pub oauth_helper: String,
    /// The models helper (`controlroom-pi-models`), found like `nono`.
    pub models_helper: String,
    /// pi's agent dir (credentials, models, settings).
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
            oauth_helper: "controlroom-pi-oauth".to_owned(),
            models_helper: "controlroom-pi-models".to_owned(),
            pi_agent_dir: None,
            keep: Vec::new(),
            copy: Vec::new(),
            npm: None,
            tmp_seed: None,
            read_only: vec!["/nix/store".to_owned()],
            nono_tmp: "/tmp".to_owned(),
            allow_loopback_upstreams: false,
            oauth_claims: BTreeMap::new(),
            routes: Vec::new(),
        }
    }
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

/// How long nono caches a credential unless a route says otherwise.
pub const DEFAULT_CACHE_TTL_SECS: u32 = 300;

fn default_ttl() -> u32 {
    DEFAULT_CACHE_TTL_SECS
}

/// [`SandboxConfig`] with paths expanded and every value validated.
#[derive(Debug, Clone)]
pub struct SandboxSettings {
    /// How to find nono.
    pub nono: String,
    /// How to find the OAuth helper.
    pub oauth_helper: String,
    /// How to find the models helper.
    pub models_helper: String,
    /// Inputs of `build_session_dirs`.
    pub agent_dir: AgentDirSpec,
    /// Readable system paths.
    pub read_only: Vec<AbsPath>,
    /// nono's TMPDIR.
    pub nono_tmp: AbsPath,
    /// Configured credential routes (used for providers that have an API key).
    pub routes: Vec<CredentialRoute>,
    /// Whether loopback upstreams are accepted (tests with a local mock model).
    pub allow_loopback_upstreams: bool,
}

/// Why `sandbox.toml` can't be used.
#[derive(Debug, thiserror::Error)]
#[error("sandbox.toml: {0}")]
pub struct SettingsError(String);

impl SandboxConfig {
    /// Expands and validates the configuration; `default_agent_dir` is `<state>/agent`.
    pub fn resolve(
        &self,
        home: Option<&AbsPath>,
        default_agent_dir: &AbsPath,
    ) -> Result<SandboxSettings, SettingsError> {
        let path = |what: &str, value: &str| {
            expand_home(value, home).map_err(|e| SettingsError(format!("{what}: {e}")))
        };
        let user_agent_dir = match &self.pi_agent_dir {
            Some(dir) => path("pi_agent_dir", dir)?,
            None => default_agent_dir.clone(),
        };
        let optional = |what: &str, value: &Option<String>| {
            value.as_deref().map(|v| path(what, v)).transpose()
        };
        Ok(SandboxSettings {
            nono: self.nono.clone(),
            oauth_helper: self.oauth_helper.clone(),
            models_helper: self.models_helper.clone(),
            agent_dir: AgentDirSpec {
                user_agent_dir: Some(user_agent_dir),
                keep: self.keep.clone(),
                npm: optional("npm", &self.npm)?,
                copy: self.copy.clone(),
                oauth_claims: self.oauth_claims.clone(),
                tmp_seed: optional("tmp_seed", &self.tmp_seed)?,
                // Found at launch time (see `launch::Host`).
                oauth_helper: None,
                // Merged in from Settings › Models (see `crate::models`).
                managed: Default::default(),
            },
            read_only: self
                .read_only
                .iter()
                .map(|p| path("read_only", p))
                .collect::<Result<_, _>>()?,
            nono_tmp: short_nono_tmp(path("nono_tmp", &self.nono_tmp)?)?,
            routes: self
                .routes
                .iter()
                .map(|r| r.resolve(self.allow_loopback_upstreams))
                .collect::<Result<_, _>>()?,
            allow_loopback_upstreams: self.allow_loopback_upstreams,
        })
    }
}

impl RouteConfig {
    /// Validates the route (see the module docs on loopback upstreams).
    pub fn resolve(&self, allow_loopback: bool) -> Result<CredentialRoute, SettingsError> {
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

/// Longest usable `nono_tmp`: nono 0.80 binds
/// `<TMPDIR>/nono-tool-sandbox-<pid>-<19 digits>-<16 hex>/supervisor.sock` (78 bytes after the
/// directory) and fails beyond the 107-byte `sun_path` limit.
pub const NONO_TMP_MAX: usize = 28;

fn short_nono_tmp(dir: AbsPath) -> Result<AbsPath, SettingsError> {
    if dir.as_str().len() > NONO_TMP_MAX {
        return Err(SettingsError(format!(
            "nono_tmp {dir:?} is longer than {NONO_TMP_MAX} bytes; nono binds unix sockets in it"
        )));
    }
    Ok(dir)
}

/// Loads `sandbox.toml`.
pub fn load(dirs: &Dirs) -> Loaded<SandboxConfig> {
    load_toml(&dirs.sandbox_toml())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_dir() -> AbsPath {
        AbsPath::new("/var/lib/controlroom-agent/agent").unwrap()
    }

    #[test]
    fn defaults() {
        let config: SandboxConfig = toml::from_str("").unwrap();
        assert_eq!(config, SandboxConfig::default());
        let s = config.resolve(None, &agent_dir()).unwrap();
        assert_eq!(s.nono, "nono");
        assert_eq!(s.agent_dir.user_agent_dir, Some(agent_dir()));
        assert_eq!(s.read_only, [AbsPath::new("/nix/store").unwrap()]);
        assert_eq!(s.nono_tmp.as_str(), "/tmp");
        assert!(s.routes.is_empty());
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
        let home = AbsPath::new("/home/alice").unwrap();
        let s = config.resolve(Some(&home), &agent_dir()).unwrap();
        assert_eq!(
            s.agent_dir.user_agent_dir.unwrap().as_str(),
            "/home/alice/agent"
        );
        assert_eq!(s.agent_dir.keep, ["npm:@controlroom/pi"]);
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
            config.resolve(None, &agent_dir()).unwrap_err()
        };
        bad("read_only = [\"relative\"]");
        bad("nono_tmp = \"tmp\"");
        bad("nono_tmp = \"/var/tmp/a-rather-long-directory-name\"");
        bad("[[routes]]\nprovider = \"x\"\nupstream = \"http://example.com\"\n");
        bad("[[routes]]\nprovider = \"mock\"\nupstream = \"http://127.0.0.1:4000\"\n");
        let loopback: SandboxConfig = toml::from_str(
            "allow_loopback_upstreams = true\n[[routes]]\nprovider = \"mock\"\nupstream = \"http://127.0.0.1:4000\"\n",
        )
        .unwrap();
        assert!(loopback.resolve(None, &agent_dir()).is_ok());
        assert!(toml::from_str::<SandboxConfig>("exec_dirs = []").is_err());
    }
}
