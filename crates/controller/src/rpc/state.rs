//! `hello` and the aggregated state feed (`state.subscribe`).

use std::sync::Arc;

use controlroom_protocol::ids::SubscriptionId;
use controlroom_protocol::ui::notifications::StateEvents;
use controlroom_protocol::ui::{
    PushInfo, StateEventsParams, StateSubscribeParams, StateSubscribeResult, UiHelloParams,
    UiHelloResult,
};
use controlroom_protocol::{ErrorData, RpcError, UI_PROTOCOL_VERSION};

use super::{Conn, Reply, mark_helloed, reply};
use crate::hub::client_gone;

/// `hello`: checks the protocol and binds the connection (and its login session) to the
/// browser's device.
pub(super) fn hello(conn: &Conn, p: UiHelloParams) -> Result<UiHelloResult, RpcError> {
    let services = &conn.hub.services;
    if !UI_PROTOCOL_VERSION.is_compatible_with(p.protocol_version) {
        return Err(RpcError::new(
            ErrorData::IncompatibleVersion {
                protocol_version: UI_PROTOCOL_VERSION,
                version: services.server_version.clone(),
            },
            format!(
                "the client speaks UI protocol {}, the server {UI_PROTOCOL_VERSION}",
                p.protocol_version
            ),
        ));
    }
    if let Some(device) = p.device_id {
        if let Some(hash) = &conn.token_hash
            && let Err(e) = conn.hub.repo().auth_sessions().set_device(hash, &device)
        {
            tracing::warn!(error = %e, "recording the login session's device");
        }
        conn.set_device(device);
    }
    mark_helloed(conn);
    Ok(UiHelloResult {
        server_version: services.server_version.clone(),
        protocol_version: UI_PROTOCOL_VERSION,
        auth: services.auth.mode,
        user: conn.principal.user_info(),
        push: services.push.as_ref().map(|p| PushInfo {
            vapid_public_key: p.vapid_public_key.clone(),
        }),
        capabilities: Vec::new(),
    })
}

/// `state.subscribe`: a snapshot or the changes since the client's cursor, then live events.
/// Changes meanwhile are buffered until the response is queued.
pub(super) fn subscribe(conn: &Arc<Conn>, p: StateSubscribeParams) -> Result<Reply, RpcError> {
    let hub = &conn.hub;
    let subscription_id = SubscriptionId::generate();
    let result = {
        let mut st = hub.lock();
        let changes = st.feed.changes_since(p.since);
        let snapshot = match changes {
            Some(_) => None,
            None => Some(hub.snapshot(&st)?),
        };
        let cursor = st.feed.cursor();
        let client = st.clients.get_mut(&conn.client).ok_or_else(client_gone)?;
        client
            .state_subs
            .insert(subscription_id, Some(changes.unwrap_or_default()));
        StateSubscribeResult {
            subscription_id,
            cursor,
            snapshot,
        }
    };
    let mut reply = reply(result)?;
    let (hub, client) = (hub.clone(), conn.client);
    reply.after = Some(Box::new(move || {
        let mut st = hub.lock();
        let Some(c) = st.clients.get_mut(&client) else {
            return;
        };
        let Some(backlog) = c
            .state_subs
            .get_mut(&subscription_id)
            .and_then(Option::take)
        else {
            return;
        };
        if !backlog.is_empty() {
            c.sink.notify::<StateEvents>(StateEventsParams {
                subscription_id,
                changes: backlog,
            });
        }
    }));
    Ok(reply)
}
