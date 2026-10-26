//! Gate allowing at most one active turn per session within a process (`agent-sessions#NFR-2`,
//! `agent-sessions/session#FR-4`). It detects no conflict between processes (`agent-sessions/session#DEC-1`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use super::{SessionError, SessionId, SessionResult};

/// Sessions that currently have an active turn, shared by a gate and its permits.
type ActiveSessions = Arc<Mutex<HashSet<SessionId>>>;

/// Gate allowing at most one active turn per session within the process. Clones share the same state, so the gate can
/// be handed to several threads. Different sessions never exclude each other.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TurnGate {
    active: ActiveSessions,
}

impl TurnGate {
    /// Builds a gate in which no session has an active turn.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Begins a turn for `session`, or fails at once if the session already has an active turn. Nothing waits.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TurnActive`] when the session already has an active turn, and
    /// [`SessionError::Poisoned`] when the internal lock was poisoned.
    pub fn try_begin(&self, session: SessionId) -> SessionResult<TurnPermit> {
        let inserted = self
            .active
            .lock()
            .map_err(|_poisoned| SessionError::Poisoned)?
            .insert(session.clone());
        if !inserted {
            return Err(SessionError::TurnActive(session));
        }
        Ok(TurnPermit {
            session,
            active: Arc::clone(&self.active),
        })
    }

    /// Tells whether the session has an active turn. A poisoned lock is recovered: reading the set cannot leave it
    /// inconsistent, and this query has no error to report.
    #[must_use]
    pub fn is_active(&self, session: &SessionId) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(session)
    }
}

/// Permit of an active turn. The session is released when the permit is dropped, including after an error or a
/// cancellation.
#[derive(Debug)]
#[non_exhaustive]
pub struct TurnPermit {
    session: SessionId,
    active: ActiveSessions,
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        // A poisoned lock is recovered here: removing a session from the set cannot leave it inconsistent, and a
        // permit must release its session even when another thread panicked.
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let _was_active: bool = active.remove(&self.session);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{TurnGate, TurnPermit};
    use crate::session::{SessionError, SessionId};

    fn session(name: &str) -> SessionId {
        SessionId::new(name).expect("a non-empty text makes a session identifier")
    }

    #[test]
    fn second_turn_is_refused() {
        let gate = TurnGate::new();
        let _held = gate
            .try_begin(session("s1"))
            .expect("the first turn of a session begins");
        let error = gate
            .try_begin(session("s1"))
            .expect_err("a second turn of the same session must be refused");
        assert!(
            matches!(&error, SessionError::TurnActive(id) if *id == session("s1")),
            "the refusal must be TurnActive carrying the session identifier, got {error}"
        );
    }

    #[test]
    fn other_sessions_are_independent() {
        let gate = TurnGate::new();
        let _held = gate
            .try_begin(session("s1"))
            .expect("the first turn of s1 begins");
        let _other = gate
            .try_begin(session("s2"))
            .expect("a turn of another session must begin while s1 is active");
    }

    #[test]
    fn dropping_releases() {
        let gate = TurnGate::new();
        let permit: TurnPermit = gate
            .try_begin(session("s1"))
            .expect("the first turn of s1 begins");
        let was_active = gate.is_active(&session("s1"));
        drop(permit);
        let is_active_after = gate.is_active(&session("s1"));
        assert!(was_active, "the session must be active while its permit is held");
        assert!(
            !is_active_after,
            "the session must be released once its permit is dropped"
        );
        let _next = gate
            .try_begin(session("s1"))
            .expect("after the permit is dropped a new turn of the session begins");
    }

    #[test]
    fn concurrent_attempts() {
        const THREADS: usize = 8;
        let gate = TurnGate::new();
        let barrier = Arc::new(Barrier::new(THREADS));
        // Every thread must be spawned before the first join: the barrier waits for all of them.
        #[expect(
            clippy::needless_collect,
            reason = "the threads must all be spawned before any is joined, or the barrier never opens"
        )]
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let gate = gate.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let _leader = barrier.wait().is_leader();
                    gate.try_begin(session("s1")).ok()
                })
            })
            .collect();
        // The permits are kept alive until the count is taken, so that no winner releases its session early.
        let permits: Vec<TurnPermit> = handles
            .into_iter()
            .filter_map(|handle| handle.join().expect("the attempt thread does not panic"))
            .collect();
        assert_eq!(
            permits.len(),
            1,
            "among eight threads attempting the same session at once, exactly one must obtain a permit"
        );
    }
}
