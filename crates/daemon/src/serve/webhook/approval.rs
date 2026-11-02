//! nono approval requests ↔ controlroom requests (spec §11.4, §11.5b).
//!
//! Every request nono sends becomes a blocking `request{kind: sandbox}` with a structured body
//! (a mediated `git push` shows remote, branch and force). The options:
//!
//! - commands, endpoints and network destinations: **Allow once** or **Deny**. nono asks for
//!   every invocation, so there is no runtime grant to offer;
//! - file access (capability elevation): **Allow for this session** or **Deny**. nono
//!   remembers a granted path for the rest of its session, so "once" can't be honored.
//!
//! A denial carries the user's reason, if any. A request that expired or was withdrawn is
//! denied with a reason saying so.

use controlroom_protocol::session::{
    OptionStyle, PathAccess, Request, RequestBody, RequestKind, RequestOption, RequestState,
    Response,
};
use controlroom_runtime::RaiseRequest;
use controlroom_sandbox::launch::shell_quote;
use controlroom_sandbox::webhook::{
    ApprovalReply, ApprovalRequest, ApprovalSubject, FileAccess, GitPush,
};

/// Option id: allow this one request.
pub const ALLOW_ONCE: &str = "allow-once";
/// Option id: allow, and nono remembers it for the rest of its session.
pub const ALLOW_SESSION: &str = "allow-session";
/// Option id: deny.
pub const DENY: &str = "deny";

/// The request to show for `approval`, or `None` when it must be denied without asking
/// (unknown request types or access kinds).
pub fn to_request(approval: &ApprovalRequest) -> Option<RaiseRequest> {
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
            (Some(body), allow_once())
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
            (Some(body), allow)
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
            (Some(body), allow_once())
        }
        ApprovalSubject::Network { .. } => (None, allow_once()),
        ApprovalSubject::Unknown { .. } => return None,
    };
    Some(RaiseRequest {
        kind: RequestKind::Sandbox,
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

/// The webhook reply for a resolved request.
pub fn to_reply(request: &Request) -> ApprovalReply {
    match request.state {
        RequestState::Answered => match request.answer.as_ref().map(|a| &a.response) {
            Some(Response::Choice { option_id, .. })
                if option_id == ALLOW_ONCE || option_id == ALLOW_SESSION =>
            {
                ApprovalReply::granted()
            }
            Some(Response::Choice {
                reason: Some(reason),
                ..
            }) if !reason.trim().is_empty() => ApprovalReply::denied(reason.trim()),
            _ => ApprovalReply::denied("denied by the user"),
        },
        RequestState::Expired => ApprovalReply::denied("nobody answered the approval in time"),
        RequestState::Withdrawn => {
            ApprovalReply::denied("the approval was withdrawn (run cancelled or session closed)")
        }
        RequestState::Pending | RequestState::Unknown => ApprovalReply::denied("not approved"),
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::UnixMillis;
    use controlroom_protocol::ids::RequestId;
    use controlroom_protocol::session::RequestAnswer;
    use controlroom_sandbox::webhook::ReplyDecision;

    use super::*;

    fn decode(json: &str) -> ApprovalRequest {
        ApprovalRequest::from_json(json.as_bytes()).unwrap()
    }

    #[test]
    fn a_git_push_shows_remote_branch_and_force() {
        let r = to_request(&decode(
            r#"{"backend":"cr","request":{"capability_type":"command","request_id":"1",
            "command":"git","args":["git","push","--force","origin","fix/flaky"]}}"#,
        ))
        .unwrap();
        assert_eq!(r.kind, RequestKind::Sandbox);
        assert_eq!(r.title, "Run `git push --force origin fix/flaky`?");
        assert_eq!(
            r.body,
            Some(RequestBody::GitPush {
                command: "git push --force origin fix/flaky".into(),
                remote: "origin".into(),
                branch: "fix/flaky".into(),
                force: true,
            })
        );
        let ids: Vec<&str> = r.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, [ALLOW_ONCE, DENY]);

        let bare = to_request(&decode(
            r#"{"backend":"cr","request":{"capability_type":"command","request_id":"2",
            "command":"git","args":["git","push"]}}"#,
        ))
        .unwrap();
        let Some(RequestBody::GitPush { remote, branch, .. }) = bare.body else {
            panic!("{:?}", bare.body);
        };
        assert_eq!(
            (remote.as_str(), branch.as_str()),
            ("(default remote)", "(current branch)")
        );
    }

    #[test]
    fn other_subjects_and_unknown_ones() {
        let file = to_request(&decode(
            r#"{"backend":"cr","request":{"capability_type":"capability","request_id":"3",
            "path":"/home/alice/notes.txt","access":"Read","reason":"intercepted"}}"#,
        ))
        .unwrap();
        assert_eq!(
            file.body,
            Some(RequestBody::Path {
                path: "/home/alice/notes.txt".into(),
                access: PathAccess::Read
            })
        );
        assert_eq!(file.detail.as_deref(), Some("intercepted"));
        assert_eq!(file.options[0].id, ALLOW_SESSION);

        let endpoint = to_request(&decode(
            r#"{"backend":"cr","request":{"capability_type":"endpoint","request_id":"4",
            "route_id":"cr_x","upstream":"https://api.example.com","method":"GET","path":"/v1/models"}}"#,
        ))
        .unwrap();
        assert_eq!(
            endpoint.body,
            Some(RequestBody::Endpoint {
                method: "GET".into(),
                url: "https://api.example.com/v1/models".into()
            })
        );

        let other = to_request(&decode(
            r#"{"backend":"cr","request":{"capability_type":"command","request_id":"5",
            "command":"kubectl","args":["kubectl","apply","-f","x y.yaml"]}}"#,
        ))
        .unwrap();
        assert_eq!(
            other.body,
            Some(RequestBody::Command {
                command: "kubectl apply -f 'x y.yaml'".into(),
                cwd: None
            })
        );

        for unknown in [
            r#"{"backend":"cr","request":{"capability_type":"teleport","request_id":"6"}}"#,
            r#"{"backend":"cr","request":{"capability_type":"capability","request_id":"7","path":"/x","access":"Exec"}}"#,
        ] {
            assert!(to_request(&decode(unknown)).is_none());
        }
    }

    fn resolved(state: RequestState, response: Option<Response>) -> Request {
        Request {
            id: RequestId::new("daemon-1"),
            kind: RequestKind::Sandbox,
            blocking: true,
            state,
            title: "Run `git push`?".into(),
            detail: None,
            body: None,
            options: vec![],
            questions: vec![],
            answer: response.map(|response| RequestAnswer {
                response,
                label: String::new(),
                by: None,
                at: UnixMillis(1),
            }),
            origin_child: None,
            created_at: UnixMillis(0),
        }
    }

    fn choice(option: &str, reason: Option<&str>) -> Option<Response> {
        Some(Response::Choice {
            option_id: option.into(),
            reason: reason.map(str::to_owned),
        })
    }

    #[test]
    fn replies_follow_the_resolution() {
        let answered =
            |option, reason| to_reply(&resolved(RequestState::Answered, choice(option, reason)));
        assert_eq!(answered(ALLOW_ONCE, None).decision, ReplyDecision::Granted);
        assert_eq!(
            answered(ALLOW_SESSION, None).decision,
            ReplyDecision::Granted
        );
        let denied = answered(DENY, Some("push to a branch instead"));
        assert_eq!(denied.decision, ReplyDecision::Denied);
        assert_eq!(denied.reason.as_deref(), Some("push to a branch instead"));
        assert_eq!(
            answered(DENY, None).reason.as_deref(),
            Some("denied by the user")
        );
        for state in [
            RequestState::Expired,
            RequestState::Withdrawn,
            RequestState::Pending,
        ] {
            assert_eq!(
                to_reply(&resolved(state, None)).decision,
                ReplyDecision::Denied
            );
        }
    }
}
