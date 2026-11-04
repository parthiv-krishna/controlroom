//! Machine records: how to reach a daemon host, its pinned host key, and what the controller
//! last learned about it. Every setter updates only its own columns.

use controlroom_protocol::UnixMillis;
use controlroom_protocol::daemon::{DaemonSettings, DoctorReport};
use controlroom_protocol::ids::{DaemonId, KeyId, MachineId};
use controlroom_protocol::version::Capability;
use rusqlite::OptionalExtension;

use super::{
    OwnerId, Store, StoreResult, conflict, json_col, json_col_opt, parsed_col, parsed_col_opt,
    to_json,
};

/// A stored machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    /// The machine.
    pub id: MachineId,
    /// Display name (unique per owner).
    pub name: String,
    /// Host name or address.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH user.
    pub username: String,
    /// An additional key; `None` means the controlroom key.
    pub key_id: Option<KeyId>,
    /// For machines on the controlroom key: the controlroom key installed on the host.
    pub auth_key_id: Option<KeyId>,
    /// The pinned host key (OpenSSH format).
    pub host_key: Option<String>,
    /// Upgrade a compatible, older daemon automatically while idle.
    pub auto_upgrade: bool,
    /// How long tool calls wait for the machine while it is unreachable (seconds).
    pub grace_s: u32,
    /// The daemon's id, once known.
    pub daemon_id: Option<DaemonId>,
    /// The remote `$HOME`, once probed.
    pub home: Option<String>,
    /// The architecture, once probed.
    pub arch: Option<String>,
    /// The running daemon's version, once known.
    pub version: Option<String>,
    /// The daemon's optional features, from its last `hello`.
    pub capabilities: Vec<Capability>,
    /// When the daemon was last reachable.
    pub last_seen: Option<UnixMillis>,
    /// The latest doctor report.
    pub doctor: Option<DoctorReport>,
    /// The daemon's settings, as last reported.
    pub daemon_settings: Option<DaemonSettings>,
    /// When it was added.
    pub created_at: UnixMillis,
}

/// A machine to add.
#[derive(Debug, Clone)]
pub struct NewMachineRow {
    /// Display name.
    pub name: String,
    /// Host.
    pub host: String,
    /// Port.
    pub port: u16,
    /// User.
    pub username: String,
    /// An additional key, or `None` for the controlroom key.
    pub key_id: Option<KeyId>,
    /// The controlroom key it authenticated with (machines on the controlroom key).
    pub auth_key_id: Option<KeyId>,
    /// The confirmed host key (OpenSSH format).
    pub host_key: String,
}

/// What a successful contact taught us.
#[derive(Debug, Clone, Default)]
pub struct SeenUpdate {
    /// The daemon's id.
    pub daemon_id: Option<DaemonId>,
    /// The running daemon's version.
    pub version: Option<String>,
    /// The daemon's capabilities.
    pub capabilities: Option<Vec<Capability>>,
    /// The remote `$HOME`.
    pub home: Option<String>,
    /// The architecture.
    pub arch: Option<String>,
}

/// Owner-scoped machine operations.
pub struct Machines<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

const COLUMNS: &str = "id, name, host, port, username, key_id, auth_key_id, host_key, \
                       auto_upgrade, daemon_id, home, arch, version, last_seen, doctor, \
                       daemon_settings, created_at, capabilities, grace_s";

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<MachineRow> {
    Ok(MachineRow {
        id: parsed_col(row, 0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get(3)?,
        username: row.get(4)?,
        key_id: parsed_col_opt(row, 5)?,
        auth_key_id: parsed_col_opt(row, 6)?,
        host_key: row.get(7)?,
        auto_upgrade: row.get(8)?,
        daemon_id: parsed_col_opt(row, 9)?,
        home: row.get(10)?,
        arch: row.get(11)?,
        version: row.get(12)?,
        last_seen: row.get::<_, Option<i64>>(13)?.map(UnixMillis),
        doctor: json_col_opt(row, 14)?,
        daemon_settings: json_col_opt(row, 15)?,
        created_at: UnixMillis(row.get(16)?),
        capabilities: json_col(row, 17)?,
        grace_s: row.get(18)?,
    })
}

impl<'a> Machines<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Adds a machine. A taken name or an already added `user@host:port` is a conflict.
    pub fn insert(&self, machine: NewMachineRow) -> StoreResult<MachineRow> {
        let id = MachineId::generate();
        self.store
            .lock()
            .execute(
                "INSERT INTO machines (id, owner_id, name, host, port, username, key_id,
                                       auth_key_id, host_key, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id.to_string(),
                    self.owner.as_str(),
                    machine.name,
                    machine.host,
                    machine.port,
                    machine.username,
                    machine.key_id.map(|k| k.to_string()),
                    machine.auth_key_id.map(|k| k.to_string()),
                    machine.host_key,
                    UnixMillis::now().0
                ],
            )
            .map_err(|e| {
                conflict(e, || {
                    format!(
                        "a machine named {:?} or for {}@{}:{} already exists",
                        machine.name, machine.username, machine.host, machine.port
                    )
                })
            })?;
        self.get(id)?
            .ok_or_else(|| super::StoreError::Conflict("the machine vanished".into()))
    }

    /// A machine by id.
    pub fn get(&self, id: MachineId) -> StoreResult<Option<MachineRow>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!("SELECT {COLUMNS} FROM machines WHERE owner_id = ?1 AND id = ?2"),
                (self.owner.as_str(), id.to_string()),
                map,
            )
            .optional()?)
    }

    /// The machine for `user@host:port`, if added.
    pub fn find(&self, host: &str, port: u16, username: &str) -> StoreResult<Option<MachineRow>> {
        Ok(self
            .store
            .lock()
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM machines
                     WHERE owner_id = ?1 AND host = ?2 AND port = ?3 AND username = ?4"
                ),
                rusqlite::params![self.owner.as_str(), host, port, username],
                map,
            )
            .optional()?)
    }

    /// Every machine, by name.
    pub fn list(&self) -> StoreResult<Vec<MachineRow>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM machines WHERE owner_id = ?1 ORDER BY name, id"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn set(&self, id: MachineId, column: &str, value: &dyn rusqlite::ToSql) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            &format!("UPDATE machines SET {column} = ?3 WHERE owner_id = ?1 AND id = ?2"),
            rusqlite::params![self.owner.as_str(), id.to_string(), value],
        )?;
        Ok(n > 0)
    }

    /// Renames a machine; a taken name is a conflict.
    pub fn set_name(&self, id: MachineId, name: &str) -> StoreResult<bool> {
        self.set(id, "name", &name).map_err(|e| match e {
            super::StoreError::Sqlite(e) => {
                conflict(e, || format!("a machine named {name:?} already exists"))
            }
            other => other,
        })
    }

    /// Chooses the key: an additional key, or `None` for the controlroom key.
    pub fn set_key(&self, id: MachineId, key_id: Option<KeyId>) -> StoreResult<bool> {
        self.set(id, "key_id", &key_id.map(|k| k.to_string()))
    }

    /// Turns automatic upgrades on or off.
    pub fn set_auto_upgrade(&self, id: MachineId, on: bool) -> StoreResult<bool> {
        self.set(id, "auto_upgrade", &on)
    }

    /// Sets the tool-call grace period (seconds).
    pub fn set_grace(&self, id: MachineId, grace_s: u32) -> StoreResult<bool> {
        self.set(id, "grace_s", &grace_s)
    }

    /// Pins (or re-pins, after the user confirmed it) the host key.
    pub fn pin_host_key(&self, id: MachineId, host_key: &str) -> StoreResult<bool> {
        self.set(id, "host_key", &host_key)
    }

    /// Records which controlroom key is installed on the host.
    pub fn set_auth_key(&self, id: MachineId, key: Option<KeyId>) -> StoreResult<bool> {
        self.set(id, "auth_key_id", &key.map(|k| k.to_string()))
    }

    /// Stores the latest doctor report.
    pub fn set_doctor(&self, id: MachineId, report: &DoctorReport) -> StoreResult<bool> {
        self.set(id, "doctor", &to_json(report)?)
    }

    /// Stores the daemon's settings as it reported them.
    pub fn set_daemon_settings(
        &self,
        id: MachineId,
        settings: &DaemonSettings,
    ) -> StoreResult<bool> {
        self.set(id, "daemon_settings", &to_json(settings)?)
    }

    /// Records a successful contact; absent fields keep their stored values.
    pub fn record_seen(
        &self,
        id: MachineId,
        seen: &SeenUpdate,
        at: UnixMillis,
    ) -> StoreResult<bool> {
        let n = self.store.lock().execute(
            "UPDATE machines SET last_seen = ?3,
                 daemon_id = COALESCE(?4, daemon_id), version = COALESCE(?5, version),
                 home = COALESCE(?6, home), arch = COALESCE(?7, arch),
                 capabilities = COALESCE(?8, capabilities)
             WHERE owner_id = ?1 AND id = ?2",
            rusqlite::params![
                self.owner.as_str(),
                id.to_string(),
                at.0,
                seen.daemon_id.map(|d| d.to_string()),
                seen.version,
                seen.home,
                seen.arch,
                seen.capabilities.as_ref().map(to_json).transpose()?
            ],
        )?;
        Ok(n > 0)
    }

    /// Removes the machine. Task links and grants that point at it are kept.
    pub fn delete(&self, id: MachineId) -> StoreResult<bool> {
        let conn = self.store.lock();
        conn.execute(
            "DELETE FROM sandbox_profiles WHERE owner_id = ?1 AND scope_kind = 'machine'
             AND scope_id = ?2",
            (self.owner.as_str(), id.to_string()),
        )?;
        let n = conn.execute(
            "DELETE FROM machines WHERE owner_id = ?1 AND id = ?2",
            (self.owner.as_str(), id.to_string()),
        )?;
        Ok(n > 0)
    }
}
