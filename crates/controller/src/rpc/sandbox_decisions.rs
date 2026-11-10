//! "Always allow" and "Never" on a file request (spec §11.4, §11.6): once the agent daemon has
//! recorded the answer (`request.respond`), the controller, which owns the sandbox defaults,
//! makes it standing on the requesting machine's profile:
//!
//! - **Always allow** → a standing path grant with the requested access (`write` becomes
//!   `read_write`), so new sessions and restarts get it in their profile;
//! - **Never** → the path joins the machine's never-list: later requests are denied on the host
//!   without asking.
//!
//! The host already applied the answer to the running workspace. Each decision is logged with
//! the deciding device, which is also recorded on the grant.

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::RequestRespondResult;
use controlroom_protocol::error::Resource;
use controlroom_protocol::host::options::{ALLOW_ALWAYS, NEVER};
use controlroom_protocol::session::{
    GrantKind, PathAccess, RequestBody, RequestKind, RequestState, Response,
};
use controlroom_protocol::ui::{NewGrant, ProfileChange, ProfileScope};
use serde_json::Value;

use super::sandbox::device_label;
use super::{Conn, profile_paths};
use crate::errors::{not_found, store_error};

/// Makes an answered "Always allow" or "Never" standing; other answers are left alone.
pub(super) fn remember(conn: &Conn, result: &Value) -> Result<(), RpcError> {
    let Ok(result) = serde_json::from_value::<RequestRespondResult>(result.clone()) else {
        return Ok(());
    };
    let Some((scope, change)) = standing_change(&result)? else {
        return Ok(());
    };
    let repo = conn.hub.repo();
    if let ProfileScope::Machine { machine_id } = &scope {
        repo.machines()
            .get(*machine_id)
            .map_err(store_error)?
            .ok_or_else(|| not_found(Resource::Machine, machine_id))?;
    }
    let device = conn.device_ref();
    tracing::info!(
        ?scope,
        ?change,
        device = %device_label(device.as_ref()),
        "sandbox decision on a file request"
    );
    repo.sandbox_profiles()
        .change(&scope, &change, device.as_ref())
        .map_err(store_error)
}

/// The profile change an answered file request asks for, if any.
fn standing_change(
    result: &RequestRespondResult,
) -> Result<Option<(ProfileScope, ProfileChange)>, RpcError> {
    let request = &result.request;
    let option = match request.answer.as_ref().map(|a| &a.response) {
        Some(Response::Choice { option_id, .. }) => option_id.as_str(),
        _ => return Ok(None),
    };
    let Some(RequestBody::Path {
        path,
        access,
        machine_id: Some(machine_id),
        ..
    }) = &request.body
    else {
        return Ok(None);
    };
    if request.kind != RequestKind::Sandbox || request.state != RequestState::Answered {
        return Ok(None);
    }
    let change = match option {
        ALLOW_ALWAYS => ProfileChange::AddGrant {
            grant: NewGrant {
                kind: GrantKind::Path,
                value: path.as_str().to_owned(),
                access: Some(match access {
                    PathAccess::Read => PathAccess::Read,
                    _ => PathAccess::ReadWrite,
                }),
            },
        },
        NEVER => ProfileChange::AddNever {
            path: path.as_str().to_owned(),
        },
        _ => return Ok(None),
    };
    let scope = ProfileScope::Machine {
        machine_id: *machine_id,
    };
    Ok(Some((scope, profile_paths::check_change(change)?)))
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::ids::{MachineId, RequestId, UnixMillis};
    use controlroom_protocol::session::{Request, RequestAnswer};

    use super::*;

    fn answered(option: &str, access: PathAccess, kind: RequestKind) -> RequestRespondResult {
        RequestRespondResult {
            request: Request {
                id: RequestId::new("approval-1"),
                kind,
                blocking: true,
                state: RequestState::Answered,
                title: "Read /home/alice/.cache/nix/x?".into(),
                detail: None,
                body: Some(RequestBody::Path {
                    path: "/home/alice/.cache/nix/x".into(),
                    access,
                    in_workspace: Some(false),
                    machine_id: Some(machine()),
                    machine: Some("devbox".into()),
                }),
                options: Vec::new(),
                questions: Vec::new(),
                answer: Some(RequestAnswer {
                    response: Response::Choice {
                        option_id: option.into(),
                        reason: None,
                    },
                    label: "Always allow (devbox)".into(),
                    by: None,
                    at: UnixMillis(1),
                }),
                origin_child: None,
                created_at: UnixMillis(1),
            },
            then: None,
        }
    }

    fn machine() -> MachineId {
        "01JAM000000000000000000001".parse().unwrap()
    }

    #[test]
    fn always_and_never_become_changes_of_the_machine_profile() {
        let scope = ProfileScope::Machine {
            machine_id: machine(),
        };
        let always = standing_change(&answered(
            ALLOW_ALWAYS,
            PathAccess::Write,
            RequestKind::Sandbox,
        ));
        assert_eq!(
            always.unwrap(),
            Some((
                scope.clone(),
                ProfileChange::AddGrant {
                    grant: NewGrant {
                        kind: GrantKind::Path,
                        value: "/home/alice/.cache/nix/x".into(),
                        access: Some(PathAccess::ReadWrite),
                    },
                },
            ))
        );
        let never = standing_change(&answered(NEVER, PathAccess::Read, RequestKind::Sandbox));
        assert_eq!(
            never.unwrap(),
            Some((
                scope,
                ProfileChange::AddNever {
                    path: "/home/alice/.cache/nix/x".into(),
                },
            ))
        );
        for (option, kind) in [
            ("allow-session", RequestKind::Sandbox),
            ("deny", RequestKind::Sandbox),
            (ALLOW_ALWAYS, RequestKind::Permission),
        ] {
            let change = standing_change(&answered(option, PathAccess::Read, kind)).unwrap();
            assert_eq!(change, None, "{option}");
        }
    }
}
