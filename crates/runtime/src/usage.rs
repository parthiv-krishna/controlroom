//! A session's cumulative usage, summed from the driver's usage reports like pi's footer
//! (`getSessionStats`): every report adds its tokens and cost, and an agent reply sets the
//! latest cache hit rate. Folded state: a re-fold of the transcript rebuilds it, so it covers
//! the whole session across agent processes and native sessions.

use controlroom_protocol::session::SessionUsage;
use controlroom_protocol::timeline::TokenUsage;

/// Adds one model call's usage to the session's totals.
pub(crate) fn add(usage: &mut SessionUsage, tokens: &TokenUsage, cost: Option<f64>, reply: bool) {
    let totals = &mut usage.tokens;
    totals.input += tokens.input;
    totals.output += tokens.output;
    totals.cache_read += tokens.cache_read;
    totals.cache_write += tokens.cache_write;
    usage.cost += cost.unwrap_or(0.0);
    if reply {
        usage.latest_cache_hit = cache_hit(tokens);
    }
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

    #[test]
    fn reports_add_up_and_only_replies_set_the_cache_hit() {
        let mut usage = SessionUsage::default();
        add(&mut usage, &tokens(10, 5, 90, 0), Some(0.5), true);
        assert_eq!(usage.latest_cache_hit, Some(0.9));
        // A compaction's or a tool's model call counts, but leaves the latest reply's rate.
        add(&mut usage, &tokens(100, 20, 0, 0), Some(0.25), false);
        add(&mut usage, &tokens(1, 1, 0, 0), None, false);
        assert_eq!(usage.tokens, tokens(111, 26, 90, 0));
        assert_eq!(usage.cost, 0.75);
        assert_eq!(usage.latest_cache_hit, Some(0.9));
        add(&mut usage, &tokens(30, 1, 50, 20), None, true);
        assert_eq!(usage.latest_cache_hit, Some(0.5));
    }

    #[test]
    fn a_reply_without_prompt_tokens_clears_the_cache_hit() {
        let mut usage = SessionUsage::default();
        add(&mut usage, &tokens(10, 5, 90, 0), None, true);
        add(&mut usage, &tokens(0, 0, 0, 0), None, true);
        assert_eq!(usage.latest_cache_hit, None);
        assert_eq!(usage.tokens, tokens(10, 5, 90, 0));
    }
}
