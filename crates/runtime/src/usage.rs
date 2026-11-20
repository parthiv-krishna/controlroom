//! A session's cumulative usage, summed from the driver's usage reports like pi's footer
//! (`getSessionStats`): every report adds its tokens and cost, and an agent reply sets the
//! latest cache hit rate. Folded state: a re-fold of the transcript rebuilds it, so it covers
//! the whole session across agent processes and native sessions.
//!
//! The same reports are also counted per provider (pi's id, so per Settings › Models account)
//! in hourly buckets by the record's time ([`ByProvider`]), which the agent daemon sums over its
//! sessions for each account's last day, week and all time.

use std::collections::BTreeMap;

use controlroom_protocol::UnixMillis;
use controlroom_protocol::agentd::UsageTotals;
use controlroom_protocol::session::SessionUsage;
use controlroom_protocol::timeline::TokenUsage;

/// One hour in unix ms: the bucket size of [`ByProvider`].
const HOUR_MS: i64 = 3_600_000;

/// A session's usage per provider id, in buckets by hour (unix ms / [`HOUR_MS`]).
pub(crate) type ByProvider = BTreeMap<String, BTreeMap<i64, UsageTotals>>;

/// One model call's usage, as the driver reported it.
pub(crate) struct Report<'a> {
    pub tokens: &'a TokenUsage,
    pub cost: Option<f64>,
    /// An agent reply.
    pub reply: bool,
    /// The provider that served it, when known.
    pub provider: Option<&'a str>,
    /// The record's time.
    pub at: UnixMillis,
}

/// Adds one model call's usage to the session's totals and to its provider's bucket.
pub(crate) fn add(usage: &mut SessionUsage, by_provider: &mut ByProvider, report: &Report<'_>) {
    let totals = &mut usage.tokens;
    add_tokens(totals, report.tokens);
    usage.cost += report.cost.unwrap_or(0.0);
    if report.reply {
        usage.latest_cache_hit = cache_hit(report.tokens);
    }
    if let Some(provider) = report.provider {
        let bucket = by_provider
            .entry(provider.to_owned())
            .or_default()
            .entry(report.at.0.div_euclid(HOUR_MS))
            .or_default();
        add_tokens(&mut bucket.tokens, report.tokens);
        bucket.cost += report.cost.unwrap_or(0.0);
    }
}

/// Adds `by_provider`'s usage to `sums`: for each provider, one total per `since` (the buckets
/// of the hour holding it and later).
pub(crate) fn sum_since(
    sums: &mut BTreeMap<String, Vec<UsageTotals>>,
    by_provider: &ByProvider,
    since: &[UnixMillis],
) {
    for (provider, buckets) in by_provider {
        let sums = sums
            .entry(provider.clone())
            .or_insert_with(|| vec![UsageTotals::default(); since.len()]);
        for (sum, since) in sums.iter_mut().zip(since) {
            for bucket in buckets.range(since.0.div_euclid(HOUR_MS)..).map(|(_, b)| b) {
                add_tokens(&mut sum.tokens, &bucket.tokens);
                sum.cost += bucket.cost;
            }
        }
    }
}

fn add_tokens(totals: &mut TokenUsage, tokens: &TokenUsage) {
    totals.input += tokens.input;
    totals.output += tokens.output;
    totals.cache_read += tokens.cache_read;
    totals.cache_write += tokens.cache_write;
}

/// Cache reads over prompt tokens; `None` without prompt tokens.
fn cache_hit(tokens: &TokenUsage) -> Option<f64> {
    let prompt = tokens.input + tokens.cache_read + tokens.cache_write;
    (prompt > 0).then(|| tokens.cache_read as f64 / prompt as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: u64, output: u64, cache_read: u64, cache_write: u64) -> TokenUsage {
        TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    fn report<'a>(
        tokens: &'a TokenUsage,
        cost: Option<f64>,
        reply: bool,
        provider: Option<&'a str>,
        at: i64,
    ) -> Report<'a> {
        Report {
            tokens,
            cost,
            reply,
            provider,
            at: UnixMillis(at),
        }
    }

    #[test]
    fn reports_add_up_and_only_replies_set_the_cache_hit() {
        let mut usage = SessionUsage::default();
        let mut by = ByProvider::default();
        let mut push = |usage: &mut SessionUsage, t: TokenUsage, cost, reply| {
            add(usage, &mut by, &report(&t, cost, reply, None, 0));
        };
        push(&mut usage, tokens(10, 5, 90, 0), Some(0.5), true);
        // A compaction's or a tool's model call counts, but leaves the latest reply's rate.
        push(&mut usage, tokens(100, 20, 0, 0), Some(0.25), false);
        push(&mut usage, tokens(1, 1, 0, 0), None, false);
        assert_eq!(usage.tokens, tokens(111, 26, 90, 0));
        assert_eq!(usage.cost, 0.75);
        assert_eq!(usage.latest_cache_hit, Some(0.9));
        push(&mut usage, tokens(30, 1, 50, 20), None, true);
        assert_eq!(usage.latest_cache_hit, Some(0.5));
        assert!(by.is_empty(), "no provider, no bucket");
    }

    #[test]
    fn a_reply_without_prompt_tokens_clears_the_cache_hit() {
        let mut usage = SessionUsage::default();
        let mut by = ByProvider::default();
        add(
            &mut usage,
            &mut by,
            &report(&tokens(10, 5, 90, 0), None, true, None, 0),
        );
        add(
            &mut usage,
            &mut by,
            &report(&tokens(0, 0, 0, 0), None, true, None, 0),
        );
        assert_eq!(usage.latest_cache_hit, None);
        assert_eq!(usage.tokens, tokens(10, 5, 90, 0));
    }

    #[test]
    fn providers_are_counted_by_hour_and_summed_since() {
        let mut usage = SessionUsage::default();
        let mut by = ByProvider::default();
        let day = 24 * HOUR_MS;
        let t = tokens(10, 1, 0, 0);
        for (provider, at, cost) in [
            ("anthropic", 0, 1.0),
            ("anthropic", 10 * day + 5, 2.0),
            ("anthropic-work", 10 * day + HOUR_MS + 1, 4.0),
            ("anthropic", 10 * day + 2 * HOUR_MS, 8.0),
        ] {
            add(
                &mut usage,
                &mut by,
                &report(&t, Some(cost), true, Some(provider), at),
            );
        }
        assert_eq!(by["anthropic"].len(), 3);
        let mut sums = BTreeMap::new();
        // Since the middle of hour 1 of day 10: that hour's bucket counts, as do later ones.
        let since = [UnixMillis(0), UnixMillis(10 * day + HOUR_MS + 30)];
        sum_since(&mut sums, &by, &since);
        sum_since(&mut sums, &by, &since);
        let cost = |p: &str| sums[p].iter().map(|s| s.cost).collect::<Vec<_>>();
        // Added twice (two sessions).
        assert_eq!(cost("anthropic"), [22.0, 16.0]);
        assert_eq!(cost("anthropic-work"), [8.0, 8.0]);
        assert_eq!(sums["anthropic"][0].tokens, tokens(60, 6, 0, 0));
    }
}
