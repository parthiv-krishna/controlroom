//! The open connections, for notifications, and the single-controller rule
//! (`docs/protocol.md` §5): at most one controller connection per daemon. A `hello` with the
//! same `controller_id` replaces the old connection (which is closed), so a half-open
//! connection never blocks a reconnect; a different `controller_id` gets
//! `controller_conflict` while the first is connected. Debug clients connect alongside.
//!
//! Notifications never block: a connection whose bounded queue is full is closed (its
//! subscriber resubscribes from its cursor after reconnecting, so nothing is lost).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use controlroom_protocol::ids::{ControllerId, UnixMillis};
use controlroom_protocol::jsonrpc::{NotificationMethod, RpcNotification, encode_line};
use controlroom_protocol::{ErrorData, RpcError};
use tokio::sync::{mpsc, watch};

/// A connection's outgoing side.
#[derive(Clone)]
pub struct ConnHandle {
    /// Queued lines.
    pub out: mpsc::Sender<String>,
    /// Closes the connection when set.
    pub close: watch::Sender<bool>,
}

impl ConnHandle {
    /// Queues a notification without waiting; closes the connection if its queue is full.
    pub fn notify<N: NotificationMethod>(&self, params: N::Params) {
        let line = match encode_line(&RpcNotification::new::<N>(params)) {
            Ok(line) => line,
            Err(e) => {
                tracing::error!("encoding {}: {e}", N::NAME);
                return;
            }
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = self.out.try_send(line) {
            tracing::warn!("a connection fell behind on notifications; closing it");
            self.close.send_replace(true);
        }
    }
}

struct Controller {
    controller_id: ControllerId,
    connection: u64,
    since: UnixMillis,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<u64, ConnHandle>,
    controller: Option<Controller>,
}

/// Every open connection, and which one is the controller.
#[derive(Default)]
pub struct Conns(Mutex<Inner>);

impl Conns {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a connection (after its `hello`).
    pub fn add(&self, id: u64, handle: ConnHandle) {
        self.lock().conns.insert(id, handle);
    }

    /// Forgets a connection that ended (and frees the controller slot if it held it).
    pub fn remove(&self, id: u64) {
        let mut inner = self.lock();
        inner.conns.remove(&id);
        if inner
            .controller
            .as_ref()
            .is_some_and(|c| c.connection == id)
        {
            inner.controller = None;
        }
    }

    /// Makes `connection` the controller `controller_id`, closing the same controller's
    /// older connection; `controller_conflict` while another controller is connected.
    pub fn claim_controller(
        &self,
        controller_id: ControllerId,
        connection: u64,
    ) -> Result<(), RpcError> {
        let mut inner = self.lock();
        if let Some(current) = &inner.controller {
            if current.controller_id != controller_id {
                return Err(RpcError::new(
                    ErrorData::ControllerConflict {
                        controller_id: current.controller_id,
                        connected_since: current.since,
                    },
                    "another controller is connected to this host daemon",
                ));
            }
            if let Some(old) = inner.conns.get(&current.connection) {
                old.close.send_replace(true);
            }
        }
        inner.controller = Some(Controller {
            controller_id,
            connection,
            since: UnixMillis::now(),
        });
        Ok(())
    }

    /// Whether `connection` is the controller.
    pub fn is_controller(&self, connection: u64) -> bool {
        self.lock()
            .controller
            .as_ref()
            .is_some_and(|c| c.connection == connection)
    }

    /// A connection's handle.
    pub fn get(&self, connection: u64) -> Option<ConnHandle> {
        self.lock().conns.get(&connection).cloned()
    }

    /// The controller's handle, if one is connected.
    pub fn controller(&self) -> Option<ConnHandle> {
        let inner = self.lock();
        let id = inner.controller.as_ref()?.connection;
        inner.conns.get(&id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> (ConnHandle, watch::Receiver<bool>) {
        let (out, _) = mpsc::channel(1);
        let (close, closed) = watch::channel(false);
        (ConnHandle { out, close }, closed)
    }

    #[test]
    fn one_controller_and_the_same_id_replaces() {
        let conns = Conns::default();
        let (a, b) = (ControllerId::generate(), ControllerId::generate());
        let (h1, closed1) = handle();
        conns.add(1, h1);
        conns.claim_controller(a, 1).unwrap();
        let (h2, _) = handle();
        conns.add(2, h2);
        let err = conns.claim_controller(b, 2).unwrap_err();
        assert!(matches!(
            err.data,
            Some(ErrorData::ControllerConflict { controller_id, .. }) if controller_id == a
        ));
        let (h3, _) = handle();
        conns.add(3, h3);
        conns.claim_controller(a, 3).unwrap();
        assert!(*closed1.borrow(), "the replaced connection is closed");
        conns.remove(1);
        assert!(conns.is_controller(3));
        conns.remove(3);
        conns.claim_controller(b, 2).unwrap();
    }
}
