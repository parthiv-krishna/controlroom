//! Sandbox defaults per machine and project (spec §11.6). Each edit is applied inside one
//! transaction, so concurrent edits of the same profile never lose each other.

use controlroom_protocol::ids::{GrantId, MachineId, ProjectId, UnixMillis};
use controlroom_protocol::session::{
    DeviceRef, GrantScope, PathAccess, SandboxGrant, SandboxLevel,
};
use controlroom_protocol::ui::{NewGrant, ProfileChange, ProfileScope, SandboxProfile};
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
            never: json_col(row, 6)?,
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

const COLUMNS: &str = "scope_kind, scope_id, level, grants, domains, mediated, never";

/// Applies one change to a profile; `by` made it.
fn apply(profile: &mut SandboxProfile, change: &ProfileChange, by: Option<&DeviceRef>) {
    match change {
        ProfileChange::SetLevel { level } => profile.level = Some(*level),
        ProfileChange::ClearLevel => profile.level = None,
        ProfileChange::AddDomain { domain } => {
            if !profile.domains.contains(domain) {
                profile.domains.push(domain.clone());
            }
        }
        ProfileChange::RemoveDomain { domain } => profile.domains.retain(|d| d != domain),
        ProfileChange::AddGrant { grant } => add_grant(profile, grant, by),
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
        ProfileChange::AddNever { path } => {
            if !profile.never.contains(path) {
                profile.never.push(path.clone());
            }
        }
        ProfileChange::RemoveNever { path } => profile.never.retain(|p| p != path),
    }
}

/// Adds a standing path grant, or widens the access of the same path granted before.
fn add_grant(profile: &mut SandboxProfile, grant: &NewGrant, by: Option<&DeviceRef>) {
    let wider = |a: Option<PathAccess>, b: Option<PathAccess>| {
        if a == Some(PathAccess::ReadWrite) || b == Some(PathAccess::ReadWrite) {
            Some(PathAccess::ReadWrite)
        } else {
            a.or(b)
        }
    };
    match profile
        .grants
        .iter_mut()
        .find(|g| g.kind == grant.kind && g.value == grant.value)
    {
        Some(existing) => existing.access = wider(existing.access, grant.access),
        None => profile.grants.push(SandboxGrant {
            id: GrantId::generate(),
            kind: grant.kind,
            value: grant.value.clone(),
            access: grant.access,
            scope: GrantScope::Standing,
            granted_at: UnixMillis::now(),
            by: by.cloned(),
        }),
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

    /// Applies `change`, made by device `by`, to the profile of `scope`, creating it if needed.
    /// A profile left with no settings is deleted.
    pub fn change(
        &self,
        scope: &ProfileScope,
        change: &ProfileChange,
        by: Option<&DeviceRef>,
    ) -> StoreResult<()> {
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
                    never: Vec::new(),
                });
            apply(&mut profile, change, by);
            let empty = profile.level.is_none()
                && profile.grants.is_empty()
                && profile.domains.is_empty()
                && profile.mediated.is_empty()
                && profile.never.is_empty();
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
                                               domains, mediated, never)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (owner_id, scope_kind, scope_id) DO UPDATE SET
                     level = excluded.level, grants = excluded.grants,
                     domains = excluded.domains, mediated = excluded.mediated,
                     never = excluded.never",
                rusqlite::params![
                    self.owner.as_str(),
                    kind,
                    id,
                    profile.level.as_ref().map(enum_str).transpose()?,
                    to_json(&profile.grants)?,
                    to_json(&profile.domains)?,
                    to_json(&profile.mediated)?,
                    to_json(&profile.never)?,
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
    use controlroom_protocol::session::GrantKind;

    fn empty() -> SandboxProfile {
        SandboxProfile {
            scope: ProfileScope::Unknown,
            level: None,
            grants: vec![],
            domains: vec![],
            mediated: vec![],
            never: vec![],
        }
    }

    #[test]
    fn changes_edit_lists_without_duplicates() {
        let mut p = empty();
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
            apply(&mut p, &change, None);
        }
        assert_eq!(p.domains, ["example.com"]);
        assert_eq!(p.mediated.len(), 1);
        assert_eq!(p.mediated[0].action, MediationAction::Deny);
        assert_eq!(p.level, Some(SandboxLevel::Relaxed));
        apply(&mut p, &ProfileChange::ClearLevel, None);
        assert_eq!(p.level, None);
    }

    #[test]
    fn path_grants_widen_and_never_paths_are_a_set() {
        let mut p = empty();
        let device = DeviceRef {
            device_id: controlroom_protocol::ids::DeviceId::new("laptop"),
            label: "laptop".into(),
        };
        let grant = |access| ProfileChange::AddGrant {
            grant: NewGrant {
                kind: GrantKind::Path,
                value: "~/.cache/nix".into(),
                access: Some(access),
            },
        };
        apply(&mut p, &grant(PathAccess::Read), Some(&device));
        apply(&mut p, &grant(PathAccess::ReadWrite), None);
        apply(&mut p, &grant(PathAccess::Read), None);
        assert_eq!(p.grants.len(), 1);
        let g = &p.grants[0];
        assert_eq!(
            (g.kind, g.value.as_str(), g.access, g.scope),
            (
                GrantKind::Path,
                "~/.cache/nix",
                Some(PathAccess::ReadWrite),
                GrantScope::Standing
            )
        );
        assert_eq!(g.by.as_ref(), Some(&device));

        let never = |path: &str| ProfileChange::AddNever { path: path.into() };
        apply(&mut p, &never("~/.bash_history"), None);
        apply(&mut p, &never("~/.bash_history"), None);
        apply(&mut p, &never("/srv/secret"), None);
        assert_eq!(p.never, ["~/.bash_history", "/srv/secret"]);
        apply(
            &mut p,
            &ProfileChange::RemoveNever {
                path: "~/.bash_history".into(),
            },
            None,
        );
        assert_eq!(p.never, ["/srv/secret"]);
    }
}
