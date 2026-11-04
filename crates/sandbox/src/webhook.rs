//! nono approval webhooks (spec §11.4, nono approval-webhook protocol v1, unsigned variant).
//!
//! nono `POST`s `{"backend", "request"}` to `http://127.0.0.1:<port>/nono/v1/<session>/<secret>`
//! and waits for `{"decision": "granted"|"denied", "reason"}`. Requests are correlated on the
//! URL because endpoint requests carry no session id. Every request maps to a blocking
//! `request{kind: sandbox}` prompt; anything unrecognized is denied without asking.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::names::SessionId;

/// URL path prefix of the approval webhook.
pub const HOOK_PATH_PREFIX: &str = "/nono/v1/";

/// A per-session random secret embedded in the webhook URL (64 hex characters).
#[derive(Clone, PartialEq, Eq)]
pub struct HookSecret(String);

impl HookSecret {
    /// Generates a fresh secret from the OS random source.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// Parses a secret previously produced by [`HookSecret::generate`].
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(s.to_owned()))
    }

    /// The secret as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compares in constant time (for equal lengths).
    pub fn matches(&self, other: &HookSecret) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .bytes()
                .zip(other.0.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

impl fmt::Debug for HookSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HookSecret(<redacted>)")
    }
}

/// The approval webhook URL of one session (or, on a host, of one workspace).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookUrl {
    /// Loopback port of the daemon's webhook listener.
    pub port: u16,
    /// The session (or host workspace) the hook belongs to.
    pub session: SessionId,
    /// The session's secret.
    pub secret: HookSecret,
}

impl HookUrl {
    /// The full URL written into the profile.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}{HOOK_PATH_PREFIX}{}/{}",
            self.port,
            self.session,
            self.secret.as_str()
        )
    }
}

/// Splits a request path `/nono/v1/<session>/<secret>` into its parts.
///
/// The caller must still check the secret against the session's with
/// [`HookSecret::matches`].
pub fn parse_hook_path(path: &str) -> Option<(SessionId, HookSecret)> {
    let rest = path.strip_prefix(HOOK_PATH_PREFIX)?;
    let (session, secret) = rest.split_once('/')?;
    Some((SessionId::new(session).ok()?, HookSecret::parse(secret)?))
}

/// File access requested through capability elevation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    /// Read.
    Read,
    /// Write.
    Write,
    /// Read and write.
    ReadWrite,
    /// A value this version does not know.
    Unknown,
}

/// What a webhook request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalSubject {
    /// A mediated command invocation.
    Command {
        /// The command name.
        command: String,
        /// Full argv, including `argv[0]`.
        args: Vec<String>,
        /// The caller (`session` or another mediated command).
        caller: Option<String>,
    },
    /// File access outside the profile (capability elevation).
    Capability {
        /// The requested path.
        path: String,
        /// The requested access.
        access: FileAccess,
    },
    /// An API endpoint on a credential route.
    Endpoint {
        /// The route (`cr_*`).
        route_id: String,
        /// The upstream origin.
        upstream: String,
        /// HTTP method.
        method: String,
        /// Request path.
        path: String,
    },
    /// A network destination (declared by the protocol, not sent by nono 0.80).
    Network {
        /// Host name.
        host: String,
        /// Port.
        port: Option<u16>,
    },
    /// A request type this version does not know; always denied.
    Unknown {
        /// The `capability_type` value.
        capability_type: String,
    },
}

/// A decoded approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// nono's request id (unique per request).
    pub request_id: String,
    /// The approval backend name from the profile.
    pub backend: String,
    /// nono's reason text, if any.
    pub reason: Option<String>,
    /// What is being asked for.
    pub subject: ApprovalSubject,
}

/// Errors decoding a webhook body.
#[derive(Debug, thiserror::Error)]
#[error("malformed approval webhook body: {0}")]
pub struct DecodeError(#[from] serde_json::Error);

#[derive(Deserialize)]
struct RawEnvelope {
    #[serde(default)]
    backend: String,
    request: RawRequest,
}

/// Every field of every variant, all optional, so unknown shapes still decode.
#[derive(Deserialize)]
struct RawRequest {
    capability_type: String,
    #[serde(default)]
    request_id: String,
    reason: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    caller: Option<String>,
    path: Option<String>,
    access: Option<String>,
    route_id: Option<String>,
    upstream: Option<String>,
    method: Option<String>,
    host: Option<String>,
    port: Option<u16>,
}

impl ApprovalRequest {
    /// Decodes a webhook body. Unknown request types decode to [`ApprovalSubject::Unknown`];
    /// known types with missing required fields do too, so they are denied rather than
    /// failing the HTTP exchange.
    pub fn from_json(body: &[u8]) -> Result<Self, DecodeError> {
        let raw: RawEnvelope = serde_json::from_slice(body)?;
        let r = raw.request;
        let unknown = || ApprovalSubject::Unknown {
            capability_type: r.capability_type.clone(),
        };
        let subject = match r.capability_type.as_str() {
            "command" => match &r.command {
                Some(command) => ApprovalSubject::Command {
                    command: command.clone(),
                    args: r.args.clone(),
                    caller: r.caller.clone(),
                },
                None => unknown(),
            },
            "capability" => match &r.path {
                Some(path) => ApprovalSubject::Capability {
                    path: path.clone(),
                    access: match r.access.as_deref() {
                        Some("Read") => FileAccess::Read,
                        Some("Write") => FileAccess::Write,
                        Some("ReadWrite") => FileAccess::ReadWrite,
                        _ => FileAccess::Unknown,
                    },
                },
                None => unknown(),
            },
            "endpoint" => match (&r.method, &r.path) {
                (Some(method), Some(path)) => ApprovalSubject::Endpoint {
                    route_id: r.route_id.clone().unwrap_or_default(),
                    upstream: r.upstream.clone().unwrap_or_default(),
                    method: method.clone(),
                    path: path.clone(),
                },
                _ => unknown(),
            },
            "network" => match &r.host {
                Some(host) => ApprovalSubject::Network {
                    host: host.clone(),
                    port: r.port,
                },
                None => unknown(),
            },
            _ => unknown(),
        };
        Ok(Self {
            request_id: r.request_id,
            backend: raw.backend,
            reason: r.reason,
            subject,
        })
    }

    /// The user-facing prompt, or `None` when the request must be denied without asking
    /// (unknown types or file access of unknown kind).
    pub fn prompt(&self) -> Option<SandboxPrompt> {
        let title = match &self.subject {
            ApprovalSubject::Command { args, command, .. } => {
                let shown = if args.is_empty() {
                    command.clone()
                } else {
                    shell_words(args)
                };
                format!("Run `{shown}`?")
            }
            ApprovalSubject::Capability { path, access } => {
                let verb = match access {
                    FileAccess::Read => "Read",
                    FileAccess::Write => "Write",
                    FileAccess::ReadWrite => "Read and write",
                    FileAccess::Unknown => return None,
                };
                format!("{verb} {path}?")
            }
            ApprovalSubject::Endpoint {
                method,
                path,
                upstream,
                ..
            } => format!("Call {method} {path} on {upstream}?"),
            ApprovalSubject::Network { host, port } => match port {
                Some(port) => format!("Connect to {host}:{port}?"),
                None => format!("Connect to {host}?"),
            },
            ApprovalSubject::Unknown { .. } => return None,
        };
        let git_push = match &self.subject {
            ApprovalSubject::Command { command, args, .. } if command == "git" => {
                GitPush::parse(args)
            }
            _ => None,
        };
        Some(SandboxPrompt {
            title,
            reason: self.reason.clone(),
            git_push,
        })
    }
}

/// A blocking `request{kind: sandbox}` to show the user. The runtime (P3) maps this onto
/// the protocol's request type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPrompt {
    /// One-line question, e.g. "Run `git push origin main`?".
    pub title: String,
    /// nono's reason text, if any.
    pub reason: Option<String>,
    /// Remote, refs and force flag when the request is a `git push`.
    pub git_push: Option<GitPush>,
}

/// The parts of a `git push` invocation the approval card shows (spec §11.5b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPush {
    /// The remote, if given.
    pub remote: Option<String>,
    /// The refspecs, if given.
    pub refspecs: Vec<String>,
    /// Whether the push can overwrite remote history.
    pub force: bool,
}

impl GitPush {
    /// Extracts remote, refspecs and force from `git [global opts] push [opts] [remote] [refs]`.
    /// Returns `None` if `push` is not the subcommand.
    pub fn parse(argv: &[String]) -> Option<Self> {
        let mut rest = argv.iter().skip(1);
        // Global options before the subcommand; `-c`/`-C` take a value.
        loop {
            let arg = rest.next()?;
            match arg.as_str() {
                "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                    rest.next()?;
                }
                a if a.starts_with('-') => {}
                "push" => break,
                _ => return None,
            }
        }
        let mut force = false;
        let mut positional = Vec::new();
        let mut options_done = false;
        let mut rest = rest.peekable();
        while let Some(arg) = rest.next() {
            if options_done || !arg.starts_with('-') {
                positional.push(arg.clone());
                continue;
            }
            match arg.as_str() {
                "--" => options_done = true,
                "-f" | "--force" | "--mirror" | "--delete" | "-d" | "--prune" => force = true,
                a if a.starts_with("--force-with-lease")
                    || a.starts_with("--force-if-includes") =>
                {
                    force = true
                }
                "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec" => {
                    rest.next();
                }
                a if a.starts_with('-') && !a.starts_with("--") && a.contains('f') => force = true,
                _ => {}
            }
        }
        let mut positional = positional.into_iter();
        let remote = positional.next();
        let refspecs: Vec<String> = positional.collect();
        force |= refspecs
            .iter()
            .any(|r| r.starts_with('+') || r.starts_with(':'));
        Some(Self {
            remote,
            refspecs,
            force,
        })
    }
}

/// The webhook reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalReply {
    /// The decision.
    pub decision: ReplyDecision,
    /// Optional reason shown to the agent on denial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A webhook decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyDecision {
    /// Allow this request.
    Granted,
    /// Refuse this request.
    Denied,
}

impl ApprovalReply {
    /// A grant.
    pub fn granted() -> Self {
        Self {
            decision: ReplyDecision::Granted,
            reason: None,
        }
    }

    /// A denial with a reason.
    pub fn denied(reason: impl Into<String>) -> Self {
        Self {
            decision: ReplyDecision::Denied,
            reason: Some(reason.into()),
        }
    }

    /// The JSON body to send with status 200.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("reply serialization cannot fail")
    }
}

/// Joins argv for display, quoting elements that need it.
fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(|a| crate::launch::shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "01HZY3K4M5N6P7Q8R9S0T1V2W3";

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn url_round_trip_and_secret_check() {
        let secret = HookSecret::generate().unwrap();
        let hook = HookUrl {
            port: 4242,
            session: SessionId::new(SESSION).unwrap(),
            secret: secret.clone(),
        };
        let url = hook.url();
        let path = url.strip_prefix("http://127.0.0.1:4242").unwrap();
        let (session, parsed) = parse_hook_path(path).unwrap();
        assert_eq!(session.as_str(), SESSION);
        assert!(parsed.matches(&secret));
        assert!(!HookSecret::generate().unwrap().matches(&secret));
        assert!(parse_hook_path("/nono/v1/not-a-ulid/abc").is_none());
        assert!(parse_hook_path(&format!("/nono/v1/{SESSION}/short")).is_none());
        assert!(!format!("{secret:?}").contains(secret.as_str()));
    }

    #[test]
    fn decodes_the_three_spike_shapes() {
        let command = br#"{"backend":"cr","request":{"capability_type":"command",
            "request_id":"tool-sandbox-approve-git-1","command":"git",
            "args":["git","push","origin","main"],"caller":"session",
            "intercept_rule":"push","reason":null,"child_pid":7,"session_id":"x"}}"#;
        let r = ApprovalRequest::from_json(command).unwrap();
        assert_eq!(r.backend, "cr");
        let p = r.prompt().unwrap();
        assert_eq!(p.title, "Run `git push origin main`?");
        assert_eq!(
            p.git_push,
            Some(GitPush {
                remote: Some("origin".into()),
                refspecs: vec!["main".into()],
                force: false
            })
        );

        let capability = br#"{"backend":"cr","request":{"capability_type":"capability",
            "request_id":"seccomp-1","path":"/home/alice/outside/canary.txt","access":"Read",
            "reason":"Sandbox intercepted file operation (seccomp-notify)","child_pid":7,"session_id":"x"}}"#;
        let r = ApprovalRequest::from_json(capability).unwrap();
        assert_eq!(
            r.prompt().unwrap().title,
            "Read /home/alice/outside/canary.txt?"
        );

        let endpoint = br#"{"backend":"cr","request":{"capability_type":"endpoint",
            "request_id":"proxy-endpoint-approval-1","route_id":"cr_mock","upstream":"https://api.example.com",
            "method":"GET","path":"/model/info","rule_label":"endpoint_policy.approve[GET /model/info]",
            "reason":"model discovery","child_pid":0,"session_id":"proxy"}}"#;
        let r = ApprovalRequest::from_json(endpoint).unwrap();
        let p = r.prompt().unwrap();
        assert_eq!(p.title, "Call GET /model/info on https://api.example.com?");
        assert_eq!(p.reason.as_deref(), Some("model discovery"));
    }

    #[test]
    fn unknown_or_incomplete_requests_are_denied_without_prompt() {
        for body in [
            br#"{"backend":"cr","request":{"capability_type":"teleport","request_id":"1"}}"#.as_slice(),
            br#"{"backend":"cr","request":{"capability_type":"command","request_id":"1"}}"#,
            br#"{"backend":"cr","request":{"capability_type":"capability","path":"/x","access":"Exec"}}"#,
        ] {
            let r = ApprovalRequest::from_json(body).unwrap();
            assert!(r.prompt().is_none(), "{r:?}");
        }
        assert!(ApprovalRequest::from_json(b"not json").is_err());
    }

    #[test]
    fn git_push_force_detection() {
        let force = |a: &[&str]| GitPush::parse(&args(a)).unwrap().force;
        assert!(!force(&["git", "push"]));
        assert!(force(&["git", "push", "--force", "origin", "main"]));
        assert!(force(&["git", "push", "-qf", "origin"]));
        assert!(force(&["git", "push", "--force-with-lease=main", "origin"]));
        assert!(force(&["git", "push", "origin", "+main"]));
        assert!(force(&["git", "push", "origin", ":old"]));
        assert!(force(&["git", "push", "origin", "--delete", "old"]));
        let p = GitPush::parse(&args(&[
            "git", "-c", "a=b", "-C", "/w", "push", "-o", "ci.skip", "up", "a:b",
        ]))
        .unwrap();
        assert_eq!(p.remote.as_deref(), Some("up"));
        assert_eq!(p.refspecs, ["a:b"]);
        assert!(!p.force);
        assert!(GitPush::parse(&args(&["git", "status"])).is_none());
    }

    #[test]
    fn reply_bodies() {
        assert_eq!(
            ApprovalReply::granted().to_json(),
            br#"{"decision":"granted"}"#
        );
        assert_eq!(
            ApprovalReply::denied("no").to_json(),
            br#"{"decision":"denied","reason":"no"}"#
        );
    }
}
