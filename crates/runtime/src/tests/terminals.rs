//! The user's own terminals: opened sandboxed or (confirmed) unsandboxed, never seen or reached
//! by the agent, ended by closing them, their process ending, a host reboot, the session's
//! close and their workspace's removal; their approvals are marked as the user's.

use controlroom_drivers::fake::FakeScript;
use controlroom_protocol::ErrorData;
use controlroom_protocol::agent::{HostCallParams, ShellEndParams, ShellNewParams};
use controlroom_protocol::agentd::{
    TerminalCloseParams, TerminalOpenParams, WorkspaceAddParams, WorkspaceRemoveParams,
};
use controlroom_protocol::host::HostOp;
use controlroom_protocol::ids::{CallId, DeviceId, OpId, ShellId, WorkspaceId};
use controlroom_protocol::session::{
    DeviceRef, RequestKind, Shell, ShellOwner, ShellSandbox, WorkspaceSpec,
};

use super::Harness;

fn open(
    id: controlroom_protocol::ids::SessionId,
    workspace: WorkspaceId,
    sandbox: ShellSandbox,
    confirm: bool,
) -> TerminalOpenParams {
    TerminalOpenParams {
        op_id: OpId::generate(),
        id,
        workspace_id: workspace,
        sandbox,
        confirm,
        by: Some(DeviceRef {
            device_id: DeviceId::new("dev-laptop"),
            label: "Laptop".into(),
        }),
    }
}

fn names(shells: &[Shell]) -> Vec<&str> {
    shells.iter().map(|s| s.name.as_str()).collect()
}

#[tokio::test]
async fn the_users_terminals_are_private_to_the_user() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let workspace = h.summary(id).workspaces[0].id;

    let mine =
        h.rt.terminal_open(open(id, workspace, ShellSandbox::Workspace, false))
            .await
            .unwrap();
    assert_eq!(
        (mine.name.as_str(), mine.owner, mine.sandbox),
        ("you@devbox", ShellOwner::User, ShellSandbox::Workspace)
    );
    // Unsandboxed only when confirmed.
    let err =
        h.rt.terminal_open(open(id, workspace, ShellSandbox::Unsandboxed, false))
            .await
            .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::InvalidParams)),
        "{err:?}"
    );
    let params = open(id, workspace, ShellSandbox::Unsandboxed, true);
    let raw = h.rt.terminal_open(params.clone()).await.unwrap();
    assert_eq!(
        (raw.name.as_str(), raw.sandbox),
        ("you@devbox-2", ShellSandbox::Unsandboxed)
    );
    assert_eq!(h.rt.terminal_open(params).await.unwrap(), raw, "idempotent");
    let devbox = h.devbox;
    let on_host = h.machines.shells(devbox);
    assert!(
        on_host
            .iter()
            .any(|s| s.shell.name == "you@devbox-2" && s.shell.sandbox == ShellSandbox::Unsandboxed),
        "{on_host:?}"
    );

    // The UI sees them; the agent doesn't.
    assert_eq!(
        names(&h.summary(id).shells),
        ["devbox", "you@devbox", "you@devbox-2"]
    );
    let listed = h.rt.shell_list(id).await.unwrap().shells;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].shell.name, "devbox");
    let info = h.rt.session_info(id).await.unwrap();
    assert_eq!(info.shells.len(), 1);

    // The agent can't run commands in them, open or close them.
    let err =
        h.rt.host_call(
            id,
            HostCallParams {
                call_id: CallId::new("b1"),
                machine: "devbox".into(),
                op: HostOp::Bash {
                    shell: "you@devbox".into(),
                    command: "cat ~/.ssh/id_ed25519".into(),
                    timeout_s: None,
                },
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::ShellGone { .. })),
        "{err:?}"
    );
    assert!(h.machines.ran(devbox).is_empty());
    let err =
        h.rt.shell_open(
            id,
            ShellNewParams {
                call_id: CallId::new("n1"),
                name: "you@devbox".into(),
                machine: "devbox".into(),
                cwd: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::InvalidParams)),
        "{err:?}"
    );
    let err =
        h.rt.shell_close(
            id,
            ShellEndParams {
                call_id: CallId::new("e1"),
                name: "you@devbox".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::ShellGone { .. })),
        "{err:?}"
    );

    // The user closes one of theirs; the agent's shells can't be closed this way.
    let close = |name: &str| TerminalCloseParams {
        op_id: OpId::generate(),
        id,
        name: name.into(),
    };
    let err = h.rt.terminal_close(close("devbox")).await.unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::InvalidParams)),
        "{err:?}"
    );
    h.rt.terminal_close(close("you@devbox")).await.unwrap();
    assert_eq!(names(&h.summary(id).shells), ["devbox", "you@devbox-2"]);
    let gone = ShellId::of(id, "you@devbox");
    h.machines
        .wait("the host closed it", move |m| {
            m.shells(devbox).iter().all(|s| s.shell_id != gone)
        })
        .await;

    // Closing the session ends the rest; reopening it doesn't bring them back.
    h.rt.close(h.op(id)).await.unwrap();
    assert_eq!(names(&h.summary(id).shells), ["devbox"]);
    h.machines
        .wait("the host closed every shell", move |m| {
            m.shells(devbox).is_empty()
        })
        .await;
    h.rt.reopen(h.op(id)).await.unwrap();
    h.machines
        .wait("the agent's shell reopened", move |m| {
            m.shells(devbox).len() == 1
        })
        .await;
    assert_eq!(names(&h.summary(id).shells), ["devbox"]);
    let err =
        h.rt.terminal_close(close("you@devbox-2"))
            .await
            .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::ShellGone { .. })));
}

#[tokio::test]
async fn a_terminal_ends_with_its_process_a_reboot_or_its_workspace() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let workspace = h.summary(id).workspaces[0].id;
    let devbox = h.devbox;

    // Its process exits: the host says so, and it is gone (not reopened).
    let mine =
        h.rt.terminal_open(open(id, workspace, ShellSandbox::Unsandboxed, true))
            .await
            .unwrap();
    h.machines.end_shell(devbox, &ShellId::of(id, &mine.name));
    h.wait(id, "the ended terminal is gone", |s| s.shells.len() == 1)
        .await;

    // The host reboots without it: the next connection drops it, the agent's shell reopens.
    h.rt.terminal_open(open(id, workspace, ShellSandbox::Workspace, false))
        .await
        .unwrap();
    assert_eq!(h.summary(id).shells.len(), 2);
    h.machines.reboot(devbox);
    let s = h
        .wait(id, "the rebooted host's terminal is gone", |s| {
            names(&s.shells) == ["devbox"]
        })
        .await;
    assert_eq!(s.shells[0].owner, ShellOwner::Agent);
    h.machines
        .wait("the agent's shell reopened", move |m| {
            names(
                &m.shells(devbox)
                    .into_iter()
                    .map(|s| s.shell)
                    .collect::<Vec<_>>(),
            ) == ["devbox"]
        })
        .await;

    // A terminal's approval is marked as the user's.
    h.machines.approval_from(devbox, id, workspace, true);
    let s = h.wait(id, "the approval", |s| !s.requests.is_empty()).await;
    assert_eq!(s.requests[0].kind, RequestKind::Sandbox);
    assert!(
        s.requests[0].title.starts_with("Your terminal: "),
        "{}",
        s.requests[0].title
    );

    // A second workspace's terminal ends with the workspace.
    let added =
        h.rt.workspace_add(WorkspaceAddParams {
            op_id: OpId::generate(),
            id,
            workspace: WorkspaceSpec {
                machine_id: devbox,
                path: "~/src/web".into(),
            },
        })
        .await
        .unwrap();
    let theirs =
        h.rt.terminal_open(open(id, added.id, ShellSandbox::Workspace, false))
            .await
            .unwrap();
    assert_eq!(theirs.name, "you@devbox-web");
    h.rt.workspace_remove(WorkspaceRemoveParams {
        op_id: OpId::generate(),
        id,
        workspace_id: added.id,
    })
    .await
    .unwrap();
    assert_eq!(names(&h.summary(id).shells), ["devbox"]);
}
