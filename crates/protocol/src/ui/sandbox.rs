//! Sandbox defaults per machine and project (Settings › Sandbox).
//!
//! A session's effective profile combines its machine's defaults with those of every project it
//! belongs to: the strictest level, and the union of standing grants, domains and mediated
//! commands. Edits apply to new sessions and restarts.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{GrantId, MachineId, ProjectId};
use crate::session::{MediatedCommand, SandboxGrant, SandboxLevel};

/// What a default profile belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileScope {
    /// A machine.
    Machine {
        /// The machine.
        machine_id: MachineId,
    },
    /// A project.
    Project {
        /// The project.
        project_id: ProjectId,
    },
    /// A scope this build doesn't know.
    #[serde(other)]
    Unknown,
}

/// The sandbox defaults of one machine or project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxProfile {
    /// What it belongs to.
    pub scope: ProfileScope,
    /// The level; `null` leaves it to the other scopes (default `strict`).
    pub level: Option<SandboxLevel>,
    /// Standing grants.
    pub grants: Vec<SandboxGrant>,
    /// Allowed network domains.
    pub domains: Vec<String>,
    /// Mediated commands beyond the built-in defaults.
    pub mediated: Vec<MediatedCommand>,
}

/// Result of `sandbox.profiles` and `sandbox.set_profile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxProfilesResult {
    /// Every machine and project profile that has settings.
    pub profiles: Vec<SandboxProfile>,
}

/// One edit of a default profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileChange {
    /// Set the level.
    SetLevel {
        /// The level.
        level: SandboxLevel,
    },
    /// Leave the level to the other scopes.
    ClearLevel,
    /// Allow a network domain.
    AddDomain {
        /// The domain, e.g. `"registry.npmjs.org"`.
        domain: String,
    },
    /// Stop allowing a domain.
    RemoveDomain {
        /// The domain.
        domain: String,
    },
    /// Remove a standing grant.
    RemoveGrant {
        /// The grant.
        grant_id: GrantId,
    },
    /// Mediate a command (or change its action).
    SetMediated {
        /// The command and its action.
        mediated: MediatedCommand,
    },
    /// Stop mediating a command.
    RemoveMediated {
        /// The command, as in [`MediatedCommand::command`].
        command: String,
    },
}

/// Params of `sandbox.set_profile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SandboxSetProfileParams {
    /// The machine or project.
    pub scope: ProfileScope,
    /// The edit.
    pub change: ProfileChange,
}
