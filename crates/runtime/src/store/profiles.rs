//! The profiles' policies (`profile_policies`): the controller's last `profiles.report`, kept
//! so launches after a restart don't wait for the controller.

use controlroom_protocol::agentd::ProfilePolicy;
use controlroom_protocol::ids::UnixMillis;
use rusqlite::params;

use super::{Store, json};

impl Store {
    /// Every reported profile's policy.
    pub(crate) fn load_profiles(&self) -> rusqlite::Result<Vec<ProfilePolicy>> {
        let mut stmt = self.conn.prepare(
            "SELECT profile_id, models, model_defaults, private FROM profile_policies
             ORDER BY profile_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, models, defaults, private) = row?;
            let decoded = (
                id.parse(),
                serde_json::from_str(&models),
                serde_json::from_str(&defaults),
            );
            match decoded {
                (Ok(profile_id), Ok(models), Ok(model_defaults)) => out.push(ProfilePolicy {
                    profile_id,
                    models,
                    model_defaults,
                    private,
                }),
                // Unreadable: left out, so the profile admits nothing until the next report.
                _ => tracing::warn!("profile {id}: its stored policy doesn't decode"),
            }
        }
        Ok(out)
    }

    /// Replaces every policy with `profiles`, reported at `at`.
    pub(crate) fn save_profiles(
        &self,
        profiles: &[ProfilePolicy],
        at: UnixMillis,
    ) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM profile_policies", [])?;
        for p in profiles {
            self.conn.execute(
                "INSERT INTO profile_policies (profile_id, models, model_defaults, private,
                 reported_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    p.profile_id.to_string(),
                    json(&p.models),
                    json(&p.model_defaults),
                    p.private,
                    at.0
                ],
            )?;
        }
        Ok(())
    }
}
