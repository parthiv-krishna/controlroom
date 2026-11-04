//! The old UI API's `session.create` (a legacy daemon's session): create the session with its
//! first prompt, then link it to tasks.
//!
//! The daemon creates the session and queues its first prompt in one transaction under the
//! create's `op_id`, so a retry returns the original session and the prompt is sent once. The
//! controller fills in the sandbox defaults and the prompt's origin (this connection's device,
//! replacing whatever a client sent). Task links are set-like and a task only moves from
//! `backlog`/`todo`, so retrying the whole call is safe. Links follow the create: a session
//! must exist before it can be linked.

use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::daemon::SessionCreateResult;
use controlroom_protocol::daemon::methods::SessionCreate;
use controlroom_protocol::error::Resource;
use controlroom_protocol::session::PromptOrigin;
use controlroom_protocol::ui::{SessionLink, TaskStatus, UiSessionCreateParams};

use super::{Conn, sandbox};
use crate::errors::{call_error, not_found, store_error};
use crate::store::TaskLink;

pub(super) async fn create(
    conn: &Arc<Conn>,
    p: UiSessionCreateParams,
) -> Result<SessionCreateResult, RpcError> {
    let hub = &conn.hub;
    let machine_id = p.machine_id;
    for task in &p.task_ids {
        hub.task(*task)?;
    }
    let mut create = *p.create;
    // The controller's defaults and attribution replace whatever a client sent.
    create.sandbox = Some(sandbox::defaults(hub, &[machine_id], &p.task_ids)?);
    if let Some(prompt) = &mut create.prompt {
        prompt.origin = PromptOrigin::User {
            device: conn.device_ref(),
        };
    }
    let daemon = hub.daemon(machine_id)?;
    let created = daemon
        .call::<SessionCreate>(&create)
        .await
        .map_err(|e| call_error(e, machine_id, || hub.connectivity(machine_id)))?;
    let link = SessionLink {
        machine_id,
        session_id: created.session.id,
    };
    for task in &p.task_ids {
        let mut st = hub.lock();
        let repo = hub.repo();
        let tasks = repo.tasks();
        if !tasks
            .link(*task, &TaskLink::legacy(&link))
            .map_err(store_error)?
        {
            return Err(not_found(Resource::Task, task));
        }
        tasks
            .move_if_in(
                *task,
                &[TaskStatus::Backlog, TaskStatus::Todo],
                TaskStatus::Doing,
            )
            .map_err(store_error)?;
        let row = hub.task(*task)?;
        hub.emit_task(&mut st, &row);
    }
    Ok(created)
}
