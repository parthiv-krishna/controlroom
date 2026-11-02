//! The hub's event bus. Subsystems that react to machine and session changes subscribe here
//! and keep their own state, instead of growing the hub: the notifier (attention and push) and
//! each machine's auto-upgrade watcher now; the broker, policy and timeline cache later.
//!
//! Events are sent under the hub lock, so every subscriber sees them in feed order. The bus is
//! bounded: a subscriber that falls behind is told how many events it missed.

use controlroom_protocol::ids::{MachineId, SessionId};
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::ui::Connectivity;

/// How many events a slow subscriber may fall behind.
pub(crate) const BUS_CAPACITY: usize = 1024;

/// Something changed in the hub.
#[derive(Debug, Clone)]
pub(crate) enum BusEvent {
    /// A machine's link to its daemon changed.
    Connectivity {
        machine_id: MachineId,
        connectivity: Connectivity,
    },
    /// A machine's record changed (e.g. its settings).
    MachineChanged { machine_id: MachineId },
    /// A session appeared or its summary changed.
    Session {
        machine_id: MachineId,
        previous: Option<Box<SessionSummary>>,
        current: Box<SessionSummary>,
    },
    /// A session was deleted on its machine.
    SessionRemoved {
        machine_id: MachineId,
        session_id: SessionId,
    },
}

impl BusEvent {
    /// The machine the event is about.
    pub(crate) fn machine_id(&self) -> MachineId {
        match self {
            BusEvent::Connectivity { machine_id, .. }
            | BusEvent::MachineChanged { machine_id }
            | BusEvent::Session { machine_id, .. }
            | BusEvent::SessionRemoved { machine_id, .. } => *machine_id,
        }
    }
}
