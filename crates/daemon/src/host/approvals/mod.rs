//! Sandbox approvals of the host's workspaces (spec §11.4): a loopback HTTP listener nono
//! `POST`s its approval requests to, at `http://127.0.0.1:<port>/nono/v1/<workspace>/<secret>`,
//! and the host API side (`approvals.subscribe`, `approval.request`, `approval.closed`,
//! `approval.respond`).
//!
//! - **Correlation and trust.** The URL names the workspace; its secret is generated when the
//!   workspace is opened (`workspaces/<id>/hook.secret`, outside the sandbox) and compared in
//!   constant time. Anything else is answered 403/404 (nono treats non-2xx as a denial).
//! - **Flow.** The request becomes a pending [`HostApproval`] announced to the subscribed
//!   controller; the first `approval.respond` is the reply. The reply goes out before nono's
//!   `X-Nono-Timeout-Secs` (minus a margin); if nobody answered by then, or nono hangs up, the
//!   approval closes as `expired`. Closing the workspace withdraws its approvals.
//! - **Restarts.** The port is kept in `host/webhook.port` and bound again on the next start, so
//!   shells and jobs that outlive the daemon still reach it. Pending approvals die with their
//!   HTTP connection; the agent daemon resubscribes and sees only live ones.

mod http;
mod mapping;

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use controlroom_protocol::host::{
    ApprovalClosedParams, ApprovalRequestParams, ApprovalRespondParams, ApprovalsSubscribeResult,
    HostApproval, notifications,
};
use controlroom_protocol::ids::{ApprovalId, SubscriptionId, UnixMillis, WorkspaceId};
use controlroom_protocol::session::RequestState;
use controlroom_protocol::{ErrorData, Resource, RpcError};
use controlroom_sandbox::webhook::{ApprovalReply, ApprovalRequest, HookSecret, parse_hook_path};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, oneshot};

pub use mapping::{ALLOW_ONCE, ALLOW_SESSION, DENY};
use mapping::{Outcome, reply, show};

use super::Host;
use super::conns::Conns;
use crate::dirs::write_private_file;

/// Connections served at once; further ones wait in the accept queue.
const MAX_CONNECTIONS: usize = 64;
/// Time a client gets to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Waiting budget when nono doesn't say how long it waits.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Reply this much before nono's own timeout (at most a quarter of it).
const REPLY_MARGIN: Duration = Duration::from_secs(2);

struct Pending {
    approval: HostApproval,
    answer: oneshot::Sender<Outcome>,
}

#[derive(Default)]
struct Inner {
    /// By id; ULIDs sort by creation, so this is oldest first.
    pending: BTreeMap<ApprovalId, Pending>,
    /// Subscribed connections and their subscription ids.
    subscribers: HashMap<u64, SubscriptionId>,
}

/// Pending approvals and their subscribers.
#[derive(Default)]
pub struct Approvals {
    inner: Mutex<Inner>,
    /// The webhook's port, when it runs.
    port: Mutex<Option<u16>>,
}

impl Approvals {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The webhook's port, when it runs (without it, approve rules deny).
    pub fn port(&self) -> Option<u16> {
        *self.port.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `approvals.subscribe` from connection `conn`: the pending approvals, then notifications.
    pub fn subscribe(&self, conn: u64) -> ApprovalsSubscribeResult {
        let mut inner = self.lock();
        let subscription_id = SubscriptionId::generate();
        inner.subscribers.insert(conn, subscription_id);
        ApprovalsSubscribeResult {
            subscription_id,
            pending: inner.pending.values().map(|p| p.approval.clone()).collect(),
        }
    }

    /// Ends connection `conn`'s subscription with this id; false if it has none.
    pub fn unsubscribe(&self, conn: u64, id: &SubscriptionId) -> bool {
        let mut inner = self.lock();
        if inner.subscribers.get(&conn) == Some(id) {
            inner.subscribers.remove(&conn);
            true
        } else {
            false
        }
    }

    /// `approval.respond`: the first answer wins.
    pub fn respond(&self, params: ApprovalRespondParams) -> Result<(), RpcError> {
        let mut inner = self.lock();
        let Some(pending) = inner.pending.get(&params.approval_id) else {
            return Err(RpcError::new(
                ErrorData::NotFound {
                    resource: Resource::Approval,
                    id: params.approval_id.to_string(),
                },
                "no such pending approval",
            ));
        };
        if !pending
            .approval
            .options
            .iter()
            .any(|o| o.id == params.option_id)
        {
            return Err(RpcError::new(
                ErrorData::InvalidParams,
                format!("{:?} is not an option of this approval", params.option_id),
            ));
        }
        let pending = inner
            .pending
            .remove(&params.approval_id)
            .expect("checked above");
        let _ = pending.answer.send(Outcome::Chosen {
            option_id: params.option_id,
            reason: params.reason,
        });
        Ok(())
    }

    /// Withdraws the pending approvals of a closed workspace.
    pub fn withdraw_workspace(&self, conns: &Conns, workspace: &WorkspaceId) {
        let ids: Vec<ApprovalId> = self
            .lock()
            .pending
            .iter()
            .filter(|(_, p)| p.approval.workspace_id == *workspace)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.close(conns, &id, RequestState::Withdrawn);
        }
    }

    /// Closes a pending approval without an answer and tells the subscribers.
    fn close(&self, conns: &Conns, id: &ApprovalId, state: RequestState) {
        let Some(pending) = self.lock().pending.remove(id) else {
            return;
        };
        let outcome = if state == RequestState::Withdrawn {
            Outcome::Withdrawn
        } else {
            Outcome::Expired
        };
        let _ = pending.answer.send(outcome);
        self.notify(conns, |subscription_id| {
            Notice::Closed(ApprovalClosedParams {
                subscription_id,
                approval_id: *id,
                state,
            })
        });
    }

    fn notify(&self, conns: &Conns, notice: impl Fn(SubscriptionId) -> Notice) {
        let subscribers: Vec<(u64, SubscriptionId)> = self
            .lock()
            .subscribers
            .iter()
            .map(|(c, s)| (*c, *s))
            .collect();
        for (conn, subscription) in subscribers {
            let Some(handle) = conns.get(conn) else {
                self.lock().subscribers.remove(&conn);
                continue;
            };
            match notice(subscription) {
                Notice::Request(p) => handle.notify::<notifications::ApprovalRequest>(*p),
                Notice::Closed(p) => handle.notify::<notifications::ApprovalClosed>(p),
            }
        }
    }
}

enum Notice {
    Request(Box<ApprovalRequestParams>),
    Closed(ApprovalClosedParams),
}

/// Binds the listener on loopback: the port of the previous run if it is free, else a new one
/// (saved for the next run).
pub async fn bind(host: &Host) -> std::io::Result<TcpListener> {
    let file = host.paths.webhook_port();
    let saved: Option<u16> = std::fs::read_to_string(file.as_path())
        .ok()
        .and_then(|text| text.trim().parse().ok());
    let listener = match saved {
        Some(port) => match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => listener,
            Err(_) => TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?,
        },
        None => TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?,
    };
    let port = listener.local_addr()?.port();
    if saved != Some(port) {
        write_private_file(&file, port.to_string().as_bytes())?;
    }
    *host
        .approvals
        .port
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(port);
    Ok(listener)
}

/// Serves approval requests until the task is dropped.
pub async fn serve(listener: TcpListener, host: Arc<Host>) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(permit) = slots.clone().acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok((stream, peer)) => {
                let host = host.clone();
                tokio::spawn(async move {
                    handle(stream, peer, &host).await;
                    drop(permit);
                });
            }
            Err(e) => tracing::warn!("webhook accept: {e}"),
        }
    }
}

async fn handle(mut stream: TcpStream, peer: SocketAddr, host: &Arc<Host>) {
    if !peer.ip().is_loopback() {
        return;
    }
    let request = match tokio::time::timeout(READ_TIMEOUT, http::read_request(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(e)) => {
            if let Some(status) = e.status() {
                let _ = http::write_response(&mut stream, status, b"{}").await;
            }
            return;
        }
        Err(_) => return,
    };
    let (status, reply) = match route(&request, host) {
        Ok((approval, workspace)) => {
            let budget = reply_budget(request.header("x-nono-timeout-secs"));
            match decide(host, workspace, &approval, budget, &mut stream).await {
                Some(reply) => (200, reply),
                // nono hung up: nobody to answer.
                None => return,
            }
        }
        Err(status) => (status, ApprovalReply::denied("rejected by controlroomd")),
    };
    if let Err(e) = http::write_response(&mut stream, status, &reply.to_json()).await {
        tracing::warn!("webhook reply: {e}");
    }
}

/// Checks method, path and secret, and decodes the body. The error is the status to answer.
fn route(
    request: &http::Request,
    host: &Host,
) -> Result<
    (
        ApprovalRequest,
        (WorkspaceId, controlroom_protocol::ids::SessionId),
    ),
    u16,
> {
    if request.method != "POST" {
        return Err(405);
    }
    let (id, secret) = parse_hook_path(&request.path).ok_or(404u16)?;
    let workspace: WorkspaceId = id.as_str().parse().map_err(|_| 404u16)?;
    let stored = std::fs::read_to_string(host.paths.workspace(id.as_str()).hook_secret().as_path())
        .ok()
        .and_then(|s| HookSecret::parse(s.trim()));
    let session = host.workspaces.get(&workspace).map(|w| w.session_id);
    let (Some(stored), Some(session)) = (stored, session) else {
        tracing::warn!("webhook: a request for unknown workspace {workspace}");
        return Err(403);
    };
    if !stored.matches(&secret) {
        tracing::warn!("webhook: a request for workspace {workspace} with a wrong secret");
        return Err(403);
    }
    let approval = ApprovalRequest::from_json(&request.body).map_err(|e| {
        tracing::warn!("webhook: workspace {workspace}: {e}");
        400u16
    })?;
    Ok((approval, (workspace, session)))
}

/// Raises the approval and waits for its first resolution, at most `budget`. `None` when the
/// client hung up first (the approval then expires).
async fn decide(
    host: &Host,
    (workspace_id, session_id): (WorkspaceId, controlroom_protocol::ids::SessionId),
    request: &ApprovalRequest,
    budget: Duration,
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Option<ApprovalReply> {
    let Some(shown) = show(request) else {
        tracing::info!("workspace {workspace_id}: denying an approval it can't show");
        return Some(ApprovalReply::denied(
            "controlroom can't show this kind of request",
        ));
    };
    let approval = HostApproval {
        id: ApprovalId::generate(),
        session_id,
        workspace_id,
        title: shown.title,
        detail: shown.detail,
        body: shown.body,
        options: shown.options,
        created_at: UnixMillis::now(),
    };
    let id = approval.id;
    let (answer, answered) = oneshot::channel();
    let approvals = &host.approvals;
    approvals.lock().pending.insert(
        id,
        Pending {
            approval: approval.clone(),
            answer,
        },
    );
    tracing::info!(
        "workspace {workspace_id}: approval {id}: {}",
        approval.title
    );
    approvals.notify(&host.conns, |subscription_id| {
        Notice::Request(Box::new(ApprovalRequestParams {
            subscription_id,
            approval: approval.clone(),
        }))
    });
    let outcome = tokio::select! {
        outcome = answered => outcome.unwrap_or(Outcome::Expired),
        () = tokio::time::sleep(budget) => {
            approvals.close(&host.conns, &id, RequestState::Expired);
            Outcome::Expired
        }
        () = http::closed(stream) => {
            tracing::info!("workspace {workspace_id}: the approval's client hung up");
            approvals.close(&host.conns, &id, RequestState::Expired);
            return None;
        }
    };
    tracing::info!("workspace {workspace_id}: approval {id}: {outcome:?}");
    Some(reply(&outcome))
}

/// How long to wait for the user: nono's timeout minus a margin.
fn reply_budget(header: Option<&str>) -> Duration {
    let timeout = header
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    timeout.saturating_sub(REPLY_MARGIN.min(timeout / 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_go_out_before_nonos_timeout() {
        assert_eq!(reply_budget(Some("600")), Duration::from_secs(598));
        assert_eq!(reply_budget(Some("1")), Duration::from_millis(750));
        assert_eq!(reply_budget(None), Duration::from_secs(28));
    }
}
