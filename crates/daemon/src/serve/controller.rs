//! The single-controller rule (`docs/protocol.md`): at most one controller connection per
//! daemon. A `hello` with the same `controller_id` replaces the old connection (which is
//! closed, ending its subscriptions), so a half-open connection never blocks a reconnect; a
//! different `controller_id` gets `controller_conflict` while the first is connected.

use std::sync::{Mutex, PoisonError};

use controlroom_protocol::ids::{ControllerId, UnixMillis};
use controlroom_protocol::{ErrorData, RpcError};
use tokio::sync::watch;

/// The connected controller, if any.
#[derive(Default)]
pub struct Controllers(Mutex<Option<Slot>>);

struct Slot {
    controller_id: ControllerId,
    connection: u64,
    since: UnixMillis,
    /// Closes the connection when set.
    close: watch::Sender<bool>,
}

impl Controllers {
    /// Registers `connection` as the controller `controller_id`; `close` closes it later.
    pub fn claim(
        &self,
        controller_id: ControllerId,
        connection: u64,
        close: watch::Sender<bool>,
    ) -> Result<(), RpcError> {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = slot.as_ref() {
            if current.controller_id != controller_id {
                return Err(RpcError::new(
                    ErrorData::ControllerConflict {
                        controller_id: current.controller_id,
                        connected_since: current.since,
                    },
                    "another controller is connected to this daemon",
                ));
            }
            current.close.send_replace(true);
        }
        *slot = Some(Slot {
            controller_id,
            connection,
            since: UnixMillis::now(),
            close,
        });
        Ok(())
    }

    /// Forgets `connection` if it is the controller.
    pub fn release(&self, connection: u64) {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|s| s.connection == connection) {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_controller_same_id_replaces() {
        let controllers = Controllers::default();
        let (a, b) = (ControllerId::generate(), ControllerId::generate());
        let (close1, closed1) = watch::channel(false);
        controllers.claim(a, 1, close1).unwrap();
        let (close2, _) = watch::channel(false);
        let err = controllers.claim(b, 2, close2).unwrap_err();
        assert!(
            matches!(err.data, Some(ErrorData::ControllerConflict { controller_id, .. }) if controller_id == a)
        );
        assert!(!*closed1.borrow());

        let (close3, closed3) = watch::channel(false);
        controllers.claim(a, 3, close3).unwrap();
        assert!(*closed1.borrow(), "the replaced connection is closed");
        controllers.release(1); // the old connection going away keeps the new one
        let (close4, _) = watch::channel(false);
        assert!(controllers.claim(b, 4, close4).is_err());
        controllers.release(3);
        let (close5, _) = watch::channel(false);
        controllers.claim(b, 5, close5).unwrap();
        assert!(!*closed3.borrow());
    }
}
