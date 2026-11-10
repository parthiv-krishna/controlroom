//! The subset of the nono 0.80 profile schema controlroom emits.
//!
//! Field names follow `nono-profile.schema.json`. There is deliberately no `extends`, `packs`
//! or `network_profile` field: generated profiles can never reference registry packs or
//! named base profiles. Maps are `BTreeMap` so serialization is deterministic.

use std::collections::BTreeMap;

use serde::Serialize;

/// A complete generated profile (`profile.json`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NonoProfile {
    /// Profile metadata.
    pub meta: Meta,
    /// Working-directory access.
    pub workdir: Workdir,
    /// Filesystem grants.
    pub filesystem: Filesystem,
    /// Built-in policy groups to add or remove.
    pub groups: Groups,
    /// Linux hardening.
    pub linux: Linux,
    /// Environment filtering and injection.
    pub environment: Environment,
    /// Network policy.
    pub network: Network,
    /// Host-side credential capture commands for `cmd://` routes.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub credential_capture: BTreeMap<String, CredentialCapture>,
    /// Capability elevation and its approval backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Security>,
    /// Command mediation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_policies: Option<CommandPolicies>,
}

/// `meta`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Meta {
    /// Profile name (`cr-<session>`).
    pub name: String,
}

/// `workdir`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Workdir {
    /// Always `readwrite` for controlroom sessions.
    pub access: &'static str,
}

/// `filesystem`. Every entry is an absolute path.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Filesystem {
    /// Read-only directories. (nono 0.80 takes a file here too, as a file grant; generated
    /// profiles list files in `read_file`.)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    /// Read-only single files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub read_file: Vec<String>,
    /// Read-write directories.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Read-write single files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_file: Vec<String>,
    /// AF_UNIX sockets the sandbox may connect to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unix_socket: Vec<String>,
    /// Directories where descendant AF_UNIX sockets may be bound and connected.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unix_socket_subtree_bind: Vec<String>,
    /// Granted paths exempted from nono's built-in denials (see [`crate::limits`]); nono only
    /// lifts a denial for a path some grant covers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bypass_protection: Vec<String>,
}

/// `groups`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Groups {
    /// Built-in groups to include.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<&'static str>,
    /// Built-in groups to remove from the implicit default set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<&'static str>,
}

/// `linux`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Linux {
    /// `pathname` closes the nscd DNS side channel.
    pub af_unix_mediation: &'static str,
}

/// `environment`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Environment {
    /// Inherited variables to strip.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deny_vars: Vec<&'static str>,
    /// Variables injected into the sandbox.
    pub set_vars: BTreeMap<&'static str, String>,
}

/// `network`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Network {
    /// Block all network access (used when nothing is allowed).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub block: bool,
    /// Hostnames reachable through the proxy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_domain: Vec<String>,
    /// Active credential routes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<String>,
    /// Credential route definitions.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_credentials: BTreeMap<String, CustomCredential>,
}

/// One `network.custom_credentials` entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomCredential {
    /// Upstream origin.
    pub upstream: String,
    /// `cmd://<route>`.
    pub credential_key: String,
    /// Phantom-token variable exported to the sandbox.
    pub env_var: String,
    /// Header the real credential is injected into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_header: Option<String>,
    /// Header value template (`Bearer {}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_format: Option<String>,
    /// Mandatory endpoint policy.
    pub endpoint_policy: EndpointPolicy,
}

/// `endpoint_policy`: default deny plus explicit allow/approve rules.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EndpointPolicy {
    /// Always `{"decision": "deny"}`.
    pub default: DefaultDecision,
    /// Requests forwarded without asking.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<EndpointRule>,
    /// Requests that need approval.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub approve: Vec<EndpointRule>,
}

/// `{"decision": ...}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefaultDecision {
    /// `deny`, `approve` or `allow`.
    pub decision: &'static str,
}

/// One endpoint rule.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EndpointRule {
    /// HTTP method or `*`.
    pub method: String,
    /// Path glob.
    pub path: String,
    /// Reason shown in the approval request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One `credential_capture` entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CredentialCapture {
    /// Host-side command (no shell); never contains a secret.
    pub command: Vec<String>,
    /// Maximum run time.
    pub timeout_secs: u32,
    /// How long nono caches the captured value.
    pub cache_ttl_secs: u32,
}

/// `security`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Security {
    /// Ask the webhook for file access outside the profile.
    pub capability_elevation: bool,
    /// Approval backends for capability requests.
    pub approval_backends: BTreeMap<&'static str, ApprovalBackend>,
    /// Default backend.
    pub approval_defaults: ApprovalDefaults,
}

/// A webhook approval backend.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalBackend {
    /// Always `webhook`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// The session's hook URL.
    pub url: String,
    /// How long nono waits before treating silence as a denial.
    pub timeout_secs: u32,
}

/// `approval_defaults`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalDefaults {
    /// Backend name.
    pub backend: &'static str,
}

/// `command_policies`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandPolicies {
    /// Directories scanned, like `PATH`'s, for executables the session may run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub executable_dirs: Vec<String>,
    /// Approval backends for command and endpoint approvals.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub approval_backends: BTreeMap<&'static str, ApprovalBackend>,
    /// Default backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_defaults: Option<ApprovalDefaults>,
    /// Mediated commands keyed by name.
    pub commands: BTreeMap<String, CommandPolicy>,
}

/// One mediated command.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandPolicy {
    /// Exact executable the name is pinned to.
    pub executable: String,
    /// Other mediated commands this one may invoke.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub can_use: Vec<String>,
    /// Per-caller policies (`session` for direct invocation).
    pub from: BTreeMap<String, CallerPolicy>,
}

/// A caller edge: `"deny"` or a sandbox with an optional invocation policy.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CallerPolicy {
    /// `"deny"`.
    Deny(&'static str),
    /// A command sandbox.
    Edge(Box<CallerEdge>),
}

/// `{"sandbox": ..., "invocation_policy": ...}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallerEdge {
    /// What the command receives.
    pub sandbox: CommandSandbox,
    /// Argument rules evaluated before launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_policy: Option<InvocationPolicy>,
}

/// A command sandbox (starts from nono's minimal baseline; nothing is inherited).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CommandSandbox {
    /// Paths the command may execute (git re-execs its helpers by absolute path).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exec_paths: Vec<String>,
    /// Readable directories (absolute or nono `@git:` tokens).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fs_read: Vec<String>,
    /// Readable files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fs_read_file: Vec<String>,
    /// Writable directories.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fs_write: Vec<String>,
    /// Writable files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fs_write_file: Vec<String>,
    /// Network policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<CommandNetwork>,
    /// Environment passed to the command.
    pub environment: CommandEnvironment,
}

/// A command sandbox's network policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CommandNetwork {
    /// Hostnames reachable through the command's own proxy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow_domain: Vec<String>,
    /// Raw TCP connect ports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tcp_connect_ports: Vec<u16>,
}

/// A command sandbox's environment.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CommandEnvironment {
    /// Variables passed through.
    pub allow_vars: Vec<&'static str>,
}

/// `invocation_policy`: default plus deny/approve rules (deny wins over approve).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvocationPolicy {
    /// `allow` or `approve`.
    pub default: &'static str,
    /// Invocations to refuse.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<InvocationRule>,
    /// Invocations that need approval.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub approve: Vec<InvocationRule>,
    /// Invocations that run without asking (when the default is `approve`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<InvocationRule>,
}

/// An invocation rule matching a contiguous argv window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvocationRule {
    /// The matcher.
    pub argv: ArgvContains,
}

/// `{"contains": [...]}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArgvContains {
    /// The contiguous window.
    pub contains: Vec<String>,
}
