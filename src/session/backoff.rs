//! Retry schedule after a transient inference error: 1 s, doubling at each retry, capped at five minutes, with no limit on
//! the number of retries (`agent-sessions#FR-6`, `agent-sessions/session#FR-7`). It computes durations only: the wait
//! itself is the cancellable sleep of the cancel module.

use std::time::Duration;

/// Delays in seconds of the first retries, each the double of the previous one.
const DOUBLING_SECS: [u64; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];

/// Delay reached after the doubling and kept for every later retry.
const CAP: Duration = Duration::from_secs(300);

/// Retry schedule. Each call to [`Backoff::next_delay`] returns the next delay and advances the schedule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Backoff {
    attempts: u32,
}

impl Backoff {
    /// Builds a schedule standing at its first delay of one second.
    #[must_use]
    pub const fn new() -> Self {
        Self { attempts: 0 }
    }

    /// Returns the delay to wait before the next retry, then advances the schedule.
    pub fn next_delay(&mut self) -> Duration {
        let index = usize::try_from(self.attempts).unwrap_or(usize::MAX);
        let delay = DOUBLING_SECS.get(index).copied().map_or(CAP, Duration::from_secs);
        self.attempts = self.attempts.saturating_add(1);
        delay
    }

    /// Returns the number of delays already returned since the last reset.
    #[must_use]
    pub const fn attempts(self) -> u32 {
        self.attempts
    }

    /// Restarts the schedule at one second, to be called after a successful inference call.
    pub const fn reset(&mut self) {
        self.attempts = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Backoff;

    #[test]
    fn delays_double_up_to_the_cap() {
        let mut backoff = Backoff::new();
        let delays: Vec<Duration> = (0..11).map(|_| backoff.next_delay()).collect();
        let expected = [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300].map(Duration::from_secs);
        assert_eq!(
            delays, expected,
            "the delays must double from one second up to the five-minute cap, then stay at the cap"
        );
    }

    #[test]
    fn reset_restarts_the_schedule() {
        let mut backoff = Backoff::new();
        let produced: Vec<Duration> = (0..5).map(|_| backoff.next_delay()).collect();
        assert_eq!(
            produced.len(),
            5,
            "the test must produce five delays before the reset"
        );
        assert_eq!(
            backoff.attempts(),
            5,
            "attempts must count the delays already returned"
        );
        backoff.reset();
        assert_eq!(
            backoff.attempts(),
            0,
            "reset must bring the attempt count back to zero"
        );
        assert_eq!(
            backoff.next_delay(),
            Duration::from_secs(1),
            "the first delay after a reset must be one second"
        );
    }

    #[test]
    fn very_many_attempts_never_overflow() {
        let mut backoff = Backoff::new();
        let mut last = Duration::ZERO;
        for _ in 0..100_000 {
            last = backoff.next_delay();
        }
        assert_eq!(
            last,
            Duration::from_secs(300),
            "after one hundred thousand attempts the delay must stay at the five-minute cap"
        );
    }
}
