//! Stale credentials (spec §11.3): a running agent keeps the credential routes, accounts and
//! model policy of its launch; changes reach it only when it restarts (`session.restart`). When
//! its model requests then fail at the connection level (nono's proxy has no route for a
//! provider added after the launch, so pi reports "Connection error."), the summary says why
//! (`stale_credentials`) and the UI offers the restart. Quiet otherwise: a connection failure
//! without a change after the launch, or a change without a failure, sets nothing.
//!
//! The facts:
//!
//! - when each provider's credential or route last changed in Settings › Models (the agent
//!   daemon reports it with [`crate::Runtime::credentials_changed`]) and when each profile's
//!   model policy last changed (`profiles.report`), persisted in the store's `kv` table;
//! - when the agent started (its `meta` record) and whether its last failed model request
//!   failed at the connection level, folded from the transcript, so both survive a restart of
//!   the agent daemon (the agent outlives it).

use std::collections::BTreeMap;

use controlroom_drivers::{DriverEvent, FoldOut, MODEL_OPTION, model_provider};
use controlroom_protocol::ids::{ProfileId, SessionId, UnixMillis};
use controlroom_protocol::session::{OptionControl, RunOutcome};
use controlroom_protocol::timeline::{ItemBody, TimelineOp};
use serde::{Deserialize, Serialize};

use crate::core::Core;
use crate::session::Process;
use crate::store::Store;

/// The `kv` key of [`CredentialChanges`].
const KEY: &str = "credential_changes";

/// What pi (and the HTTP clients under it) report when a request never reached the provider.
const CONNECTION_ERRORS: [&str; 7] = [
    "connection error",
    "fetch failed",
    "econnrefused",
    "econnreset",
    "enotfound",
    "socket hang up",
    "network error",
];

/// When credentials and model policies last changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct CredentialChanges {
    /// By provider (account) id: its key, sign-in or gateway in Settings › Models.
    providers: BTreeMap<String, UnixMillis>,
    /// By profile: its model policy (the accounts it admits).
    profiles: BTreeMap<ProfileId, UnixMillis>,
}

impl CredentialChanges {
    /// The stored changes; none when nothing was stored yet.
    pub(crate) fn load(store: &Store) -> rusqlite::Result<Self> {
        Ok(store.kv_get(KEY)?.unwrap_or_default())
    }

    pub(crate) fn save(&self, store: &Store) -> rusqlite::Result<()> {
        store.kv_set(KEY, self)
    }

    /// Whether what an agent started at `since` depends on changed after it: the credential of
    /// `provider` (of any provider, when the agent's model is unknown) or `profile`'s policy.
    fn changed_after(
        &self,
        provider: Option<&str>,
        profile: Option<ProfileId>,
        since: UnixMillis,
    ) -> bool {
        let provider_changed = match provider {
            Some(id) => self.providers.get(id).is_some_and(|at| *at > since),
            None => self.providers.values().any(|at| *at > since),
        };
        let policy_changed = profile
            .and_then(|id| self.profiles.get(&id))
            .is_some_and(|at| *at > since);
        provider_changed || policy_changed
    }
}

/// Whether an error text says the request never reached the provider.
pub(crate) fn is_connection_error(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    CONNECTION_ERRORS.iter().any(|e| text.contains(e))
}

/// Notes in `process` what a folded record says about model requests: a run or a retry that
/// failed at the connection level sets the flag, a run that completed clears it.
pub(crate) fn observe(process: &mut Process, out: &FoldOut) {
    for op in &out.ops {
        if let TimelineOp::Upsert { item } = op
            && let ItemBody::Retry {
                error: Some(error), ..
            } = &item.body
            && is_connection_error(error)
        {
            process.connection_failed = true;
        }
    }
    for event in &out.events {
        if let DriverEvent::RunSettled { outcome, error } = event {
            if *outcome == RunOutcome::Completed {
                process.connection_failed = false;
            } else if error.as_deref().is_some_and(is_connection_error) {
                process.connection_failed = true;
            }
        }
    }
}

impl Core {
    /// Settings › Models changed the credentials or routes of `providers` now: summaries of
    /// running agents launched before may become stale.
    pub(crate) fn credentials_changed(&mut self, providers: &[String]) {
        let now = self.clock.now();
        let changes = self.credentials_mut();
        for provider in providers {
            changes.providers.insert(provider.clone(), now);
        }
        self.touch_all();
    }

    /// Records the profiles whose model policy differs from the last report (a profile new to
    /// the report counts: before it, its sessions were admitted nothing).
    pub(crate) fn policies_changed(&mut self, profiles: &[ProfileId]) {
        let now = self.clock.now();
        let changes = self.credentials_mut();
        for id in profiles {
            changes.profiles.insert(*id, now);
        }
        self.touch_all();
    }

    /// The summary's `stale_credentials` (see the module docs).
    pub(crate) fn stale_credentials(&self, id: SessionId) -> bool {
        let Some(session) = self.sessions.get(&id) else {
            return false;
        };
        let process = &session.derived.process;
        let Some(started_at) = process.started_at else {
            return false;
        };
        if !process.live || !process.connection_failed {
            return false;
        }
        let provider = session
            .view()
            .options
            .iter()
            .find(|o| o.id == MODEL_OPTION)
            .and_then(|o| match &o.control {
                OptionControl::Select { value, .. } => Some(model_provider(value)),
                _ => None,
            });
        self.credentials
            .changed_after(provider, session.data.profile_id, started_at)
    }

    /// Every running agent's summary is computed again at commit.
    fn touch_all(&mut self) {
        let live: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.derived.process.live)
            .map(|(id, _)| *id)
            .collect();
        for id in live {
            self.touch(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_errors_are_told_apart_from_provider_errors() {
        assert!(is_connection_error("Connection error."));
        assert!(is_connection_error("TypeError: fetch failed"));
        assert!(is_connection_error("connect ECONNREFUSED 127.0.0.1:443"));
        assert!(!is_connection_error("401 invalid x-api-key"));
        assert!(!is_connection_error("503 service unavailable"));
    }

    #[test]
    fn only_changes_after_the_start_count() {
        let profile = ProfileId::generate();
        let mut changes = CredentialChanges::default();
        changes.providers.insert("anthropic".into(), UnixMillis(20));
        changes.profiles.insert(profile, UnixMillis(10));
        assert!(changes.changed_after(Some("anthropic"), None, UnixMillis(15)));
        assert!(!changes.changed_after(Some("openai"), None, UnixMillis(15)));
        assert!(changes.changed_after(None, None, UnixMillis(15)));
        assert!(!changes.changed_after(Some("anthropic"), None, UnixMillis(20)));
        assert!(changes.changed_after(Some("openai"), Some(profile), UnixMillis(5)));
        assert!(!changes.changed_after(Some("openai"), Some(profile), UnixMillis(10)));
    }
}
