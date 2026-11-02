//! Sandbox defaults per machine and project (spec §11.6). Each edit is applied inside one
//! transaction, so concurrent edits of the same profile never lose each other.

use controlroom_protocol::ids::{MachineId, ProjectId};
use controlroom_protocol::session::SandboxLevel;
use controlroom_protocol::ui::{ProfileChange, ProfileScope, SandboxProfile};
use rusqlite::OptionalExtension;

use super::{OwnerId, Store, StoreError, StoreResult, enum_str, json_col, parse, to_json};

/// Owner-scoped sandbox profile operations.
pub struct SandboxProfiles<'a> {
    store: &'a Store,
    owner: &'a OwnerId,
}

fn scope_key(scope: &ProfileScope) -> StoreResult<(&'static str, String)> {
    match scope {
        ProfileScope::Machine { machine_id } => Ok(("machine", machine_id.to_string())),
        ProfileScope::Project { project_id } => Ok(("project", project_id.to_string())),
        ProfileScope::Unknown => Err(StoreError::InvalidValue {
            column: "sandbox_profiles.scope",
            value: "unknown".into(),
        }),
    }
}

type RawProfile = (String, String, Option<String>, SandboxProfile);

fn map(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawProfile> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        SandboxProfile {
            scope: ProfileScope::Unknown,
            level: None,
            grants: json_col(row, 3)?,
            domains: json_col(row, 4)?,
            mediated: json_col(row, 5)?,
        },
    ))
}

fn from_raw((kind, id, level, mut profile): RawProfile) -> StoreResult<SandboxProfile> {
    profile.scope = match kind.as_str() {
        "machine" => ProfileScope::Machine {
            machine_id: parse::<MachineId>("sandbox_profiles.scope_id", &id)?,
        },
        _ => ProfileScope::Project {
            project_id: parse::<ProjectId>("sandbox_profiles.scope_id", &id)?,
        },
    };
    profile.level = level
        .map(|l| serde_json::from_value::<SandboxLevel>(serde_json::Value::String(l)))
        .transpose()?;
    Ok(profile)
}

const COLUMNS: &str = "scope_kind, scope_id, level, grants, domains, mediated";

/// Applies one change to a profile.
fn apply(profile: &mut SandboxProfile, change: &ProfileChange) {
    match change {
        ProfileChange::SetLevel { level } => profile.level = Some(*level),
        ProfileChange::ClearLevel => profile.level = None,
        ProfileChange::AddDomain { domain } => {
            if !profile.domains.contains(domain) {
                profile.domains.push(domain.clone());
            }
        }
        ProfileChange::RemoveDomain { domain } => profile.domains.retain(|d| d != domain),
        ProfileChange::RemoveGrant { grant_id } => profile.grants.retain(|g| g.id != *grant_id),
        ProfileChange::SetMediated { mediated } => {
            match profile
                .mediated
                .iter_mut()
                .find(|m| m.command == mediated.command)
            {
                Some(existing) => existing.action = mediated.action,
                None => profile.mediated.push(mediated.clone()),
            }
        }
        ProfileChange::RemoveMediated { command } => {
            profile.mediated.retain(|m| m.command != *command)
        }
    }
}

impl<'a> SandboxProfiles<'a> {
    pub(super) fn new(store: &'a Store, owner: &'a OwnerId) -> Self {
        Self { store, owner }
    }

    /// Every profile that has settings.
    pub fn list(&self) -> StoreResult<Vec<SandboxProfile>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM sandbox_profiles WHERE owner_id = ?1 ORDER BY scope_kind, scope_id"
        ))?;
        let rows = stmt
            .query_map([self.owner.as_str()], map)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(from_raw).collect()
    }

    /// Applies `change` to the profile of `scope`, creating it if needed. A profile left with
    /// no settings is deleted.
    pub fn change(&self, scope: &ProfileScope, change: &ProfileChange) -> StoreResult<()> {
        let (kind, id) = scope_key(scope)?;
        self.store.transaction(|tx| {
            let mut profile = tx
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM sandbox_profiles
                         WHERE owner_id = ?1 AND scope_kind = ?2 AND scope_id = ?3"
                    ),
                    (self.owner.as_str(), kind, &id),
                    map,
                )
                .optional()?
                .map(from_raw)
                .transpose()?
                .unwrap_or(SandboxProfile {
                    scope: scope.clone(),
                    level: None,
                    grants: Vec::new(),
                    domains: Vec::new(),
                    mediated: Vec::new(),
                });
            apply(&mut profile, change);
            let empty = profile.level.is_none()
                && profile.grants.is_empty()
                && profile.domains.is_empty()
                && profile.mediated.is_empty();
            if empty {
                tx.execute(
                    "DELETE FROM sandbox_profiles
                     WHERE owner_id = ?1 AND scope_kind = ?2 AND scope_id = ?3",
                    (self.owner.as_str(), kind, &id),
                )?;
                return Ok(());
            }
            tx.execute(
                "INSERT INTO sandbox_profiles (owner_id, scope_kind, scope_id, level, grants,
                                               domains, mediated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (owner_id, scope_kind, scope_id) DO UPDATE SET
                     level = excluded.level, grants = excluded.grants,
                     domains = excluded.domains, mediated = excluded.mediated",
                rusqlite::params![
                    self.owner.as_str(),
                    kind,
                    id,
                    profile.level.as_ref().map(enum_str).transpose()?,
                    to_json(&profile.grants)?,
                    to_json(&profile.domains)?,
                    to_json(&profile.mediated)?,
                ],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::session::{MediatedCommand, MediationAction};

    use super::*;

    #[test]
    fn changes_edit_lists_without_duplicates() {
        let mut p = SandboxProfile {
            scope: ProfileScope::Unknown,
            level: None,
            grants: vec![],
            domains: vec![],
            mediated: vec![],
        };
        let push = |action| ProfileChange::SetMediated {
            mediated: MediatedCommand {
                command: "git push".into(),
                action,
            },
        };
        for change in [
            ProfileChange::AddDomain {
                domain: "example.com".into(),
            },
            ProfileChange::AddDomain {
                domain: "example.com".into(),
            },
            push(MediationAction::Approve),
            push(MediationAction::Deny),
            ProfileChange::SetLevel {
                level: SandboxLevel::Relaxed,
            },
        ] {
            apply(&mut p, &change);
        }
        assert_eq!(p.domains, ["example.com"]);
        assert_eq!(p.mediated.len(), 1);
        assert_eq!(p.mediated[0].action, MediationAction::Deny);
        assert_eq!(p.level, Some(SandboxLevel::Relaxed));
        apply(&mut p, &ProfileChange::ClearLevel);
        assert_eq!(p.level, None);
    }
}
