//! Applying a machine's feed (`machine.subscribe` snapshot and `machine.events`) to the hub's
//! session cache, emitting state events and bus events for every change.

use std::collections::HashSet;

use controlroom_protocol::daemon::{
    DaemonSettings, MachineChange, MachineEvent, MachineEventsParams, MachineSubscribeResult,
};
use controlroom_protocol::ids::MachineId;
use controlroom_protocol::session::SessionSummary;
use controlroom_protocol::ui::{MachineSession, StateEvent};

use super::{BusEvent, HubState};

/// Stores a summary; emits it if it changed.
fn upsert(st: &mut HubState, machine_id: MachineId, session: SessionSummary) {
    let Some(entry) = st.machines.get_mut(&machine_id) else {
        return;
    };
    let previous = entry.sessions.insert(session.id, session.clone());
    if previous.as_ref() == Some(&session) {
        return;
    }
    let stale = entry.stale();
    let _ = st.bus.send(BusEvent::Session {
        machine_id,
        previous: previous.map(Box::new),
        current: Box::new(session.clone()),
    });
    st.emit(StateEvent::SessionUpserted {
        session: Box::new(MachineSession {
            machine_id,
            stale,
            session,
        }),
    });
}

/// Applies a `machine.subscribe` result (from its response hook, before any of its events).
/// Returns the daemon settings of a snapshot.
pub(super) fn subscribed(
    st: &mut HubState,
    machine_id: MachineId,
    result: MachineSubscribeResult,
) -> Option<DaemonSettings> {
    let entry = st.machines.get_mut(&machine_id)?;
    entry.machine_sub = Some(result.subscription_id);
    entry.rev = Some(result.rev);
    let snapshot = result.snapshot?;
    let keep: HashSet<_> = snapshot.sessions.iter().map(|s| s.id).collect();
    let gone: Vec<_> = entry
        .sessions
        .keys()
        .filter(|id| !keep.contains(id))
        .copied()
        .collect();
    for session_id in &gone {
        entry.sessions.remove(session_id);
    }
    for session_id in gone {
        st.session_removed(machine_id, session_id);
    }
    for session in snapshot.sessions {
        upsert(st, machine_id, session);
    }
    Some(snapshot.settings)
}

/// Applies `machine.events`. Returns new daemon settings, if any.
pub(super) fn events(
    st: &mut HubState,
    machine_id: MachineId,
    p: MachineEventsParams,
) -> Option<DaemonSettings> {
    let entry = st.machines.get_mut(&machine_id)?;
    if entry.machine_sub != Some(p.subscription_id) {
        return None;
    }
    let mut settings = None;
    for MachineChange { rev, event } in p.changes {
        if let Some(entry) = st.machines.get_mut(&machine_id) {
            entry.rev = Some(rev);
        }
        match event {
            MachineEvent::SessionUpserted { session } => upsert(st, machine_id, *session),
            MachineEvent::SessionRemoved { id } => {
                let removed = st
                    .machines
                    .get_mut(&machine_id)
                    .and_then(|e| e.sessions.remove(&id));
                if removed.is_some() {
                    st.session_removed(machine_id, id);
                }
            }
            MachineEvent::SettingsChanged { settings: s } => settings = Some(s),
            MachineEvent::Unknown => {}
        }
    }
    settings
}
