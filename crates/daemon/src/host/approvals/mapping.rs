//! nono approval requests ↔ host approvals (spec §11.4, §11.5b).
//!
//! Every request nono sends becomes a [`HostApproval`] with a structured body (a mediated
//! `git push` shows remote, branch and force). The options ([`options`]):
//!
//! - commands, endpoints and network destinations: **Allow once** or **Deny** (nono asks for
//!   every invocation, so there is no runtime grant to offer);
//! - file access (capability elevation): **Allow for this session**, **Always allow** (also a
//!   standing grant on the machine's sandbox defaults, made by the controller), **Never**
//!   (also the machine's "never" list) or **Deny**. nono remembers a granted path for the rest
//!   of its run, so "once" can't be honored. "Always allow" is left out, and the detail says
//!   why, when the standing grant couldn't take effect (`standing`, from
//!   [`crate::host::sandbox::GrantLimits`]), so no answer is offered that silently does nothing.
//!
//! A denial carries the user's reason, if any; an expired or withdrawn approval is denied with
//! a reason saying so.
//!
//! [`HostApproval`]: controlroom_protocol::host::HostApproval

use controlroom_protocol::host::options::{ALLOW_ALWAYS, ALLOW_ONCE, ALLOW_SESSION, DENY, NEVER};
use controlroom_protocol::session::{OptionStyle, PathAccess, RequestBody, RequestOption};
use controlroom_sandbox::AbsPath;
use controlroom_sandbox::launch::shell_quote;
use controlroom_sandbox::policy::Access;
use controlroom_sandbox::webhook::{
    ApprovalReply, ApprovalRequest, ApprovalSubject, FileAccess, GitPush,
};

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

/// The access of a file request, or `None` for an access kind this build doesn't know.
pub fn path_access(access: FileAccess) -> Option<PathAccess> {
    match access {
        FileAccess::Read => Some(PathAccess::Read),
        FileAccess::Write => Some(PathAccess::Write),
        FileAccess::ReadWrite => Some(PathAccess::ReadWrite),
        FileAccess::Unknown => None,
    }
}

/// What to show for `approval` from the workspace at `root`, or `None` when it must be denied
/// without asking (unknown request types or access kinds). `standing` says why a standing grant
/// of a path with an access couldn't take effect, if it couldn't.
pub fn show(
    approval: &ApprovalRequest,
    root: &AbsPath,
    standing: impl Fn(&AbsPath, Access) -> Option<String>,
) -> Option<Shown> {
    let prompt = approval.prompt()?;
    let mut detail = prompt.reason;
    let (body, options) = match &approval.subject {
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
            (body, vec![allow_once(), deny()])
        }
        ApprovalSubject::Capability { path, access } => {
            let abs = AbsPath::new(path).ok();
            let body = RequestBody::Path {
                path: path.as_str().into(),
                access: path_access(*access)?,
                in_workspace: abs.as_ref().map(|p| root.contains(p)),
                machine_id: None,
                machine: None,
            };
            // "Always allow" grants read for a read and read-write for anything else.
            let grant = match access {
                FileAccess::Read => Access::Read,
                _ => Access::ReadWrite,
            };
            let cant_stand = match &abs {
                Some(path) => standing(path, grant),
                None => Some("it is not an absolute path".into()),
            };
            let option = |id: &str, label: &str, style| RequestOption {
                id: id.into(),
                label: label.into(),
                style,
            };
            let mut options = vec![option(
                ALLOW_SESSION,
                "Allow for this session",
                OptionStyle::AllowAlways,
            )];
            match cant_stand {
                Some(why) => detail = Some(format!("“Always allow” isn’t offered: {why}.")),
                None => options.push(option(
                    ALLOW_ALWAYS,
                    "Always allow",
                    OptionStyle::AllowAlways,
                )),
            }
            options.extend([deny(), option(NEVER, "Never", OptionStyle::Deny)]);
            (body, options)
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
            (body, vec![allow_once(), deny()])
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
            (body, vec![allow_once(), deny()])
        }
        ApprovalSubject::Unknown { .. } => return None,
    };
    Some(Shown {
        title: prompt.title,
        detail,
        body,
        options,
    })
}

/// Plain denial; listed before "Never", so a client that takes the first deny-styled option
/// (e.g. to deny with a reason) never answers "Never" by accident.
fn deny() -> RequestOption {
    RequestOption {
        id: DENY.into(),
        label: "Deny".into(),
        style: OptionStyle::Deny,
    }
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
            if [ALLOW_ONCE, ALLOW_SESSION, ALLOW_ALWAYS].contains(&option_id.as_str()) =>
        {
            ApprovalReply::granted()
        }
        Outcome::Chosen {
            reason: Some(reason),
            ..
        } if !reason.trim().is_empty() => ApprovalReply::denied(reason.trim()),
        Outcome::Chosen { option_id, .. } if option_id == NEVER => {
            ApprovalReply::denied("the user never allows this path")
        }
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

    fn root() -> AbsPath {
        AbsPath::new("/home/alice/project").unwrap()
    }

    fn no_limits(_: &AbsPath, _: Access) -> Option<String> {
        None
    }

    /// A file request whose standing grant couldn't take effect offers no "Always allow" and
    /// says why; a write request is checked as the read-write grant "Always allow" would make.
    #[test]
    fn always_is_left_out_when_it_could_not_take_effect() {
        let request = |path: &str, access: &str| {
            decode(&format!(
                r#"{{"backend":"cr","request":{{"capability_type":"capability","request_id":"4",
                "path":"{path}","access":"{access}"}}}}"#
            ))
        };
        let limits = |path: &AbsPath, access: Access| {
            (path.as_str() == "/home/alice/.config" || access == Access::ReadWrite)
                .then(|| "it contains /home/alice/.config/gcloud".to_owned())
        };
        let ids =
            |shown: &Shown| -> Vec<String> { shown.options.iter().map(|o| o.id.clone()).collect() };
        let config = show(&request("/home/alice/.config", "Read"), &root(), limits).unwrap();
        assert_eq!(ids(&config), [ALLOW_SESSION, DENY, NEVER]);
        assert_eq!(
            config.detail.as_deref(),
            Some("“Always allow” isn’t offered: it contains /home/alice/.config/gcloud.")
        );
        let write = show(&request("/srv/x", "Write"), &root(), limits).unwrap();
        assert_eq!(ids(&write), [ALLOW_SESSION, DENY, NEVER]);
        let read = show(&request("/srv/x", "Read"), &root(), limits).unwrap();
        assert_eq!(ids(&read), [ALLOW_SESSION, ALLOW_ALWAYS, DENY, NEVER]);
    }

    #[test]
    fn a_git_push_shows_remote_branch_and_force() {
        let shown = show(
            &decode(
                r#"{"backend":"cr","request":{"capability_type":"command","request_id":"1",
            "command":"git","args":["git","push","--force","origin","fix/flaky"]}}"#,
            ),
            &root(),
            no_limits,
        )
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
        let file = |path: &str| {
            show(
                &decode(&format!(
                    r#"{{"backend":"cr","request":{{"capability_type":"capability","request_id":"3",
                    "path":"{path}","access":"Read","reason":"intercepted"}}}}"#
                )),
                &root(),
                no_limits,
            )
            .unwrap()
        };
        let outside = file("/home/alice/notes.txt");
        assert_eq!(outside.detail.as_deref(), Some("intercepted"));
        let ids: Vec<&str> = outside.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, [ALLOW_SESSION, ALLOW_ALWAYS, DENY, NEVER]);
        assert!(matches!(
            outside.body,
            RequestBody::Path {
                access: PathAccess::Read,
                in_workspace: Some(false),
                ..
            }
        ));
        let inside = file("/home/alice/project/.git/config");
        assert!(matches!(
            inside.body,
            RequestBody::Path {
                in_workspace: Some(true),
                ..
            }
        ));
        let unknown =
            decode(r#"{"backend":"cr","request":{"capability_type":"teleport","request_id":"6"}}"#);
        assert!(show(&unknown, &root(), no_limits).is_none());
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
        assert_eq!(chosen(ALLOW_ALWAYS, None).decision, ReplyDecision::Granted);
        assert_eq!(chosen(NEVER, None).decision, ReplyDecision::Denied);
        let denied = chosen(DENY, Some("not that branch"));
        assert_eq!(denied.decision, ReplyDecision::Denied);
        assert_eq!(denied.reason.as_deref(), Some("not that branch"));
        assert_eq!(reply(&Outcome::Expired).decision, ReplyDecision::Denied);
        assert_eq!(reply(&Outcome::Withdrawn).decision, ReplyDecision::Denied);
    }
}
