//! Thresholds of the context window: compaction before a request, refusal of a session whose system prompt is too large,
//! and refusal of a user message that would make the next request overflow (`agent-sessions/context-window#FR-2`,
//! `FR-6`, `FR-10`, `AC-2`, `AC-3`, `AC-6`).

use std::num::NonZeroU64;

use super::usage::estimate_tokens;
use super::{ContextError, ContextResult};

/// Default conversion ratio: 2500 thousandths of a character per token, that is 2.5 characters per token.
const DEFAULT_CHARS_PER_TOKEN_MILLI: NonZeroU64 = match NonZeroU64::new(2500) {
    Some(ratio) => ratio,
    // Unreachable: 2500 is not zero. The fallback keeps the constant free of any panic.
    None => NonZeroU64::MIN,
};

/// Thresholds of the context window for one model profile. Sizes are in tokens, except the render cap, which is in
/// characters until it is converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Budget {
    max: u64,
    prompt: u64,
    response_max: u64,
    render_cap_chars: u64,
    chars_per_token_milli: NonZeroU64,
}

impl Budget {
    /// Builds a budget from the maximum context size `max`, the size of the summary request `prompt`, the largest
    /// response `response_max` (`R`) and the render cap of a tool output in characters. The ratio is 2.5 characters per
    /// token until it is changed by [`Budget::with_chars_per_token_milli`].
    #[must_use]
    pub const fn new(max: u64, prompt: u64, response_max: u64, render_cap_chars: u64) -> Self {
        Self {
            max,
            prompt,
            response_max,
            render_cap_chars,
            chars_per_token_milli: DEFAULT_CHARS_PER_TOKEN_MILLI,
        }
    }

    /// Changes the ratio of characters per token, in thousandths. Returns nothing for a ratio of zero.
    #[must_use]
    pub fn with_chars_per_token_milli(self, milli: u64) -> Option<Self> {
        NonZeroU64::new(milli).map(|ratio| Self {
            chars_per_token_milli: ratio,
            ..self
        })
    }

    /// Returns `T`, the render cap converted to tokens, rounded up.
    #[must_use]
    pub fn render_cap_tokens(&self) -> u64 {
        estimate_tokens(self.render_cap_chars, self.chars_per_token_milli, 0)
    }

    /// Tells whether compaction is due before a request whose size is `session` tokens, that is whether
    /// `PROMPT + SESSION + 2R + T > MAX`. An overflowing sum is above `MAX`.
    #[must_use]
    pub fn needs_compaction(&self, session: u64) -> bool {
        self.committed(session).is_none_or(|total| total > self.max)
    }

    /// Accepts a session starting with a system prompt of `system` tokens, or refuses it.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::SystemTooLarge`] when `system` is above the startup bound, or when no system prompt fits
    /// (the bound is then reported as zero).
    pub fn check_startup(&self, system: u64) -> ContextResult<()> {
        match self.startup_limit() {
            Some(limit) if system <= limit => Ok(()),
            limit => Err(ContextError::SystemTooLarge {
                system,
                limit: limit.unwrap_or(0),
            }),
        }
    }

    /// Accepts a user message of `message` tokens next to a system prompt of `system` tokens, or refuses it.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::MessageTooLarge`] when the message would make [`Budget::needs_compaction`] true for the
    /// session `system + message`.
    pub fn check_user_message(&self, system: u64, message: u64) -> ContextResult<()> {
        let fits = system
            .checked_add(message)
            .is_some_and(|session| !self.needs_compaction(session));
        if fits {
            return Ok(());
        }
        Err(ContextError::MessageTooLarge {
            size: message,
            available: self.message_room(system),
        })
    }

    /// Sum `PROMPT + SESSION + 2R + T`, or nothing when it overflows.
    fn committed(&self, session: u64) -> Option<u64> {
        let twice_response = self.response_max.checked_mul(2)?;
        [self.prompt, session, twice_response, self.render_cap_tokens()]
            .into_iter()
            .try_fold(0u64, u64::checked_add)
    }

    /// Largest system prompt accepted at startup, `MAX - PROMPT - 3R - T`, or nothing when no system prompt fits.
    fn startup_limit(&self) -> Option<u64> {
        let response_terms = self.response_max.checked_mul(3)?;
        let incompressible = [self.prompt, response_terms, self.render_cap_tokens()]
            .into_iter()
            .try_fold(0u64, u64::checked_add)?;
        self.max.checked_sub(incompressible)
    }

    /// Room left for a user message after the system prompt, `MAX - PROMPT - 2R - T - SYS`, zero when there is none.
    fn message_room(&self, system: u64) -> u64 {
        let used = self.committed(system).unwrap_or(u64::MAX);
        self.max.saturating_sub(used)
    }
}

#[cfg(test)]
mod tests {
    use super::Budget;
    use crate::context::ContextError;

    /// MAX 10000, PROMPT 500, R 1000, render cap 1000 characters (T = 400).
    fn budget() -> Budget {
        Budget::new(10_000, 500, 1000, 1000)
    }

    #[test]
    fn no_compaction_at_the_boundary() {
        let budget = budget();
        assert_eq!(
            budget.render_cap_tokens(),
            400,
            "T must be 400 for a cap of 1000 characters"
        );
        assert!(
            !budget.needs_compaction(7100),
            "a margin of exactly 0 must not trigger compaction"
        );
        assert!(
            budget.needs_compaction(7101),
            "a negative margin must trigger compaction"
        );
    }

    #[test]
    fn worst_case_always_fits() {
        // SESSION just below the threshold: 10000 - 500 - 2000 - 400 = 7100 is the last size without compaction.
        let budget = budget();
        let session = 7099;
        assert!(!budget.needs_compaction(session));
        let after_response = session + 1000;
        let after_tool_result = after_response + 400;
        // Summary request (PROMPT) plus its response (R) must fit next to the session.
        let summary = 500 + after_tool_result + 1000;
        assert!(
            summary <= 10_000,
            "PROMPT, the session, the response R and the result T must fit in MAX"
        );
    }

    #[test]
    fn startup_refused_when_the_system_prompt_is_too_big() {
        // MAX - PROMPT - 3R - T = 10000 - 500 - 3000 - 500 = 6000, with a render cap of 1250 characters (T = 500).
        let budget = Budget::new(10_000, 500, 1000, 1250);
        assert_eq!(budget.render_cap_tokens(), 500);
        let refused = budget.check_startup(6001);
        assert!(
            matches!(
                refused,
                Err(ContextError::SystemTooLarge {
                    system: 6001,
                    limit: 6000
                })
            ),
            "6001 must fail with SystemTooLarge naming 6001 and 6000, got {refused:?}"
        );
        assert!(
            matches!(budget.check_startup(6000), Ok(())),
            "6000 must be accepted"
        );
    }

    #[test]
    fn no_compaction_loop_at_the_startup_boundary() {
        let budget = Budget::new(10_000, 500, 1000, 1250);
        let system = 6000;
        // The session compacted to SYS + R.
        let compacted = system + 1000;
        assert!(
            !budget.needs_compaction(compacted),
            "a session compacted to SYS + R at the startup bound must not trigger compaction again"
        );
    }

    #[test]
    fn oversized_user_message_refused() {
        // SYS = 5000: the room for a message is 10000 - 500 - 2000 - 400 - 5000 = 2100.
        let budget = budget();
        assert!(
            matches!(budget.check_user_message(5000, 2100), Ok(())),
            "a message filling the room is accepted"
        );
        let refused = budget.check_user_message(5000, 2101);
        assert!(
            matches!(
                refused,
                Err(ContextError::MessageTooLarge {
                    size: 2101,
                    available: 2100
                })
            ),
            "a message one token over the room must fail with MessageTooLarge, got {refused:?}"
        );
    }

    #[test]
    fn render_cap_converts_with_2_5_characters_per_token() {
        assert_eq!(Budget::new(10_000, 500, 1000, 1000).render_cap_tokens(), 400);
        assert_eq!(Budget::new(10_000, 500, 1000, 1001).render_cap_tokens(), 401);
    }

    #[test]
    fn overflow_safe() {
        let huge = Budget::new(u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        assert!(
            huge.needs_compaction(u64::MAX),
            "a sum above u64::MAX must be reported as above MAX, without wrapping"
        );
        assert!(matches!(
            huge.check_startup(u64::MAX),
            Err(ContextError::SystemTooLarge { .. })
        ));
        assert!(
            matches!(huge.check_startup(0), Err(ContextError::SystemTooLarge { .. })),
            "an incompressible sum above MAX refuses any SYS"
        );

        let empty = Budget::new(u64::MAX, 0, 0, 0);
        assert!(!empty.needs_compaction(u64::MAX));
        assert!(matches!(empty.check_startup(u64::MAX), Ok(())));
    }

    #[test]
    fn zero_ratio_refused() {
        assert!(budget().with_chars_per_token_milli(0).is_none());
        assert!(budget().with_chars_per_token_milli(2500).is_some());
    }
}
