//! Keeping up with a host across connections, and what its notifications do.
//!
//! **After every new host connection** (a new `since` or epoch in `hostops.machines`) the
//! runtime renews what lived on the old connection: `jobs.subscribe {since: cursor}` (the job
//! event log from the persisted cursor; see `jobs::events`), `approvals.subscribe` (pending
//! approvals), `workspace.open` of every open session's workspaces there (idempotent; refreshes
//! instruction files and skills), and `shell.open` of shells the host's new epoch doesn't know.
//! Requests of an earlier connection still pending are withdrawn first.
//!
//! **Job events** arriving before the subscription's result are buffered and applied after its
//! backlog; events of an older subscription are ignored. The consumed revision is acknowledged
//! (`jobs.ack`) by the tick.
//!
//! **Notifications** (`hostops.notify`): `tool.output` and `tool.waiting_input` go to the
//! calling session's extension (the latter also raises a `terminal_input` request),
//! `shell.changed` updates the shell, `jobs.event` feeds the job event log, and
//! `approval.request` / `approval.closed` raise and end sandbox approvals.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlroom_protocol::RpcError;
use controlroom_protocol::agentd::HostOpRequest;
use controlroom_protocol::host::{
    ApprovalsSubscribeResult, HostNotification, HostRequest, JobEvent, JobsAckParams,
    JobsSubscribeParams, JobsSubscribeResult,
};
use controlroom_protocol::ids::{HostOpId, MachineId, Rev, SessionId, SubscriptionId};
use controlroom_protocol::jsonrpc::NoParams;
use serde_json::Value;

use super::calls::parse;
use super::shells::open_shells;
use crate::core::Core;
use crate::errors::internal;
use crate::runtime::{Inner, Runtime};
use crate::session::AgentEvent;

/// The state of one host's subscriptions.
#[derive(Debug, Clone, Default)]
pub(crate) struct HostSync {
    /// The `jobs.subscribe` of the current connection.
    jobs_request: Option<HostOpId>,
    /// Its subscription, once answered.
    jobs_subscription: Option<SubscriptionId>,
    /// Job events that arrived before the subscription's result.
    buffered: Vec<(SubscriptionId, Vec<JobEvent>)>,
    /// The `approvals.subscribe` of the current connection.
    approvals_request: Option<HostOpId>,
    /// The revision last acknowledged with `jobs.ack`.
    acked: Rev,
}

/// Renews a host's subscriptions and workspaces after a new connection.
pub(crate) async fn connected(inner: Arc<Inner>, machine: MachineId) {
    let started = inner.transact(|core| core.begin_sync(machine));
    let Ok((jobs, approvals, cursor)) = started else {
        return;
    };
    let jobs_task = {
        let inner = inner.clone();
        let request = HostRequest::JobsSubscribe(JobsSubscribeParams { since: cursor });
        async move {
            let outcome = request_outcome(&inner, machine, jobs.clone(), request).await;
            match outcome.and_then(parse::<JobsSubscribeResult>) {
                Ok(result) => {
                    if let Err(e) =
                        inner.transact(|core| core.jobs_subscribed(machine, &jobs, result))
                    {
                        tracing::warn!("machine {machine}: applying its job events: {e}");
                    }
                }
                Err(e) => tracing::warn!("machine {machine}: jobs.subscribe: {e}"),
            }
        }
    };
    let approvals_task = {
        let inner = inner.clone();
        let request = HostRequest::ApprovalsSubscribe(NoParams {});
        async move {
            let outcome = request_outcome(&inner, machine, approvals.clone(), request).await;
            match outcome.and_then(parse::<ApprovalsSubscribeResult>) {
                Ok(result) => {
                    let _ = inner.transact(|core| {
                        if core
                            .sync
                            .get(&machine)
                            .and_then(|s| s.approvals_request.as_ref())
                            != Some(&approvals)
                        {
                            return Ok(());
                        }
                        core.host_approvals(machine, result.pending)
                    });
                }
                Err(e) => tracing::warn!("machine {machine}: approvals.subscribe: {e}"),
            }
        }
    };
    futures::future::join3(
        jobs_task,
        approvals_task,
        refresh_workspaces(inner.clone(), machine),
    )
    .await;
}

/// Sends a host-level request and waits for its outcome (`Err` when withdrawn).
async fn request_outcome(
    inner: &Arc<Inner>,
    machine: MachineId,
    key: HostOpId,
    request: HostRequest,
) -> Result<Value, RpcError> {
    let response = inner.bridge.request(HostOpRequest {
        id: key,
        machine_id: machine,
        session_id: None,
        request,
    });
    response
        .await
        .map_err(|_| internal("withdrawn by a newer connection"))?
}

/// Reopens the workspaces (and shells) of the open sessions on `machine`.
async fn refresh_workspaces(inner: Arc<Inner>, machine: MachineId) {
    refresh(inner, |w| w.machine_id == machine, None).await;
}

/// Reopens one session's workspaces (its sandbox changed) and shells.
pub(crate) async fn refresh_session(inner: Arc<Inner>, id: SessionId) {
    refresh(inner, |_| true, Some(id)).await;
}

async fn refresh(
    inner: Arc<Inner>,
    which: impl Fn(&super::records::WorkspaceRecord) -> bool,
    only: Option<SessionId>,
) {
    let due: Vec<(SessionId, super::records::WorkspaceRecord, _)> = inner.with_core(|core| {
        core.sessions
            .values()
            .filter(|s| !s.data.closed() && only.is_none_or(|id| id == s.data.id))
            .flat_map(|s| {
                let sandbox = s.data.effective_sandbox();
                s.data
                    .workspaces
                    .iter()
                    .filter(|w| which(w))
                    .map(move |w| (s.data.id, w.clone(), sandbox.clone()))
            })
            .collect()
    });
    let mut sessions: Vec<SessionId> = due.iter().map(|(id, ..)| *id).collect();
    sessions.dedup();
    let opens = due.into_iter().map(|(id, workspace, sandbox)| {
        let inner = inner.clone();
        async move {
            match super::workspaces::open_workspace(&inner, id, &workspace, sandbox).await {
                Ok(open) => {
                    let _ = inner.transact(|core| core.workspace_refreshed(id, workspace.id, open));
                }
                Err(e) => tracing::warn!("session {id}: reopening {}: {e}", workspace.path),
            }
        }
    });
    futures::future::join_all(opens).await;
    let shells = sessions
        .into_iter()
        .map(|id| open_shells(inner.clone(), id));
    futures::future::join_all(shells).await;
}

impl Runtime {
    /// `hostops.notify`: a notification from `machine`'s host daemon.
    pub fn hostops_notify(&self, machine: MachineId, notification: HostNotification) {
        let inner = self.inner();
        let result = match notification {
            HostNotification::ToolOutput(params) => {
                inner.with_core(|core| {
                    core.forward(&params.call_id, AgentEvent::Output(params.clone()))
                });
                Ok(())
            }
            HostNotification::ToolWaitingInput(params) => {
                inner.with_core(|core| {
                    core.forward(&params.call_id, AgentEvent::WaitingInput(params.clone()))
                });
                inner.transact(|core| core.waiting_input(&params))
            }
            HostNotification::ShellChanged(params) => {
                inner.transact(|core| core.shell_changed(&params.shell))
            }
            HostNotification::JobsEvent(params) => inner.transact(|core| {
                core.jobs_notified(machine, params.subscription_id, params.events)
            }),
            HostNotification::ApprovalRequest(params) => {
                inner.transact(|core| core.host_approval(machine, params.approval))
            }
            HostNotification::ApprovalClosed(params) => {
                inner.transact(|core| core.host_approval_closed(&params.approval_id, params.state))
            }
        };
        if let Err(e) = result {
            tracing::warn!("machine {machine}: handling a notification: {e}");
        }
    }
}

impl Core {
    /// Starts a new connection's sync: withdraws the old connection's subscriptions and
    /// returns the new request ids and the job cursor.
    fn begin_sync(
        &mut self,
        machine: MachineId,
    ) -> Result<(HostOpId, HostOpId, Option<Rev>), RpcError> {
        let cursor = self.store.cursor(machine).map_err(internal)?;
        let old = self.sync.entry(machine).or_default().clone();
        for key in [old.jobs_request, old.approvals_request]
            .into_iter()
            .flatten()
        {
            self.effect(crate::core::HostEffect::Withdraw(key));
        }
        let suffix = ulid::Ulid::generate();
        let jobs = HostOpId::new(format!("jobs.subscribe:{machine}:{suffix}"));
        let approvals = HostOpId::new(format!("approvals.subscribe:{machine}:{suffix}"));
        let sync = self.sync.entry(machine).or_default();
        sync.jobs_request = Some(jobs.clone());
        sync.jobs_subscription = None;
        sync.buffered.clear();
        sync.approvals_request = Some(approvals.clone());
        Ok((jobs, approvals, cursor))
    }

    /// The result of the current connection's `jobs.subscribe`: applies the backlog, then the
    /// events that arrived meanwhile.
    fn jobs_subscribed(
        &mut self,
        machine: MachineId,
        key: &HostOpId,
        result: JobsSubscribeResult,
    ) -> Result<(), RpcError> {
        let Some(sync) = self.sync.get_mut(&machine) else {
            return Ok(());
        };
        if sync.jobs_request.as_ref() != Some(key) {
            return Ok(());
        }
        sync.jobs_subscription = Some(result.subscription_id);
        let buffered: Vec<JobEvent> = std::mem::take(&mut sync.buffered)
            .into_iter()
            .filter(|(s, _)| *s == result.subscription_id)
            .flat_map(|(_, events)| events)
            .collect();
        let mut events = result.events;
        events.extend(buffered);
        self.job_events(machine, events)
    }

    /// `jobs.event`: applied when it belongs to the current subscription, buffered while the
    /// subscription's result is on its way.
    fn jobs_notified(
        &mut self,
        machine: MachineId,
        subscription: SubscriptionId,
        events: Vec<JobEvent>,
    ) -> Result<(), RpcError> {
        let Some(sync) = self.sync.get_mut(&machine) else {
            return Ok(());
        };
        match sync.jobs_subscription {
            Some(current) if current == subscription => self.job_events(machine, events),
            None if sync.jobs_request.is_some() => {
                sync.buffered.push((subscription, events));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Acknowledges consumed job events of connected hosts (the tick).
    pub(crate) fn send_job_acks(&mut self) -> Result<(), RpcError> {
        let machines: Vec<MachineId> = self.sync.keys().copied().collect();
        let mut sends = BTreeMap::new();
        for machine in machines {
            if !self.machines.connected(&machine) {
                continue;
            }
            let key = HostOpId::new(format!("jobs.ack:{machine}"));
            let Some(cursor) = self.store.cursor(machine).map_err(internal)? else {
                continue;
            };
            let Some(sync) = self.sync.get_mut(&machine) else {
                continue;
            };
            if sync.jobs_subscription.is_none()
                || cursor <= sync.acked
                || self.bridge.is_pending(&key)
            {
                continue;
            }
            sync.acked = cursor;
            sends.insert(machine, (key, cursor));
        }
        for (machine, (key, rev)) in sends {
            self.host_send(
                machine,
                None,
                key,
                HostRequest::JobsAck(JobsAckParams { rev }),
            );
        }
        Ok(())
    }

    /// Sends an event to the extension of the session whose call it is about.
    fn forward(&self, call_id: &controlroom_protocol::ids::CallId, event: AgentEvent) {
        let session = self.sessions.values().find(|s| {
            s.live
                .calls
                .values()
                .any(|c| c.tool.as_ref().is_some_and(|t| t.call_id == *call_id))
        });
        if let Some(session) = session {
            let _ = session.events.send(event);
        }
    }
}
