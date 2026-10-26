//! Cancellation handle handed to the host when a turn or a run starts, and the cancellable wait used during the backoff
//! (`agent-sessions#FR-4`, `agent-sessions/session#FR-5`, `agent-sessions/session#FR-7`).

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Outcome of [`CancelHandle::sleep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SleepOutcome {
    /// The whole duration elapsed before any cancellation.
    Elapsed,
    /// The handle was cancelled before the duration elapsed, or was already cancelled.
    Cancelled,
}

/// Cancellation handle shared between the host and the crate.
///
/// Clones share the same state: cancelling one cancels all of them. A child made by [`CancelHandle::child`] is
/// cancelled with its parent, but cancelling a child leaves its parent untouched.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CancelHandle {
    token: CancellationToken,
}

impl CancelHandle {
    /// Builds a handle that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a child handle, cancelled whenever this handle is cancelled.
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
        }
    }

    /// Cancels the handle and everything derived from it. Idempotent, immediate and non-blocking.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Tells whether the handle has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Completes when the handle is cancelled, at once if it already is.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Waits for `duration` unless the handle is cancelled first.
    ///
    /// Returns [`SleepOutcome::Cancelled`] as soon as the handle is cancelled, without waiting for the end of the
    /// duration, and at once when it was already cancelled.
    pub async fn sleep(&self, duration: Duration) -> SleepOutcome {
        tokio::select! {
            biased;
            () = self.token.cancelled() => SleepOutcome::Cancelled,
            () = tokio::time::sleep(duration) => SleepOutcome::Elapsed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CancelHandle, SleepOutcome};

    #[tokio::test(start_paused = true)]
    async fn sleep_elapses() {
        let handle = CancelHandle::new();
        let started = tokio::time::Instant::now();
        let outcome = handle.sleep(Duration::from_secs(2)).await;
        assert_eq!(
            outcome,
            SleepOutcome::Elapsed,
            "a sleep that is not cancelled must return Elapsed once its duration has passed"
        );
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "an Elapsed sleep must have waited the whole 2 seconds of paused time, waited {:?}",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_interrupts_sleep() {
        let handle = CancelHandle::new();
        let sleeper = handle.clone();
        let task = tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let outcome = sleeper.sleep(Duration::from_secs(300)).await;
            (outcome, started.elapsed())
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        handle.cancel();
        let (outcome, waited) = task.await.expect("the sleeping task does not panic");
        assert_eq!(
            outcome,
            SleepOutcome::Cancelled,
            "cancelling during a sleep must return Cancelled"
        );
        assert!(
            waited >= Duration::from_secs(1) && waited < Duration::from_secs(2),
            "sleep must return about one second after the cancel, not at once and not after 300 seconds, waited {waited:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn clones_and_children_share_cancellation() {
        let handle = CancelHandle::new();
        let clone = handle.clone();
        let child = handle.child();
        clone.cancel();
        assert!(
            handle.is_cancelled(),
            "the handle must report the cancel made through its clone"
        );
        assert!(clone.is_cancelled(), "the clone must report its own cancel");
        assert!(
            child.is_cancelled(),
            "the child must be cancelled with its parent"
        );

        let parent = CancelHandle::new();
        let only_child = parent.child();
        only_child.cancel();
        assert!(only_child.is_cancelled(), "the child must report its own cancel");
        assert!(
            !parent.is_cancelled(),
            "cancelling only the child must leave the parent not cancelled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_is_idempotent() {
        let handle = CancelHandle::new();
        handle.cancel();
        handle.cancel();
        assert!(
            handle.is_cancelled(),
            "a handle cancelled twice must stay cancelled"
        );
        let completed = tokio::time::timeout(Duration::from_secs(1), handle.cancelled()).await;
        assert!(
            completed.is_ok(),
            "awaiting cancelled on an already cancelled handle must complete at once"
        );
        let started = tokio::time::Instant::now();
        let outcome = handle.sleep(Duration::from_secs(300)).await;
        assert_eq!(
            outcome,
            SleepOutcome::Cancelled,
            "a sleep on an already cancelled handle must return Cancelled without waiting"
        );
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "a sleep on an already cancelled handle must not let any paused time elapse"
        );
    }
}
