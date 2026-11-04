//! Sessions on workspaces: create on several machines (opened before the commit, default
//! shells, `session.info`), refusals (an unreachable machine, a bad path), adding and removing
//! a workspace, the agent's shells, a reconnect refreshing the workspaces, the bridge's pending
//! requests on resubscribe, and delete closing the workspaces on their hosts.

use controlroom_drivers::fake::FakeScript;
use controlroom_protocol::ErrorData;
use controlroom_protocol::agent::{ShellEndParams, ShellNewParams};
use controlroom_protocol::agentd::{WorkspaceAddParams, WorkspaceRemoveParams};
use controlroom_protocol::host::HostRequest;
use controlroom_protocol::ids::{CallId, OpId, RemotePath};
use controlroom_protocol::session::{SessionState, ShellState, WorkspaceSpec};
use controlroom_protocol::timeline::InjectOrigin;
use controlroom_protocol::ui::Connectivity;

use super::Harness;

#[tokio::test]
async fn a_session_spans_machines_with_default_shells_and_info() {
    let mut h = Harness::new(FakeScript::default()).await;
    let buildbox = h.machines.add("buildbox");
    let created =
        h.rt.create(h.create_params(&[
            (h.devbox, "~/src/api"),
            (buildbox, "~/work/web"),
            (h.devbox, "~/src/web"),
        ]))
        .await
        .unwrap();
    let id = created.session.id;
    let s = h.wait_state(id, SessionState::Idle).await;
    let roots: Vec<Option<&str>> = s
        .workspaces
        .iter()
        .map(|w| w.root.as_ref().map(RemotePath::as_str))
        .collect();
    assert_eq!(
        roots,
        [
            Some("/home/alice/src/api"),
            Some("/home/alice/work/web"),
            Some("/home/alice/src/web")
        ]
    );
    assert!(s.workspaces[0].primary && !s.workspaces[1].primary);
    let shells: Vec<&str> = s.shells.iter().map(|sh| sh.name.as_str()).collect();
    assert_eq!(shells, ["devbox-api", "buildbox", "devbox-web"]);

    let info = h.rt.session_info(id).await.unwrap();
    assert_eq!(info.workspaces[1].machine, "buildbox");
    assert_eq!(info.workspaces[1].shell, "buildbox");
    assert_eq!(
        info.workspaces[0].context_files[0].content,
        "rules of devbox"
    );
    assert_eq!(info.shells[2].machine, "devbox");
    // Every default shell exists on its host.
    let devbox = h.devbox;
    h.machines
        .wait("the shells", move |m| m.shells(devbox).len() == 2)
        .await;
    assert_eq!(h.machines.shells(buildbox).len(), 1);

    // The info and the summary follow the machines' availability.
    let mut watch = h.rt.info_watch(id).await.unwrap();
    h.machines.drop_link(buildbox);
    watch
        .wait_for(|info| {
            info.as_ref().is_some_and(|i| {
                i.workspaces[1].availability.connectivity == Connectivity::Unreachable
            })
        })
        .await
        .unwrap();
    assert!(!h.summary(id).workspaces[1].availability.is_connected());
}

#[tokio::test]
async fn creates_need_connected_machines_and_existing_directories() {
    let h = Harness::new(FakeScript::default()).await;
    let buildbox = h.machines.add("buildbox");
    h.machines.drop_link(buildbox);
    let err =
        h.rt.create(h.create_params(&[(h.devbox, "~/a"), (buildbox, "~/b")]))
            .await
            .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::MachineUnavailable { .. })),
        "{err:?}"
    );
    let err =
        h.rt.create(h.create_params(&[(h.devbox, "~/a"), (h.devbox, "~/missing")]))
            .await
            .unwrap_err();
    assert!(
        matches!(err.data, Some(ErrorData::FsError { .. })),
        "{err:?}"
    );
    let err = h.rt.create(h.create_params(&[])).await.unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::InvalidParams)));
    let feed = h.rt.subscribe_sessions(None).unwrap();
    assert!(feed.snapshot.unwrap().sessions.is_empty());
    // The workspace that did open is closed again.
    let devbox = h.devbox;
    h.machines
        .wait("the close", move |m| {
            m.methods(devbox).contains(&"workspace.close".to_owned())
        })
        .await;
}

#[tokio::test]
async fn workspaces_are_added_and_removed_with_a_notice() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let buildbox = h.machines.add("buildbox");
    let add = WorkspaceAddParams {
        op_id: OpId::generate(),
        id,
        workspace: WorkspaceSpec {
            machine_id: buildbox,
            path: RemotePath::new("~/work"),
        },
    };
    let added = h.rt.workspace_add(add.clone()).await.unwrap();
    assert_eq!(h.rt.workspace_add(add).await.unwrap(), added, "op dedup");
    assert_eq!(added.root, Some(RemotePath::new("/home/alice/work")));
    let info = h.rt.session_info(id).await.unwrap();
    assert_eq!(info.workspaces.len(), 2);
    let notices: Vec<(InjectOrigin, bool)> = h.rt.inner().with_core(|core| {
        core.sessions[&id]
            .data
            .injections
            .iter()
            .map(|i| (i.origin, i.wake))
            .collect()
    });
    assert_eq!(notices, [(InjectOrigin::Workspace, false)]);

    let primary = h.summary(id).workspaces[0].id;
    let err =
        h.rt.workspace_remove(WorkspaceRemoveParams {
            op_id: OpId::generate(),
            id,
            workspace_id: primary,
        })
        .await
        .unwrap_err();
    assert!(matches!(err.data, Some(ErrorData::InvalidParams)));
    h.rt.workspace_remove(WorkspaceRemoveParams {
        op_id: OpId::generate(),
        id,
        workspace_id: added.id,
    })
    .await
    .unwrap();
    let s = h.summary(id);
    assert_eq!((s.workspaces.len(), s.shells.len()), (1, 1));
    h.machines
        .wait("the close", move |m| {
            m.methods(buildbox).contains(&"workspace.close".to_owned())
        })
        .await;
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn the_agent_opens_lists_and_closes_shells() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let new = |name: &str| ShellNewParams {
        call_id: CallId::new(format!("new-{name}")),
        name: name.into(),
        machine: "devbox".into(),
        cwd: Some(RemotePath::new("/home/alice/src/app/sub")),
    };
    let shell = h.rt.shell_open(id, new("devbox-2")).await.unwrap();
    assert_eq!(shell.machine, "devbox");
    assert_eq!(shell.shell.cwd.as_str(), "/home/alice/src/app/sub");
    assert_eq!(shell.shell.state, ShellState::Idle);
    assert_eq!(
        h.rt.shell_open(id, new("devbox-2")).await.unwrap(),
        shell,
        "idempotent"
    );
    assert!(h.rt.shell_open(id, new("bad name")).await.is_err());
    let names: Vec<String> =
        h.rt.shell_list(id)
            .await
            .unwrap()
            .shells
            .into_iter()
            .map(|s| s.shell.name)
            .collect();
    assert_eq!(names, ["devbox", "devbox-2"]);
    let close = |name: &str| ShellEndParams {
        call_id: CallId::new(format!("end-{name}")),
        name: name.into(),
    };
    assert!(
        h.rt.shell_close(id, close("devbox")).await.is_err(),
        "the default stays"
    );
    h.rt.shell_close(id, close("devbox-2")).await.unwrap();
    assert_eq!(h.summary(id).shells.len(), 1);
    h.wait_state(id, SessionState::Idle).await;
}

#[tokio::test]
async fn a_reconnect_refreshes_workspaces_and_a_resubscribe_gets_pending_requests() {
    let mut h = Harness::new(FakeScript::default()).await;
    let id = h.create().await;
    let devbox = h.devbox;
    let opens = |h: &Harness| {
        h.machines
            .methods(h.devbox)
            .iter()
            .filter(|m| *m == "workspace.open")
            .count()
    };
    let before = opens(&h);
    h.machines.drop_link(devbox);
    // Requests wait on the bridge while the host is away.
    h.rt.inner()
        .bridge
        .send(controlroom_protocol::agentd::HostOpRequest {
            id: controlroom_protocol::ids::HostOpId::new("probe"),
            machine_id: devbox,
            session_id: None,
            request: HostRequest::ShellList(controlroom_protocol::host::ShellListParams {
                workspace_id: None,
            }),
        });
    let pending = h.rt.hostops_subscribe().requests;
    assert!(
        pending.iter().any(|r| r.id.as_str() == "probe"),
        "{pending:?}"
    );
    h.machines.reconnect(devbox);
    h.machines
        .wait("the refresh", move |m| {
            m.methods(devbox)
                .iter()
                .filter(|m| *m == "workspace.open")
                .count()
                > before
        })
        .await;
    assert!(opens(&h) > before);
    h.rt.delete(h.op(id)).await.unwrap();
    h.machines
        .wait("the close", move |m| {
            m.methods(devbox).contains(&"workspace.close".to_owned())
        })
        .await;
}
