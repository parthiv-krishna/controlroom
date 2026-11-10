//! Workspaces (`workspace.open`, `workspace.close`): one session's directory on this host with
//! its own nono profile, webhook secret, `TMPDIR` and fs worker.
//!
//! `workspace.open` is idempotent by id: again with a changed sandbox it rewrites the profiles
//! and restarts the fs worker (shells and jobs started later use the new profiles; running ones
//! keep theirs until reopened). A changed **level** also restarts the agent's shells under the
//! new profile (see [`super::shells::Shells::restart_agent_shells`]); the user's terminals and
//! running jobs keep the sandbox they started with until they end. Open workspaces are
//! persisted, so a restarted daemon keeps serving them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use controlroom_protocol::host::{WorkspaceCloseParams, WorkspaceOpenParams, WorkspaceOpenResult};
use controlroom_protocol::ids::{SessionId, WorkspaceId};
use controlroom_protocol::session::SandboxSpec;
use controlroom_protocol::{ErrorData, RemotePath, Resource, RpcError};
use controlroom_sandbox::webhook::{HookSecret, HookUrl};
use controlroom_sandbox::{AbsPath, SandboxLevel};
use rusqlite::params;

use super::Host;
use super::approvals::PathRules;
use super::context::{context_files, skills};
use super::fsops::fs_error;
use super::sandbox::{self, ProfileInput, Wrapped};
use super::shells::integration;
use super::store::internal;
use super::worker::WorkerClient;
use crate::dirs::WorkspacePaths;
use crate::dirs::{create_private_dir, expand_home, write_private_file};

/// An open workspace.
pub struct Workspace {
    /// The workspace.
    pub id: WorkspaceId,
    /// Its session.
    pub session_id: SessionId,
    /// The resolved root.
    pub root: AbsPath,
    /// Its files under the host state dir.
    pub files: WorkspacePaths,
    /// The level its profiles were written at.
    pub level: SandboxLevel,
    state: Mutex<State>,
}

struct State {
    /// The generated profile; `None` at level off.
    profile: Option<AbsPath>,
    /// The profile of the user's sandboxed terminals; `None` at level off.
    user_profile: Option<AbsPath>,
    worker: Option<Arc<WorkerClient>>,
}

impl Workspace {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The profile processes of this workspace run under (`None`: unsandboxed).
    pub fn profile(&self) -> Option<AbsPath> {
        self.lock().profile.clone()
    }

    /// Wraps `argv` in this workspace's sandbox.
    pub fn wrap(&self, host: &Host, argv: Vec<String>) -> Result<Wrapped, RpcError> {
        sandbox::wrap(
            &host.sandbox,
            &host.dirs,
            self.profile().as_ref(),
            &self.root,
            argv,
        )
    }

    /// Wraps `argv` in this workspace's sandbox for one of the user's terminals (the same
    /// policy; approvals are marked as the user's).
    pub fn wrap_user(&self, host: &Host, argv: Vec<String>) -> Result<Wrapped, RpcError> {
        let profile = self.lock().user_profile.clone();
        sandbox::wrap(
            &host.sandbox,
            &host.dirs,
            profile.as_ref(),
            &self.root,
            argv,
        )
    }

    /// The fs worker, started on first use and again if it died.
    pub fn worker(&self, host: &Host) -> Result<Arc<WorkerClient>, RpcError> {
        if let Some(worker) = self.lock().worker.as_ref().filter(|w| w.is_alive()) {
            return Ok(worker.clone());
        }
        let argv = vec![
            host.exe.to_string(),
            "workspace-exec".to_owned(),
            "--root".to_owned(),
            self.root.to_string(),
            "--home".to_owned(),
            host.dirs.home().to_string(),
        ];
        let wrapped = self.wrap(host, argv)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.files.worker_log().as_path())
            .map_err(internal)?;
        let child = tokio::process::Command::new(&wrapped.argv[0])
            .args(&wrapped.argv[1..])
            .envs(wrapped.env)
            .current_dir(self.root.as_path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| sandbox::unavailable(format!("starting the fs worker: {e}")))?;
        let worker = Arc::new(WorkerClient::new(child).map_err(internal)?);
        self.lock().worker = Some(worker.clone());
        Ok(worker)
    }
}

/// The open workspaces.
#[derive(Default)]
pub struct Workspaces(Mutex<HashMap<WorkspaceId, Arc<Workspace>>>);

impl Workspaces {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<WorkspaceId, Arc<Workspace>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An open workspace.
    pub fn get(&self, id: &WorkspaceId) -> Option<Arc<Workspace>> {
        self.lock().get(id).cloned()
    }

    /// An open workspace, or `not_found {workspace}`.
    pub fn require(&self, id: &WorkspaceId) -> Result<Arc<Workspace>, RpcError> {
        self.get(id).ok_or_else(|| not_found(id))
    }

    /// Every open workspace.
    pub fn all(&self) -> Vec<Arc<Workspace>> {
        self.lock().values().cloned().collect()
    }
}

fn not_found(id: &WorkspaceId) -> RpcError {
    RpcError::new(
        ErrorData::NotFound {
            resource: Resource::Workspace,
            id: id.to_string(),
        },
        "no such workspace on this host",
    )
}

/// Reopens the persisted workspaces at startup (profiles are rewritten: the webhook port may
/// have changed).
pub fn restore(host: &Host) -> Result<(), RpcError> {
    let rows = host.store.run_blocking(|c| {
        let mut stmt = c.prepare("SELECT id, session_id, root, sandbox FROM workspaces")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    for (id, session, root, sandbox) in rows {
        let (Ok(id), Ok(session_id), Ok(root), Ok(sandbox)) = (
            id.parse::<WorkspaceId>(),
            session.parse::<SessionId>(),
            AbsPath::new(&root),
            serde_json::from_str::<SandboxSpec>(&sandbox),
        ) else {
            tracing::warn!("skipping an unreadable workspace row");
            continue;
        };
        if let Err(e) = install(host, id, session_id, root, sandbox) {
            tracing::warn!("workspace {id}: {}", e.message);
        }
    }
    Ok(())
}

/// Prepares the workspace's files and profile and registers it (replacing an older entry).
fn install(
    host: &Host,
    id: WorkspaceId,
    session_id: SessionId,
    root: AbsPath,
    sandbox: SandboxSpec,
) -> Result<Arc<Workspace>, RpcError> {
    let files = host.dirs.workspace(&id.to_string());
    for dir in [
        files.dir().clone(),
        files.tmp(),
        files.view().join("cmds"),
        files.view().join("calls"),
    ] {
        create_private_dir(&dir).map_err(|e| fs_error(dir.as_str(), e))?;
    }
    std::fs::write(files.bashrc().as_path(), integration::BASHRC)
        .map_err(|e| fs_error(files.bashrc().as_str(), e))?;
    // The agent's processes and the user's sandboxed terminals: the same policy, each with its
    // own approval webhook URL (their approvals are told apart by the secret).
    // Both get the same grants and runtime reads; the host's path rules (never-paths, and later
    // "Always allow" and "Never" answers) apply to both.
    let home = host.dirs.home();
    let policy = sandbox::policy(&sandbox, home)?;
    let rules = PathRules::new(sandbox::never_paths(&sandbox, home));
    let write = |path: AbsPath, secret: AbsPath| {
        let hook = match host.approvals.port() {
            Some(port) => Some(HookUrl {
                port,
                session: controlroom_sandbox::SessionId::new(&id.to_string())
                    .map_err(|e| internal(e.to_string()))?,
                secret: hook_secret(&secret)?,
            }),
            None => None,
        };
        sandbox::write_profile(
            &host.sandbox,
            &host.dirs,
            ProfileInput {
                id: &id.to_string(),
                root: &root,
                files: &files,
                path,
                policy: policy.clone(),
                hook,
            },
        )
    };
    let profile = write(files.profile(), files.hook_secret())?;
    let user_profile = write(files.user_profile(), files.user_hook_secret())?;
    let workspace = Arc::new(Workspace {
        id,
        session_id,
        root,
        files,
        level: policy.level,
        state: Mutex::new(State {
            profile,
            user_profile,
            worker: None,
        }),
    });
    host.approvals.set_rules(id, rules);
    host.workspaces.lock().insert(id, workspace.clone());
    Ok(workspace)
}

/// A webhook secret of the workspace (in the file `path`), created on first use.
fn hook_secret(path: &AbsPath) -> Result<HookSecret, RpcError> {
    if let Some(secret) = std::fs::read_to_string(path.as_path())
        .ok()
        .and_then(|s| HookSecret::parse(s.trim()))
    {
        return Ok(secret);
    }
    let secret = HookSecret::generate().map_err(|e| internal(e.to_string()))?;
    write_private_file(path, secret.as_str().as_bytes()).map_err(internal)?;
    Ok(secret)
}

/// `workspace.open`.
pub async fn open(
    host: &Arc<Host>,
    params: WorkspaceOpenParams,
) -> Result<WorkspaceOpenResult, RpcError> {
    let home = host.dirs.home().clone();
    let requested = params.path.as_str().to_owned();
    let root = expand_home(&requested, &home)
        .map_err(|e| fs_error(&requested, e))
        .and_then(|p| std::fs::canonicalize(p.as_path()).map_err(|e| fs_error(&requested, e)))
        .and_then(|p| AbsPath::from_path(&p).map_err(|e| fs_error(&requested, e)))?;
    if !root.as_path().is_dir() {
        return Err(fs_error(&requested, "not a directory"));
    }
    if let Some(existing) = host.workspaces.get(&params.workspace_id)
        && existing.session_id != params.session_id
    {
        return Err(RpcError::new(
            ErrorData::InvalidParams,
            "this workspace belongs to another session",
        ));
    }
    let id = params.workspace_id;
    let previous = host.workspaces.get(&id).map(|w| w.level);
    let installed = {
        let host = host.clone();
        let (root, sandbox) = (root.clone(), params.sandbox.clone());
        tokio::task::spawn_blocking(move || install(&host, id, params.session_id, root, sandbox))
            .await
            .map_err(internal)??
    };
    let sandbox_json = serde_json::to_string(&params.sandbox).map_err(internal)?;
    let (session, path, root_text) = (
        params.session_id.to_string(),
        requested.clone(),
        root.to_string(),
    );
    host.store
        .run(move |c| {
            c.execute(
                "INSERT INTO workspaces (id, session_id, path, root, sandbox)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET sandbox = excluded.sandbox",
                params![id.to_string(), session, path, root_text, sandbox_json],
            )
        })
        .await?;
    if previous.is_some_and(|level| level != installed.level) {
        host.shells
            .restart_agent_shells(host, &id, "the sandbox level changed")
            .await;
    }
    drop(installed);
    let scan_root = root.clone();
    let (context_files, skills) = tokio::task::spawn_blocking(move || {
        let root: &Path = scan_root.as_path();
        (context_files(root), skills(root))
    })
    .await
    .map_err(internal)?;
    Ok(WorkspaceOpenResult {
        root: RemotePath::from(root.as_str()),
        home: RemotePath::from(home.as_str()),
        context_files,
        skills,
    })
}

/// `workspace.close`: stops its shells and jobs, withdraws its approvals and forgets it.
pub async fn close(host: &Arc<Host>, params: WorkspaceCloseParams) -> Result<(), RpcError> {
    let id = params.workspace_id;
    host.shells.close_workspace(host, &id).await;
    host.jobs.kill_workspace(host, &id).await;
    host.approvals.withdraw_workspace(&host.conns, &id);
    let removed = host.workspaces.lock().remove(&id);
    host.store
        .run(move |c| c.execute("DELETE FROM workspaces WHERE id = ?1", [id.to_string()]))
        .await?;
    if let Some(workspace) = removed {
        let dir = workspace.files.dir().clone();
        drop(workspace);
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir.as_path())).await;
    }
    Ok(())
}
