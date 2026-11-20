//! Named accounts of a provider (Settings › Models): several keys or sign-ins for one provider,
//! e.g. a personal and a work subscription.
//!
//! A provider's first account is the provider itself: the credential stored before accounts
//! existed is its account `default`, unchanged and renamable. Each added account is a provider
//! of its own for pi, with an id derived from its name when added (`anthropic` + `work` →
//! `anthropic-work`) and kept on rename, so sessions, defaults and usage keep following it.
//! Accounts are offered for presets and UI gateways: their routes are known. A preset the
//! server's config declares keeps its server credential as its first account, and UI accounts
//! can be added next to it; an account never takes a server-declared id.

use std::collections::BTreeMap;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::agentd::{AccountUsage, ModelAccount, UsageTotals};

use super::presets;
use super::store::Stored;

/// Longest account name.
const MAX_NAME_LEN: usize = 24;

/// What sessions used, per provider id since each of the given times (the runtime's
/// `provider_usage`).
pub type UsageSource =
    std::sync::Arc<dyn Fn(&[UnixMillis]) -> BTreeMap<String, Vec<UsageTotals>> + Send + Sync>;

/// A valid account name: 1-24 lowercase letters, digits and dashes, starting with a letter or
/// digit.
pub fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let ok = (1..=MAX_NAME_LEN).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(name.to_owned())
    } else {
        Err(format!(
            "an account name is 1-{MAX_NAME_LEN} lowercase letters, digits and dashes"
        ))
    }
}

/// Whether `id` can have accounts: a preset, or a gateway set up here.
pub fn can_have_accounts(stored: &Stored, id: &str) -> bool {
    presets::preset(id).is_some()
        || stored
            .providers
            .get(id)
            .is_some_and(|p| p.gateway.is_some() && p.account_of.is_none())
}

/// The id of `base`'s new account `name`: `<base>-<name>`, or with a number appended while
/// `taken` (by a preset, another provider, or one pi knows).
pub fn new_id(base: &str, name: &str, taken: impl Fn(&str) -> bool) -> String {
    let id = format!("{base}-{name}");
    if !taken(&id) {
        return id;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{id}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The accounts of every provider with added ones: its first account, then the added ones.
pub fn list(stored: &Stored) -> Vec<ModelAccount> {
    let mut bases: Vec<&str> = stored
        .providers
        .values()
        .filter_map(|p| p.account_of.as_deref())
        .collect();
    bases.sort_unstable();
    bases.dedup();
    let mut out = Vec::new();
    for base in bases {
        let account = |id: &str| ModelAccount {
            provider: id.to_owned(),
            of: base.to_owned(),
            name: stored.account_name(id),
        };
        out.push(account(base));
        out.extend(stored.accounts_of(base).map(account));
    }
    out
}

/// Each provider's usage over the last day, week and all time, from `source` at `now`.
pub fn usage(source: &UsageSource, now: UnixMillis) -> BTreeMap<String, AccountUsage> {
    const DAY_MS: i64 = 24 * 3_600_000;
    let since = [
        UnixMillis(now.0 - DAY_MS),
        UnixMillis(now.0 - 7 * DAY_MS),
        UnixMillis(i64::MIN),
    ];
    source(&since)
        .into_iter()
        .filter_map(|(provider, sums)| match sums.as_slice() {
            [day, week, total] => Some((
                provider,
                AccountUsage {
                    day: *day,
                    week: *week,
                    total: *total,
                },
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use controlroom_protocol::agentd::Gateway;

    use super::super::store::StoredProvider;
    use super::*;

    #[test]
    fn names() {
        assert_eq!(valid_name(" work ").unwrap(), "work");
        for ok in ["personal", "w2", "team-a", "0"] {
            assert!(valid_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Work", "-x", "a b", "a_b", "x".repeat(25).as_str()] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_avoid_taken_ones() {
        assert_eq!(new_id("anthropic", "work", |_| false), "anthropic-work");
        // `openai` + `codex` would be pi's own `openai-codex`.
        let taken = ["openai-codex", "openai-codex-2"];
        assert_eq!(
            new_id("openai", "codex", |id| taken.contains(&id)),
            "openai-codex-3"
        );
    }

    #[test]
    fn accounts_and_usage() {
        let mut stored = Stored::default();
        let account = |of: &str, name: &str| StoredProvider {
            account_of: Some(of.into()),
            name: Some(name.into()),
            ..Default::default()
        };
        stored
            .providers
            .insert("anthropic-work".into(), account("anthropic", "work"));
        stored
            .providers
            .insert("anthropic-a".into(), account("anthropic", "acme"));
        stored.providers.insert(
            "anthropic".into(),
            StoredProvider {
                name: Some("personal".into()),
                ..Default::default()
            },
        );
        stored.providers.insert(
            "gw".into(),
            StoredProvider {
                gateway: Some(Gateway {
                    base_url: "https://gw.example.com/v1".into(),
                    models: vec!["m".into()],
                    auth_header: controlroom_protocol::agentd::AuthHeader::Bearer,
                }),
                ..Default::default()
            },
        );
        let names: Vec<(String, String, String)> = list(&stored)
            .into_iter()
            .map(|a| (a.provider, a.of, a.name))
            .collect();
        let row = |p: &str, n: &str| (p.to_owned(), "anthropic".to_owned(), n.to_owned());
        assert_eq!(
            names,
            [
                row("anthropic", "personal"),
                row("anthropic-a", "acme"),
                row("anthropic-work", "work")
            ]
        );
        assert!(can_have_accounts(&stored, "gw") && can_have_accounts(&stored, "openai"));
        assert!(!can_have_accounts(&stored, "anthropic-work"));
        assert!(!can_have_accounts(&stored, "mock"));

        let source: UsageSource = std::sync::Arc::new(|since: &[UnixMillis]| {
            assert_eq!(since[0], UnixMillis(1_000_000_000 - 24 * 3_600_000));
            let sums = since
                .iter()
                .enumerate()
                .map(|(n, _)| UsageTotals {
                    cost: n as f64,
                    ..Default::default()
                })
                .collect();
            BTreeMap::from([("anthropic-work".to_owned(), sums)])
        });
        let usage = usage(&source, UnixMillis(1_000_000_000));
        let work = usage["anthropic-work"];
        assert_eq!(
            (work.day.cost, work.week.cost, work.total.cost),
            (0.0, 1.0, 2.0)
        );
    }
}
