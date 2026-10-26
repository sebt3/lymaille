//! Injected clock: every date or duration measured by the crate goes through [`Clock`], never through
//! `SystemTime::now` or `Instant::now` directly (`agent-sessions#NFR-1`).

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC. Serializes as a bare number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct Timestamp {
    millis: u64,
}

impl Timestamp {
    /// Builds a timestamp from milliseconds since the Unix epoch.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    /// Returns the number of milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.millis
    }
}

/// Source of the current instant, injected into the crate and shareable between threads.
pub trait Clock: Send + Sync {
    /// Returns the current instant.
    #[must_use]
    fn now(&self) -> Timestamp;
}

/// Clock reading the host system time. It is the only place where the crate reads the system clock.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct SystemClock;

impl SystemClock {
    /// Builds the system clock.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        read_system_time()
    }
}

/// Reads the host system time. This is the only place where the crate reads the system clock (`NFR-1`).
/// An instant before the Unix epoch reads as `Timestamp(0)`.
#[expect(
    clippy::disallowed_methods,
    reason = "NFR-1: SystemClock is the single injected point that reads the host clock"
)]
fn read_system_time() -> Timestamp {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| Timestamp::from_millis(0),
        |elapsed| Timestamp::from_millis(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)),
    )
}

/// Deterministic clock: it reads the value it was given and moves only through [`ManualClock::advance`] or
/// [`ManualClock::set`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ManualClock {
    current: Mutex<Timestamp>,
}

impl ManualClock {
    /// Builds a manual clock standing at `start`.
    #[must_use]
    pub const fn new(start: Timestamp) -> Self {
        Self {
            current: Mutex::new(start),
        }
    }

    /// Moves the clock forward by `by`, saturating at the maximum timestamp.
    pub fn advance(&self, by: Duration) {
        let step = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        let mut current = self.lock();
        *current = Timestamp::from_millis(current.as_millis().saturating_add(step));
    }

    /// Replaces the current value of the clock.
    pub fn set(&self, at: Timestamp) {
        *self.lock() = at;
    }

    /// Locks the stored value. A poisoned lock is recovered: the stored value is a plain `Copy` timestamp, so it
    /// cannot be left half-written.
    fn lock(&self) -> MutexGuard<'_, Timestamp> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.lock()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Clock, ManualClock, SystemClock, Timestamp};

    #[test]
    fn manual_clock_returns_the_value_it_was_given() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        assert_eq!(
            clock.now(),
            Timestamp::from_millis(1000),
            "a manual clock must read back the value it was created with"
        );
    }

    #[test]
    fn manual_clock_advances() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        clock.advance(Duration::from_millis(500));
        assert_eq!(
            clock.now(),
            Timestamp::from_millis(1500),
            "advancing by 500 ms from 1000 ms must read 1500 ms"
        );
    }

    #[test]
    fn manual_clock_advance_saturates_instead_of_overflowing() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        clock.advance(Duration::from_millis(u64::MAX));
        assert_eq!(
            clock.now(),
            Timestamp::from_millis(u64::MAX),
            "advancing past the maximum must saturate at the maximum"
        );
    }

    #[test]
    fn manual_clock_set_replaces_the_value() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        clock.set(Timestamp::from_millis(42));
        assert_eq!(
            clock.now(),
            Timestamp::from_millis(42),
            "set must replace the current value"
        );
    }

    #[test]
    fn system_clock_is_after_2020() {
        let now = SystemClock::new().now();
        assert!(
            now > Timestamp::from_millis(1_577_836_800_000),
            "the system clock must read a time after 2020-01-01 UTC, got {}",
            now.as_millis()
        );
    }

    #[test]
    fn timestamp_serializes_as_plain_number() {
        let timestamp = Timestamp::from_millis(42);
        let json = serde_json::to_string(&timestamp).expect("a timestamp serializes");
        assert_eq!(json, "42", "a timestamp must serialize as a bare number");
        let back: Timestamp = serde_json::from_str(&json).expect("a bare number deserializes");
        assert_eq!(
            back, timestamp,
            "the serialized timestamp must deserialize to the same value"
        );
    }

    #[test]
    fn timestamp_round_trips_its_millis() {
        assert_eq!(
            Timestamp::from_millis(7).as_millis(),
            7,
            "as_millis must return the value given to from_millis"
        );
    }

    #[test]
    fn clock_can_be_shared_behind_an_arc_between_threads() {
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(7)));
        let other = Arc::clone(&clock);
        let reader = std::thread::spawn(move || other.now());
        let read = reader.join().expect("the reader thread does not panic");
        assert_eq!(
            read,
            Timestamp::from_millis(7),
            "a clock shared with another thread must read the same value"
        );
    }
}
