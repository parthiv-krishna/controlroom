//! nono approval requests ↔ host approvals (spec §11.4, §11.5b).
//!
//! Every request nono sends becomes a [`HostApproval`] with a structured body (a mediated
//! `git push` shows remote, branch and force). The options:
//!
//! - commands, endpoints and network destinations: **Allow once** or **Deny** (nono asks for
//!   every invocation, so there is no runtime grant to offer);
//! - file access (capability elevation): **Allow for this session** or **Deny** (nono remembers
//!   a granted path for the rest of its run, so "once" can't be honored).
//!
//! A denial carries the user's reason, if any; an expired or withdrawn approval is denied with
//! a reason saying so.

use controlroom_protocol::session::{OptionStyle, PathAccess, RequestBody, RequestOption};
use controlroom_sandbox::launch::shell_quote;
use controlroom_sandbox::webhook::{
    ApprovalReply, ApprovalRequest, ApprovalSubject, FileAccess, GitPush,
};

/// Option id: allow this one request.
pub const ALLOW_ONCE: &str = "allow-once";
/// Option id: allow, and nono remembers it for the rest of its run.
pub const ALLOW_SESSION: &str = "allow-session";
/// Option id: deny.
pub const DENY: &str = "deny";

/// What an approval shows: title, detail, body and options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// One-line summary.
    pub title: String,
    /// nono's reason, if it gave one.
    pub detail: Option<String>,
    /// The structured subject.
    pub body: RequestBody,
    /// The answers.
    pub options: Vec<RequestOption>,
}

/// What to show for `approval`, or `None` when it must be denied without asking (unknown
/// request types or access kinds).
pub fn show(approval: &ApprovalRequest) -> Option<Shown> {
    let prompt = approval.prompt()?;
    let (body, allow) = match &approval.subject {
        ApprovalSubject::Command { command, args, .. } => {
            let line = if args.is_empty() {
                command.clone()
            } else {
                args.iter()
                    .map(|a| shell_quote(a))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let body = match &prompt.git_push {
                Some(push) => git_push(line, push),
                None => RequestBody::Command {
                    command: line,
                    cwd: None,
                },
            };
            (body, allow_once())
        }
        ApprovalSubject::Capability { path, access } => {
            let access = match access {
                FileAccess::Read => PathAccess::Read,
                FileAccess::Write => PathAccess::Write,
                FileAccess::ReadWrite => PathAccess::ReadWrite,
                FileAccess::Unknown => return None,
            };
            let body = RequestBody::Path {
                path: path.as_str().into(),
                access,
            };
            let allow = RequestOption {
                id: ALLOW_SESSION.into(),
                label: "Allow for this session".into(),
                style: OptionStyle::AllowAlways,
            };
            (body, allow)
        }
        ApprovalSubject::Endpoint {
            method,
            path,
            upstream,
            ..
        } => {
            let body = RequestBody::Endpoint {
                method: method.clone(),
                url: format!("{upstream}{path}"),
            };
            (body, allow_once())
        }
        ApprovalSubject::Network { host, port } => {
            let url = match port {
                Some(port) => format!("{host}:{port}"),
                None => host.clone(),
            };
            let body = RequestBody::Endpoint {
                method: "CONNECT".into(),
                url,
            };
            (body, allow_once())
        }
        ApprovalSubject::Unknown { .. } => return None,
    };
    Some(Shown {
        title: prompt.title,
        detail: prompt.reason,
        body,
        options: vec![
            allow,
            RequestOption {
                id: DENY.into(),
                label: "Deny".into(),
                style: OptionStyle::Deny,
            },
        ],
    })
}

fn allow_once() -> RequestOption {
    RequestOption {
        id: ALLOW_ONCE.into(),
        label: "Allow once".into(),
        style: OptionStyle::Allow,
    }
}

fn git_push(command: String, push: &GitPush) -> RequestBody {
    RequestBody::GitPush {
        command,
        remote: push
            .remote
            .clone()
            .unwrap_or_else(|| "(default remote)".into()),
        branch: if push.refspecs.is_empty() {
            "(current branch)".into()
        } else {
            push.refspecs.join(" ")
        },
        force: push.force,
    }
}

/// How an approval ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The user chose an option.
    Chosen {
        /// The option.
        option_id: String,
        /// A denial's reason.
        reason: Option<String>,
    },
    /// Nobody answered before nono's timeout.
    Expired,
    /// Its workspace closed.
    Withdrawn,
}

/// The webhook reply for an outcome.
pub fn reply(outcome: &Outcome) -> ApprovalReply {
    match outcome {
        Outcome::Chosen { option_id, .. }
            if option_id == ALLOW_ONCE || option_id == ALLOW_SESSION =>
        {
            ApprovalReply::granted()
        }
        Outcome::Chosen {
            reason: Some(reason),
            ..
        } if !reason.trim().is_empty() => ApprovalReply::denied(reason.trim()),
        Outcome::Chosen { .. } => ApprovalReply::denied("denied by the user"),
        Outcome::Expired => ApprovalReply::denied("nobody answered the approval in time"),
        Outcome::Withdrawn => {
            ApprovalReply::denied("the approval was withdrawn (workspace closed)")
        }
    }
}

#[cfg(test)]
mod tests {
    use controlroom_sandbox::webhook::ReplyDecision;

    use super::*;

    fn decode(json: &str) -> ApprovalRequest {
        ApprovalRequest::from_json(json.as_bytes()).unwrap()
    }

    #[test]
    fn a_git_push_shows_remote_branch_and_force() {
        let shown = show(&decode(
            r#"{"backend":"cr","request":{"capability_type":"command","request_id":"1",
            "command":"git","args":["git","push","--force","origin","fix/flaky"]}}"#,
        ))
        .unwrap();
        assert_eq!(shown.title, "Run `git push --force origin fix/flaky`?");
        assert_eq!(
            shown.body,
            RequestBody::GitPush {
                command: "git push --force origin fix/flaky".into(),
                remote: "origin".into(),
                branch: "fix/flaky".into(),
                force: true,
            }
        );
        let ids: Vec<&str> = shown.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, [ALLOW_ONCE, DENY]);
    }

    #[test]
    fn file_access_and_unknown_requests() {
        let file = show(&decode(
            r#"{"backend":"cr","request":{"capability_type":"capability","request_id":"3",
            "path":"/home/alice/notes.txt","access":"Read","reason":"intercepted"}}"#,
        ))
        .unwrap();
        assert_eq!(file.detail.as_deref(), Some("intercepted"));
        assert_eq!(file.options[0].id, ALLOW_SESSION);
        let unknown =
            decode(r#"{"backend":"cr","request":{"capability_type":"teleport","request_id":"6"}}"#);
        assert!(show(&unknown).is_none());
    }

    #[test]
    fn replies_follow_the_outcome() {
        let chosen = |option: &str, reason: Option<&str>| {
            reply(&Outcome::Chosen {
                option_id: option.into(),
                reason: reason.map(str::to_owned),
            })
        };
        assert_eq!(chosen(ALLOW_ONCE, None).decision, ReplyDecision::Granted);
        assert_eq!(chosen(ALLOW_SESSION, None).decision, ReplyDecision::Granted);
        let denied = chosen(DENY, Some("not that branch"));
        assert_eq!(denied.decision, ReplyDecision::Denied);
        assert_eq!(denied.reason.as_deref(), Some("not that branch"));
        assert_eq!(reply(&Outcome::Expired).decision, ReplyDecision::Denied);
        assert_eq!(reply(&Outcome::Withdrawn).decision, ReplyDecision::Denied);
    }
}
