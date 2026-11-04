//! The controller's machines as the agent daemon knows them (`hostops.machines`): names for
//! `machine:path`, availability, host epochs and grace periods.
//!
//! A report replaces the previous one. Comparing them tells the runtime what happened to each
//! host link: a **new connection** (connected with a new `since`, or a new epoch) renews the
//! host subscriptions and refreshes the workspaces (see `sync`); a **drop** counts as a link
//! drop for every call still open on that machine. Names are persisted, so they are known after
//! a restart before the controller reports again.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlroom_protocol::agentd::HostMachine;
use controlroom_protocol::ids::{Epoch, MachineId, SessionId, UnixMillis};
use controlroom_protocol::session::Availability;
use controlroom_protocol::ui::Connectivity;
use controlroom_protocol::{ErrorData, RpcError};

use crate::core::{Core, HostEffect, Task};
use crate::errors::internal;

/// The grace period of a machine the controller hasn't reported.
pub(crate) const DEFAULT_GRACE_S: u32 = 600;
/// Where the latest report is persisted.
pub(crate) const MACHINES_KEY: &str = "machines";

/// The machines of the latest report.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Machines {
    pub list: BTreeMap<MachineId, HostMachine>,
    /// Since when machines without a report have been unavailable (the daemon's start).
    pub unknown_since: UnixMillis,
}

impl Machines {
    /// The machine's availability; `connecting` until the controller reports it.
    pub(crate) fn availability(&self, machine: &MachineId) -> Availability {
        self.list.get(machine).map_or_else(
            || Availability {
                connectivity: Connectivity::Connecting,
                since: self.unknown_since,
            },
            |m| m.availability.clone(),
        )
    }

    /// Whether host requests to the machine go through now.
    pub(crate) fn connected(&self, machine: &MachineId) -> bool {
        self.list
            .get(machine)
            .is_some_and(|m| m.availability.is_connected())
    }

    /// `machine_unavailable` unless host requests to the machine go through now.
    pub(crate) fn require_connected(&self, machine: &MachineId) -> Result<(), RpcError> {
        if self.connected(machine) {
            return Ok(());
        }
        Err(unavailable(
            *machine,
            self.availability(machine).connectivity,
            &self.name(machine),
        ))
    }

    /// The display name the agent uses (the id until the controller has named it).
    pub(crate) fn name(&self, machine: &MachineId) -> String {
        self.list
            .get(machine)
            .map_or_else(|| machine.to_string(), |m| m.name.clone())
    }

    /// The machine with this display name.
    pub(crate) fn by_name(&self, name: &str) -> Option<MachineId> {
        self.list
            .values()
            .find(|m| m.name == name)
            .map(|m| m.machine_id)
            .or_else(|| name.parse().ok())
    }

    /// How long a call waits for the machine.
    pub(crate) fn grace_ms(&self, machine: &MachineId) -> i64 {
        let grace = self
            .list
            .get(machine)
            .map_or(DEFAULT_GRACE_S, |m| m.grace_s);
        i64::from(grace) * 1000
    }

    /// The host daemon's epoch from its latest `hello`.
    pub(crate) fn epoch(&self, machine: &MachineId) -> Option<Epoch> {
        self.list.get(machine).and_then(|m| m.epoch)
    }

    /// When the machine's current outage started, if it is unavailable.
    pub(crate) fn down_since(&self, machine: &MachineId) -> Option<UnixMillis> {
        let availability = self.availability(machine);
        (!availability.is_connected()).then_some(availability.since)
    }
}

/// What a report changed for one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Change {
    /// A new host connection: subscriptions must be renewed.
    connected: bool,
    /// The link dropped.
    dropped: bool,
}

fn compare(before: Option<&HostMachine>, after: &HostMachine) -> Change {
    let up = after.availability.is_connected();
    let was_up = before.is_some_and(|b| b.availability.is_connected());
    let renewed = before
        .is_none_or(|b| b.availability.since != after.availability.since || b.epoch != after.epoch);
    Change {
        connected: up && (!was_up || renewed),
        dropped: was_up && !up,
    }
}

impl Core {
    /// Loads the persisted machines (names) at startup; all of them count as unavailable until
    /// the controller reports.
    pub(crate) fn load_machines(&mut self, now: UnixMillis) -> Result<(), RpcError> {
        let stored: Vec<HostMachine> = self
            .store
            .kv_get(MACHINES_KEY)
            .map_err(internal)?
            .unwrap_or_default();
        let list = stored
            .into_iter()
            .map(|mut m| {
                m.availability = Availability {
                    connectivity: Connectivity::Connecting,
                    since: now,
                };
                (m.machine_id, m)
            })
            .collect();
        self.machines = Arc::new(Machines {
            list,
            unknown_since: now,
        });
        for session in self.sessions.values_mut() {
            session.machines = self.machines.clone();
        }
        Ok(())
    }

    /// `hostops.machines`: takes the controller's report (see the module docs).
    pub(crate) fn set_machines(&mut self, report: Vec<HostMachine>) -> Result<(), RpcError> {
        let before = self.machines.clone();
        let list: BTreeMap<MachineId, HostMachine> =
            report.into_iter().map(|m| (m.machine_id, m)).collect();
        let mut changed = Vec::new();
        for (machine, after) in &list {
            let change = compare(before.list.get(machine), after);
            if change.connected {
                self.task(Task::Sync(*machine));
            }
            if change.dropped {
                self.link_dropped(*machine)?;
                self.fail_opens(
                    *machine,
                    after.availability.connectivity.clone(),
                    &after.name,
                );
            }
            if before.list.get(machine) != Some(after) {
                changed.push(*machine);
            }
        }
        // Machines the controller no longer has are unreachable now.
        let removed: Vec<&HostMachine> = before
            .list
            .values()
            .filter(|m| !list.contains_key(&m.machine_id))
            .collect();
        for machine in &removed {
            self.fail_opens(machine.machine_id, Connectivity::Unreachable, &machine.name);
        }
        changed.extend(removed.iter().map(|m| m.machine_id));
        let stored: Vec<&HostMachine> = list.values().collect();
        self.store.kv_set(MACHINES_KEY, &stored).map_err(internal)?;
        self.machines = Arc::new(Machines {
            list,
            unknown_since: before.unknown_since,
        });
        for session in self.sessions.values_mut() {
            session.machines = self.machines.clone();
        }
        let affected: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|s| {
                s.data
                    .workspaces
                    .iter()
                    .any(|w| changed.contains(&w.machine_id))
            })
            .map(|s| s.data.id)
            .collect();
        for id in affected {
            self.touch(id);
        }
        Ok(())
    }

    /// `machine` is no longer connected: workspaces being opened there (a create or an add
    /// that started while it looked connected) fail with `machine_unavailable` instead of
    /// waiting for it.
    fn fail_opens(&mut self, machine: MachineId, connectivity: Connectivity, name: &str) {
        let error = unavailable(machine, connectivity, name);
        self.effect(HostEffect::FailOpens(machine, error));
    }

    /// The link to `machine` dropped: every call open on it counts the drop.
    fn link_dropped(&mut self, machine: MachineId) -> Result<(), RpcError> {
        let ids: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|s| s.live.calls.values().any(|c| c.machine == machine))
            .map(|s| s.data.id)
            .collect();
        for id in ids {
            for call in self.session_mut(id)?.live.calls.values_mut() {
                if call.machine == machine {
                    call.link_drops += 1;
                }
            }
        }
        Ok(())
    }
}

/// The `machine_unavailable` error.
fn unavailable(machine: MachineId, connectivity: Connectivity, name: &str) -> RpcError {
    RpcError::new(
        ErrorData::MachineUnavailable {
            machine_id: machine,
            connectivity: Box::new(connectivity),
        },
        format!("{name} is not connected"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(connectivity: Connectivity, since: i64, epoch: Option<Epoch>) -> HostMachine {
        HostMachine {
            machine_id: MachineId::generate(),
            name: "devbox".into(),
            availability: Availability {
                connectivity,
                since: UnixMillis(since),
            },
            epoch,
            grace_s: 600,
        }
    }

    #[test]
    fn reports_tell_new_connections_and_drops() {
        let up = machine(Connectivity::Connected, 1, None);
        let change = compare(None, &up);
        assert!(change.connected && !change.dropped);
        assert_eq!(
            compare(Some(&up), &up),
            Change {
                connected: false,
                dropped: false
            }
        );
        let down = HostMachine {
            availability: Availability {
                connectivity: Connectivity::Unreachable,
                since: UnixMillis(5),
            },
            ..up.clone()
        };
        assert!(compare(Some(&up), &down).dropped);
        let again = HostMachine {
            availability: Availability {
                connectivity: Connectivity::Connected,
                since: UnixMillis(9),
            },
            ..up.clone()
        };
        assert!(compare(Some(&up), &again).connected, "a new connection");
        let restarted = HostMachine {
            epoch: Some(Epoch::generate()),
            ..up.clone()
        };
        assert!(compare(Some(&up), &restarted).connected, "a new epoch");
    }
}
