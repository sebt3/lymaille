//! Token usage of a session: current size of what would be sent, fill rate, cumulated usage per agent for the cost audit,
//! and local estimate when the provider reported nothing reliable (`agent-sessions/context-window#FR-1`, `FR-8`, `AC-1`).
//! Sizes are integers: the margin and the conversions are computed without floating point.

use std::collections::BTreeMap;
use std::num::NonZeroU64;

/// Estimate of the number of tokens of `chars` characters, rounded up, with a margin of `margin_percent` percent.
#[must_use]
pub fn estimate_tokens(chars: u64, chars_per_token_milli: NonZeroU64, margin_percent: u64) -> u64 {
    margin(tokens_of_chars(chars, chars_per_token_milli), margin_percent)
}

/// Tokens of `chars` characters at `chars_per_token_milli` thousandths of a character per token, rounded up.
fn tokens_of_chars(chars: u64, chars_per_token_milli: NonZeroU64) -> u64 {
    saturate(
        u128::from(chars)
            .saturating_mul(1000)
            .div_ceil(u128::from(chars_per_token_milli.get())),
    )
}

/// `tokens` raised by `margin_percent` percent, rounded up: `(tokens * (100 + pct) + 99) / 100`.
fn margin(tokens: u64, margin_percent: u64) -> u64 {
    saturate(
        u128::from(tokens)
            .saturating_mul(u128::from(margin_percent).saturating_add(100))
            .div_ceil(100),
    )
}

/// Narrows a value to `u64`, saturating at `u64::MAX`.
fn saturate(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Base of the session size: the last usage reported by the provider, or a local estimate that carries the margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Base {
    tokens: u64,
    estimated: bool,
}

/// Usage of one agent: the cumulated tokens, and the last prompt the provider reported for it since the last
/// compaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AgentUsage {
    cumulative: u64,
    last_reported: Option<u64>,
}

/// Usage of one session. A value type: it is `Send`, and a caller that shares it between threads holds the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UsageTracker {
    margin_percent: u64,
    chars_per_token_milli: NonZeroU64,
    base: Base,
    pending_added: u64,
    agents: BTreeMap<String, AgentUsage>,
}

impl UsageTracker {
    /// Builds a tracker with the margin of local estimates in percent (10 by default) and the ratio of characters per
    /// token in thousandths (2500 by default). Returns nothing for a ratio of zero.
    #[must_use]
    pub fn new(margin_percent: u64, chars_per_token_milli: u64) -> Option<Self> {
        Some(Self {
            margin_percent,
            chars_per_token_milli: NonZeroU64::new(chars_per_token_milli)?,
            base: Base {
                tokens: 0,
                estimated: true,
            },
            pending_added: 0,
            agents: BTreeMap::new(),
        })
    }

    /// Records one response of `agent`: the prompt and completion usage the provider reported, when it did, and the
    /// local estimate of the prompt in tokens, without margin.
    ///
    /// A reported prompt is kept as the session size unless it is zero or lower than the previous report of the same
    /// agent without a compaction in between; the estimate replaces it then, with the margin. The cumulated usage of the
    /// agent takes the prompt kept (reported, or estimated with the margin) and the reported completion (zero when the
    /// provider reported none).
    pub fn record_response(
        &mut self,
        agent: &str,
        reported_prompt: Option<u64>,
        reported_completion: Option<u64>,
        estimated_prompt: u64,
    ) {
        let estimate_with_margin = margin(estimated_prompt, self.margin_percent);
        let usage = self.agents.entry(agent.to_owned()).or_default();
        let trusted = reported_prompt
            .filter(|&tokens| tokens > 0 && usage.last_reported.is_none_or(|previous| tokens >= previous));
        let (base, kept_prompt) = match trusted {
            Some(tokens) => {
                usage.last_reported = Some(tokens);
                (
                    Base {
                        tokens,
                        estimated: false,
                    },
                    tokens,
                )
            }
            None => (
                Base {
                    tokens: estimated_prompt,
                    estimated: true,
                },
                estimate_with_margin,
            ),
        };
        usage.cumulative = usage
            .cumulative
            .saturating_add(kept_prompt)
            .saturating_add(reported_completion.unwrap_or(0));
        self.base = base;
        self.pending_added = 0;
    }

    /// Notes the size in tokens, without margin, of a message added since the last call.
    pub const fn note_added(&mut self, tokens: u64) {
        self.pending_added = self.pending_added.saturating_add(tokens);
    }

    /// Notes that the session was compacted: the session may be smaller than the previous report, so a smaller report
    /// is accepted from now on.
    pub fn note_compaction(&mut self) {
        for usage in self.agents.values_mut() {
            usage.last_reported = None;
        }
    }

    /// Sets the estimated size in tokens, without margin, of the session after a compaction. Messages added before are
    /// part of that size.
    pub const fn set_session_estimate(&mut self, tokens: u64) {
        self.base = Base {
            tokens,
            estimated: true,
        };
        self.pending_added = 0;
    }

    /// Returns the size `SESSION` in tokens: the base, reported as is or estimated with the margin, plus the messages
    /// added since the last response, with the margin.
    #[must_use]
    pub fn session_tokens(&self) -> u64 {
        let base = if self.base.estimated {
            margin(self.base.tokens, self.margin_percent)
        } else {
            self.base.tokens
        };
        base.saturating_add(margin(self.pending_added, self.margin_percent))
    }

    /// Returns the fill rate in thousandths of `max`, without clamping. A `max` of zero gives `u64::MAX`.
    #[must_use]
    pub fn fill_permille(&self, max: u64) -> u64 {
        self.session_tokens()
            .saturating_mul(1000)
            .checked_div(max)
            .unwrap_or(u64::MAX)
    }

    /// Returns the cumulated prompt and completion tokens of `agent`, or the total of the session when `agent` is
    /// `None`. The total never decreases, compaction included.
    #[must_use]
    pub fn cumulative(&self, agent: Option<&str>) -> u64 {
        if let Some(name) = agent {
            return self.agents.get(name).map_or(0, |usage| usage.cumulative);
        }
        self.agents
            .values()
            .fold(0, |total, usage| total.saturating_add(usage.cumulative))
    }

    /// Estimates the tokens of `chars` characters with this tracker's ratio and margin.
    #[must_use]
    pub fn estimate_chars(&self, chars: u64) -> u64 {
        estimate_tokens(chars, self.chars_per_token_milli, self.margin_percent)
    }
}

#[cfg(test)]
mod tests {
    use super::{UsageTracker, estimate_tokens};
    use std::num::NonZeroU64;

    fn tracker() -> UsageTracker {
        UsageTracker::new(10, 2500).expect("2500 is not zero")
    }

    #[test]
    fn reported_usage_is_the_truth() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), Some(100), 4000);
        assert_eq!(
            usage.session_tokens(),
            5000,
            "a reported prompt of 5000 is used as is, not replaced by the estimate and not marked up"
        );
    }

    #[test]
    fn estimate_replaces_missing_usage() {
        let mut usage = tracker();
        usage.record_response("main", None, None, 1000);
        assert_eq!(
            usage.session_tokens(),
            1100,
            "a local estimate of 1000 tokens carries a 10 percent margin"
        );
    }

    #[test]
    fn zero_or_lower_usage_is_replaced_by_the_estimate() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), None, 5200);
        usage.record_response("main", Some(3000), None, 5200);
        assert_eq!(
            usage.session_tokens(),
            5720,
            "a report below the previous one without compaction is replaced by the estimate with margin"
        );
    }

    #[test]
    fn lower_usage_accepted_after_a_compaction() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), None, 5000);
        usage.note_compaction();
        usage.record_response("main", Some(800), None, 800);
        assert_eq!(
            usage.session_tokens(),
            800,
            "after a compaction a smaller report is accepted"
        );
    }

    #[test]
    fn added_messages_count_with_margin() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), None, 0);
        usage.note_added(100);
        usage.note_added(50);
        assert_eq!(
            usage.session_tokens(),
            5165,
            "5000 reported plus 150 added tokens with a 10 percent margin (165)"
        );
    }

    #[test]
    fn added_messages_reset_on_new_usage() {
        let mut usage = tracker();
        usage.note_added(100);
        usage.record_response("main", Some(5000), None, 0);
        assert_eq!(
            usage.session_tokens(),
            5000,
            "the pending added size is zero once a new usage is recorded"
        );
    }

    #[test]
    fn cumulative_per_agent() {
        let mut usage = tracker();
        usage.record_response("main", Some(3000), Some(200), 0);
        usage.record_response("main", Some(3500), Some(300), 0);
        usage.record_response("sub1", Some(1000), Some(100), 0);
        assert_eq!(usage.cumulative(Some("main")), 7000);
        assert_eq!(usage.cumulative(Some("sub1")), 1100);
        assert_eq!(usage.cumulative(None), 8100);
    }

    #[test]
    fn fill_rate() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), None, 0);
        assert_eq!(usage.fill_permille(20_000), 250);
    }

    #[test]
    fn estimate_rounds_up_twice() {
        let ratio = NonZeroU64::new(2500).expect("2500 is not zero");
        assert_eq!(
            estimate_tokens(1001, ratio, 0),
            401,
            "1001 characters at 2.5 characters per token are 400.4 tokens, rounded up to 401"
        );
        assert_eq!(
            estimate_tokens(1001, ratio, 10),
            442,
            "401 tokens with a 10 percent margin are 441.1, rounded up to 442"
        );
    }

    #[test]
    fn zero_reported_prompt_is_replaced_by_the_estimate() {
        let mut usage = tracker();
        usage.record_response("main", Some(0), None, 1000);
        assert_eq!(
            usage.session_tokens(),
            1100,
            "a reported prompt of zero is not usage: the estimate of 1000 tokens carries its margin"
        );
    }

    #[test]
    fn cumulative_never_decreases_across_a_compaction() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), Some(100), 0);
        let before = usage.cumulative(Some("main"));
        usage.note_compaction();
        usage.record_response("main", Some(800), None, 800);
        let after = usage.cumulative(Some("main"));
        assert!(
            after >= before,
            "the cumulated usage must not decrease at a compaction"
        );
        assert_eq!(
            after, 5900,
            "the 800 reported after the compaction is added to the 5100 already counted"
        );
        assert_eq!(
            usage.cumulative(None),
            5900,
            "the session total follows the agent total"
        );
    }

    #[test]
    fn cumulative_saturates() {
        let mut usage = tracker();
        usage.record_response("main", Some(u64::MAX), Some(1), 0);
        usage.record_response("main", Some(u64::MAX), None, 0);
        usage.record_response("sub1", Some(10), Some(10), 0);
        assert_eq!(
            usage.cumulative(Some("main")),
            u64::MAX,
            "the cumulated usage of an agent saturates instead of wrapping"
        );
        assert_eq!(
            usage.cumulative(None),
            u64::MAX,
            "the session total saturates instead of wrapping"
        );
    }

    #[test]
    fn cumulative_counts_the_estimated_prompt_with_margin() {
        let mut usage = tracker();
        usage.record_response("main", None, Some(100), 1000);
        assert_eq!(
            usage.cumulative(Some("main")),
            1200,
            "an estimated prompt of 1000 counts 1100 with its margin, plus the 100 completion tokens"
        );
    }

    #[test]
    fn lower_report_is_checked_per_agent() {
        let mut usage = tracker();
        usage.record_response("main", Some(5000), None, 0);
        usage.record_response("sub1", Some(1000), Some(100), 0);
        usage.record_response("sub1", Some(800), Some(50), 900);
        assert_eq!(
            usage.session_tokens(),
            990,
            "the report of 800 is lower than the previous one of sub1: the estimate of 900 with margin replaces it"
        );
        assert_eq!(
            usage.cumulative(Some("sub1")),
            2140,
            "sub1 counts 1000 + 100, then the estimate with margin 990 + 50"
        );
        assert_eq!(
            usage.cumulative(Some("main")),
            5000,
            "the lower report of sub1 does not change the usage of main"
        );
    }

    #[test]
    fn estimate_near_the_maximum_saturates() {
        let fine = NonZeroU64::new(1000).expect("1000 is not zero");
        assert_eq!(estimate_tokens(u64::MAX, fine, 0), u64::MAX);
        assert_eq!(
            estimate_tokens(u64::MAX, fine, 10),
            u64::MAX,
            "the margin of a value at the maximum saturates"
        );
        let coarse = NonZeroU64::new(1).expect("1 is not zero");
        assert_eq!(
            estimate_tokens(u64::MAX, coarse, 0),
            u64::MAX,
            "a ratio of one thousandth per token saturates the token count"
        );
        assert_eq!(
            estimate_tokens(u64::MAX, NonZeroU64::new(2500).expect("2500 is not zero"), 0),
            7_378_697_629_483_820_646,
            "u64::MAX characters at 2.5 characters per token are exactly that many tokens"
        );
    }

    #[test]
    fn estimate_chars_uses_the_tracker_parameters() {
        let by_thousand = UsageTracker::new(10, 1000).expect("1000 is not zero");
        assert_eq!(
            by_thousand.estimate_chars(1000),
            1100,
            "one token per character at a ratio of 1000, with a 10 percent margin"
        );
        assert_eq!(
            tracker().estimate_chars(1001),
            442,
            "the default tracker gives the same result as estimate_tokens"
        );
    }

    #[test]
    fn set_session_estimate_resets_added_messages() {
        let mut usage = tracker();
        usage.note_added(100);
        usage.set_session_estimate(1000);
        assert_eq!(
            usage.session_tokens(),
            1100,
            "the estimate after a compaction includes the messages added before it: they are not counted twice"
        );
    }
}
