//! `hello` and the aggregated state feed (`state.subscribe`).

use std::sync::Arc;

use controlroom_protocol::ids::SubscriptionId;
use controlroom_protocol::ui::next;
use controlroom_protocol::ui::notifications::StateEvents;
use controlroom_protocol::ui::{
    PushInfo, StateEventsParams, StateSubscribeParams, StateSubscribeResult, UiHelloParams,
    UiHelloResult,
};
use controlroom_protocol::{ErrorData, RpcError, UI_PROTOCOL_VERSION};

use super::{After, Conn, Reply, UiApi, mark_helloed};
use crate::hub::{StateSub, client_gone};

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
        capabilities: vec![controlroom_protocol::Capability::SCHEDULES],
    })
}

/// `state.subscribe`: a snapshot or the changes since the client's cursor, then live events,
/// from the feed of the connection's API. Changes meanwhile are buffered until the response
/// is queued.
pub(super) fn subscribe(conn: &Arc<Conn>, p: StateSubscribeParams) -> Result<Reply, RpcError> {
    let hub = &conn.hub;
    let subscription_id = SubscriptionId::generate();
    let value = {
        let mut st = hub.lock();
        let (value, sub) = match conn.api {
            UiApi::Legacy => {
                let changes = st.feed.changes_since(p.since);
                let snapshot = match changes {
                    Some(_) => None,
                    None => Some(hub.snapshot(&st)?),
                };
                let result = StateSubscribeResult {
                    subscription_id,
                    cursor: st.feed.cursor(),
                    snapshot,
                };
                (
                    encode(&result)?,
                    StateSub::Legacy(Some(changes.unwrap_or_default())),
                )
            }
            UiApi::Next => {
                let changes = st.next_feed.changes_since(p.since);
                let snapshot = match changes {
                    Some(_) => None,
                    None => Some(hub.next_snapshot(&st)?),
                };
                let result = next::StateSubscribeResult {
                    subscription_id,
                    cursor: st.next_feed.cursor(),
                    snapshot,
                };
                (
                    encode(&result)?,
                    StateSub::Next(Some(changes.unwrap_or_default())),
                )
            }
        };
        let client = st.clients.get_mut(&conn.client).ok_or_else(client_gone)?;
        client.state_subs.insert(subscription_id, sub);
        value
    };
    let (hub, client) = (hub.clone(), conn.client);
    let after: After = Box::new(move || {
        let mut st = hub.lock();
        let Some(c) = st.clients.get_mut(&client) else {
            return;
        };
        match c.state_subs.get_mut(&subscription_id) {
            Some(StateSub::Legacy(buffer)) => {
                let changes = buffer.take().unwrap_or_default();
                if !changes.is_empty() {
                    c.sink.notify::<StateEvents>(StateEventsParams {
                        subscription_id,
                        changes,
                    });
                }
            }
            Some(StateSub::Next(buffer)) => {
                let changes = buffer.take().unwrap_or_default();
                if !changes.is_empty() {
                    c.sink
                        .notify::<next::notifications::StateEvents>(next::StateEventsParams {
                            subscription_id,
                            changes,
                        });
                }
            }
            None => {}
        }
    });
    Ok(Reply {
        value,
        after: Some(after),
    })
}

fn encode<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value).map_err(|e| crate::errors::internal(format!("encoding: {e}")))
}
