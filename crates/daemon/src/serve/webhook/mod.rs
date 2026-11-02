//! The approval webhook (spec §11.4): a loopback HTTP listener nono `POST`s its approval
//! requests to, at `http://127.0.0.1:<port>/nono/v1/<session>/<secret>`.
//!
//! - **Correlation and trust.** The URL names the session; its secret is generated per launch
//!   ([`crate::launch`] keeps it in `sessions/<id>/hook.secret`) and compared in constant time.
//!   Anything else is answered 403/404 (nono treats every non-2xx answer as a denial).
//! - **Flow.** The request becomes a blocking `sandbox` request on the session
//!   ([`Runtime::raise`]); the first resolution is the reply. The reply goes out before nono's
//!   `X-Nono-Timeout-Secs` (minus a margin); if nobody answered by then the request expires
//!   and nono is told "denied". If nono hangs up first (the agent or its sandbox died), the
//!   request expires at once.
//! - **Restarts.** The port is kept in `run/webhook.port` and bound again on the next start,
//!   so agents that outlive the daemon still reach it. Requests that were waiting when the
//!   daemon died died with their HTTP connection; recovery expires them. If the old port is
//!   taken, a new one is used and older agents' approvals fail closed until they restart.

mod approval;
mod http;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use controlroom_protocol::ids::SessionId;
use controlroom_runtime::Runtime;
use controlroom_sandbox::webhook::{ApprovalReply, ApprovalRequest, parse_hook_path};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::dirs::{Dirs, create_private_dir, write_private_file};
use crate::launch::load_hook_secret;

/// Connections served at once; further ones wait in the accept queue.
const MAX_CONNECTIONS: usize = 64;
/// Time a client gets to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Waiting budget when nono doesn't say how long it waits.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Reply this much before nono's own timeout (at most a quarter of it).
const REPLY_MARGIN: Duration = Duration::from_secs(2);

/// Binds the listener on loopback: the port of the previous run if it is free, else a new one
/// (saved for the next run).
pub async fn bind(dirs: &Dirs) -> std::io::Result<TcpListener> {
    let file = dirs.webhook_port();
    let saved: Option<u16> = std::fs::read_to_string(file.as_path())
        .ok()
        .and_then(|text| text.trim().parse().ok());
    if let Some(port) = saved
        && let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await
    {
        return Ok(listener);
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    create_private_dir(&dirs.run())?;
    write_private_file(&file, port.to_string().as_bytes())?;
    Ok(listener)
}

/// Serves approval requests until the task is dropped.
pub async fn serve(listener: TcpListener, runtime: Runtime, dirs: Dirs) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(permit) = slots.clone().acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok((stream, peer)) => {
                let (runtime, dirs) = (runtime.clone(), dirs.clone());
                tokio::spawn(async move {
                    handle(stream, peer, &runtime, &dirs).await;
                    drop(permit);
                });
            }
            Err(e) => tracing::warn!("webhook accept: {e}"),
        }
    }
}

async fn handle(mut stream: TcpStream, peer: SocketAddr, runtime: &Runtime, dirs: &Dirs) {
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
    let (status, reply) = match route(&request, dirs) {
        Ok((session, approval)) => {
            let budget = reply_budget(request.header("x-nono-timeout-secs"));
            match decide(runtime, session, &approval, budget, &mut stream).await {
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
fn route(request: &http::Request, dirs: &Dirs) -> Result<(SessionId, ApprovalRequest), u16> {
    if request.method != "POST" {
        return Err(405);
    }
    let (session, secret) = parse_hook_path(&request.path).ok_or(404u16)?;
    let known = load_hook_secret(dirs, session.as_str()).is_some_and(|s| s.matches(&secret));
    if !known {
        tracing::warn!("webhook: a request for session {session} with a wrong secret");
        return Err(403);
    }
    let session: SessionId = session.as_str().parse().map_err(|_| 404u16)?;
    let approval = ApprovalRequest::from_json(&request.body).map_err(|e| {
        tracing::warn!("webhook: session {session}: {e}");
        400u16
    })?;
    Ok((session, approval))
}

/// Raises the approval and waits for its first resolution, at most `budget`. `None` when the
/// client hung up first (the request then expires).
async fn decide(
    runtime: &Runtime,
    session: SessionId,
    approval: &ApprovalRequest,
    budget: Duration,
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Option<ApprovalReply> {
    let Some(request) = approval::to_request(approval) else {
        tracing::info!("session {session}: denying an approval request it can't show");
        return Some(ApprovalReply::denied(
            "controlroom can't show this kind of request",
        ));
    };
    let mut raised = match runtime.raise(session, request).await {
        Ok(raised) => raised,
        Err(e) => return Some(ApprovalReply::denied(e.message)),
    };
    tracing::info!(
        "session {session}: approval {} raised: {}",
        raised.request().id,
        raised.request().title
    );
    let resolved = tokio::select! {
        request = raised.resolution() => Some(request),
        () = tokio::time::sleep(budget) => None,
        () = http::closed(stream) => {
            tracing::info!("session {session}: the approval's client hung up");
            return None;
        }
    };
    let request = resolved.unwrap_or_else(|| raised.expire());
    tracing::info!(
        "session {session}: approval {} {:?}",
        request.id,
        request.state
    );
    Some(approval::to_reply(&request))
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
        assert_eq!(reply_budget(Some("8")), Duration::from_secs(6));
        assert_eq!(reply_budget(Some("1")), Duration::from_millis(750));
        assert_eq!(reply_budget(Some("0")), Duration::ZERO);
        assert_eq!(reply_budget(None), Duration::from_secs(28));
        assert_eq!(reply_budget(Some("soon")), Duration::from_secs(28));
    }
}
