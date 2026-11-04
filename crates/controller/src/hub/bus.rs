//! The hub's event bus. Subsystems that react to machine and session changes subscribe here
//! and keep their own state, instead of growing the hub: the notifier (attention and push),
//! the scheduler and each machine's auto-upgrade watcher.
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
    /// A machine's link to its host daemon changed.
    Connectivity {
        machine_id: MachineId,
        connectivity: Connectivity,
    },
    /// A machine's record changed (e.g. its settings).
    MachineChanged { machine_id: MachineId },
    /// A session (the agent daemon's) appeared or its summary changed.
    Session {
        previous: Option<Box<SessionSummary>>,
        current: Box<SessionSummary>,
    },
    /// A session was deleted.
    SessionRemoved { session_id: SessionId },
    /// A machine's last host request in flight was answered (it may be idle now).
    HostCallsDone { machine_id: MachineId },
    /// The link to the agent daemon came up (`true`) or went down.
    Agentd { connected: bool },
}

impl BusEvent {
    /// The machine the event is about, if any.
    pub(crate) fn machine_id(&self) -> Option<MachineId> {
        match self {
            BusEvent::Connectivity { machine_id, .. }
            | BusEvent::MachineChanged { machine_id }
            | BusEvent::HostCallsDone { machine_id } => Some(*machine_id),
            BusEvent::Session { .. }
            | BusEvent::SessionRemoved { .. }
            | BusEvent::Agentd { .. } => None,
        }
    }
}
