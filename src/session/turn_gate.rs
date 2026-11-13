//! Gate allowing at most one active turn per session (`agent-sessions#NFR-2`, `agent-sessions/session#FR-4`).
//! The gate is a trait the host can replace (`agent-sessions/session#FR-17`, `agent-sessions#DEC-14`); the in-memory
//! implementation below is the default and detects no conflict between processes (`agent-sessions/session#DEC-1`).

use std::collections::HashSet;
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use super::{CancelHandle, SessionError, SessionId, SessionResult};

/// Sessions that currently have an active turn, shared by a gate and its permits.
type ActiveSessions = Arc<Mutex<HashSet<SessionId>>>;

/// The boxed future of [`TurnLock::try_begin`]: a permit, or the reason none was granted.
type TurnAttempt = Pin<Box<dyn Future<Output = SessionResult<TurnPermit>> + Send>>;

/// Gate granting at most one active turn per session, as an object through `Arc<dyn TurnLock>`
/// (`agent-sessions/session#FR-17`).
///
/// The host implements it to share the exclusion across processes (`agent-sessions#DEC-14`); [`TurnGate`] is the
/// in-memory default. The request is asynchronous, as a boxed future so that the trait stays usable as a dynamic
/// object, like [`crate::NativeTool`]. It never waits on an active turn: the `TurnActive` refusal comes back at once.
pub trait TurnLock: Send + Sync {
    /// Requests the permit of a turn for `session`. A gate refuses a session that already has an active turn, and
    /// may fail for a reason of its own.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TurnActive`] when the session already has an active turn, and
    /// [`SessionError::TurnLock`] when the gate fails for a reason of its own.
    fn try_begin(&self, session: SessionId) -> TurnAttempt;
}

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
    /// The synchronous inherent call kept beside [`TurnLock::try_begin`], which this type also implements: the
    /// default gate behaves exactly as before the trait.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::TurnActive`] when the session already has an active turn, and
    /// [`SessionError::Poisoned`] when the internal lock was poisoned.
    #[expect(
        clippy::same_name_method,
        reason = "turn_gate.sdd keeps the synchronous inherent call beside the asynchronous trait implementation"
    )]
    pub fn try_begin(&self, session: SessionId) -> SessionResult<TurnPermit> {
        let inserted = self
            .active
            .lock()
            .map_err(|_poisoned| SessionError::Poisoned)?
            .insert(session.clone());
        if !inserted {
            return Err(SessionError::TurnActive(session));
        }
        let active = Arc::clone(&self.active);
        let released = session.clone();
        Ok(TurnPermit::new(
            session,
            // A poisoned lock is recovered here: removing a session from the set cannot leave it inconsistent, and a
            // permit must release its session even when another thread panicked.
            move || {
                let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
                let _was_active: bool = active.remove(&released);
            },
            // The in-memory gate has no lease to lose: it never signals a loss (`agent-sessions/session#FR-17`).
            CancelHandle::new(),
        ))
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

impl TurnLock for TurnGate {
    fn try_begin(&self, session: SessionId) -> TurnAttempt {
        let outcome = Self::try_begin(self, session);
        Box::pin(std::future::ready(outcome))
    }
}

/// Permit of an active turn.
///
/// The release closure runs exactly once when the permit is dropped, including after an error or a cancellation; a
/// gate that releases over a network launches a task from the closure or lets the lease expire, it never blocks. The
/// loss signal is triggered by the gate implementation that built the permit, never by this crate
/// (`agent-sessions/session#FR-17`).
#[non_exhaustive]
pub struct TurnPermit {
    session: SessionId,
    release: Option<Box<dyn FnOnce() + Send>>,
    lost: CancelHandle,
}

impl TurnPermit {
    /// Builds the permit of `session` for a gate implementation: `release` runs once at destruction, and `lost` is
    /// the lease-loss signal the implementation keeps triggering itself.
    pub fn new<F>(session: SessionId, release: F, lost: CancelHandle) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self {
            session,
            release: Some(Box::new(release)),
            lost,
        }
    }

    /// The session this permit was granted for.
    #[must_use]
    pub const fn session(&self) -> &SessionId {
        &self.session
    }

    /// The lease-loss signal of the permit. Clones share the same state; only the gate implementation that built
    /// the permit triggers it.
    #[must_use]
    pub fn lost(&self) -> CancelHandle {
        self.lost.clone()
    }
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        let release = self.release.take();
        if let Some(release) = release {
            release();
        }
    }
}

impl fmt::Debug for TurnPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The closure and the loss signal carry nothing to show: only the session identifier is displayed.
        f.debug_struct("TurnPermit")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex, PoisonError};
    use std::thread;

    use super::{ActiveSessions, TurnAttempt, TurnGate, TurnLock, TurnPermit};
    use crate::session::{CancelHandle, SessionError, SessionId};

    fn session(name: &str) -> SessionId {
        SessionId::new(name).expect("a non-empty text makes a session identifier")
    }

    // --- test doubles of the Scenarios, standing for host gates ---

    /// The reason a failing double reports, checked verbatim by the failure Scenario.
    const HOST_FAILURE: &str = "the test gate lost its lease";

    /// A gate double sharing its active set with its siblings, like two processes sharing one lock table, and
    /// holding the loss signal from which the signals of its permits are derived.
    struct TestGate {
        active: ActiveSessions,
        lost: CancelHandle,
    }

    impl TestGate {
        /// A double sharing `active` and holding a fresh loss signal.
        fn new(active: ActiveSessions) -> Self {
            Self {
                active,
                lost: CancelHandle::new(),
            }
        }

        /// The loss signal held by this implementation: cancelling it signals every permit it granted.
        fn signal(&self) -> CancelHandle {
            self.lost.clone()
        }
    }

    impl TurnLock for TestGate {
        fn try_begin(&self, session: SessionId) -> TurnAttempt {
            let inserted = self
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(session.clone());
            if !inserted {
                return Box::pin(std::future::ready(Err(SessionError::TurnActive(session))));
            }
            let active = Arc::clone(&self.active);
            let released = session.clone();
            let permit = TurnPermit::new(
                session,
                move || {
                    let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
                    let _released: bool = active.remove(&released);
                },
                self.lost.clone(),
            );
            Box::pin(std::future::ready(Ok(permit)))
        }
    }

    /// A gate double that fails the way a host gate may: a typed error of its own, never a permit.
    struct FailingGate;

    impl TurnLock for FailingGate {
        fn try_begin(&self, _session: SessionId) -> TurnAttempt {
            Box::pin(std::future::ready(Err(SessionError::TurnLock(
                HOST_FAILURE.to_owned(),
            ))))
        }
    }

    fn shared_state() -> ActiveSessions {
        Arc::new(Mutex::new(HashSet::new()))
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

    // Scenario (turn_gate.sdd) : la porte en mémoire s'utilise derrière le trait
    #[tokio::test(start_paused = true)]
    async fn the_memory_gate_is_used_behind_the_trait() {
        let gate: Arc<dyn TurnLock> = Arc::new(TurnGate::new());
        let started = tokio::time::Instant::now();
        let held = gate
            .try_begin(session("s1"))
            .await
            .expect("the first turn of a session begins behind the trait");
        assert_eq!(
            held.session(),
            &session("s1"),
            "the permit carries the session it was granted for"
        );
        let error = gate
            .try_begin(session("s1"))
            .await
            .expect_err("a second turn of the same session must be refused behind the trait");
        assert!(
            matches!(&error, SessionError::TurnActive(id) if *id == session("s1")),
            "the refusal must be TurnActive carrying the session identifier, got {error}"
        );
        assert_eq!(
            started.elapsed(),
            std::time::Duration::ZERO,
            "neither attempt may wait: the in-memory gate refuses at once"
        );
    }

    // Must (turn_gate.sdd) : le trait est Send et Sync, pour un `Arc<dyn TurnLock>` partagé par la boucle de tour
    #[test]
    fn the_trait_can_be_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn TurnLock>();
        assert_send_sync::<Arc<dyn TurnLock>>();
    }

    // Scenario (turn_gate.sdd) : deux portes partagées refusent un second tour entre elles
    #[tokio::test]
    async fn two_shared_gates_refuse_a_second_turn_between_them() {
        let state = shared_state();
        let first: Arc<dyn TurnLock> = Arc::new(TestGate::new(Arc::clone(&state)));
        let second: Arc<dyn TurnLock> = Arc::new(TestGate::new(state));
        let held = first
            .try_begin(session("s1"))
            .await
            .expect("the first gate grants s1");
        let error = second
            .try_begin(session("s1"))
            .await
            .expect_err("the second gate must refuse s1 held by the first");
        assert!(
            matches!(&error, SessionError::TurnActive(id) if *id == session("s1")),
            "the refusal between the two gates must be TurnActive, got {error}"
        );
        let _other = second
            .try_begin(session("s2"))
            .await
            .expect("another session must be granted while s1 is held by the other gate");
        drop(held);
        let _again = second
            .try_begin(session("s1"))
            .await
            .expect("once the first gate's permit is dropped, the second gate grants s1");
    }

    // Scenario (turn_gate.sdd) : la fermeture de relâchement est appelée une fois
    #[test]
    fn the_release_closure_runs_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let permit = TurnPermit::new(
            session("s1"),
            move || {
                let _calls = counter.fetch_add(1, Ordering::SeqCst);
            },
            CancelHandle::new(),
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the release closure must not run while the permit is held"
        );
        let panicking = thread::spawn(|| panic!("the test thread panics on purpose"));
        let joined = panicking.join();
        assert!(
            joined.is_err(),
            "the spawned thread must panic for this Scenario to cover a release after a panic"
        );
        drop(permit);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the release closure must run exactly once at destruction, including after a panic of another thread"
        );
    }

    // Scenario (turn_gate.sdd) : le signal de perte est observable et partagé
    #[tokio::test]
    async fn the_lost_signal_is_observable_and_shared() {
        let gate = Arc::new(TestGate::new(shared_state()));
        let held = gate
            .try_begin(session("s1"))
            .await
            .expect("the test gate grants s1");
        let signal = held.lost();
        assert!(
            !signal.is_cancelled(),
            "the loss signal must not be cancelled while the lease holds"
        );
        gate.signal().cancel();
        assert!(
            held.lost().is_cancelled(),
            "the implementation cancelling its signal must make `lost` report the loss"
        );
        assert!(
            signal.clone().is_cancelled(),
            "clones of the loss signal must share the same state"
        );
        let still_held = gate
            .try_begin(session("s1"))
            .await
            .expect_err("a signalled loss must not release the permit by itself");
        assert!(
            matches!(&still_held, SessionError::TurnActive(id) if *id == session("s1")),
            "the session must stay held while its permit is alive, got {still_held}"
        );
    }

    // Scenario (turn_gate.sdd) : la porte en mémoire ne déclenche jamais la perte
    #[test]
    fn the_memory_gate_never_signals_loss() {
        let gate = TurnGate::new();
        let permit = gate
            .try_begin(session("s1"))
            .expect("the in-memory gate grants s1");
        let signal = permit.lost();
        assert!(
            !signal.is_cancelled(),
            "the signal must not be cancelled while the permit is taken"
        );
        assert!(
            !permit.lost().is_cancelled(),
            "the signal must not be cancelled while the permit is held"
        );
        drop(permit);
        assert!(
            !signal.is_cancelled(),
            "dropping the permit must not signal a loss either"
        );
    }

    // Scenario (turn_gate.sdd) : l'échec d'une porte de l'hôte est une erreur typée
    #[tokio::test]
    async fn a_failing_host_gate_returns_a_typed_error() {
        let gate: Arc<dyn TurnLock> = Arc::new(FailingGate);
        let error = gate
            .try_begin(session("s1"))
            .await
            .expect_err("the host gate must fail with its own reason");
        assert!(
            matches!(&error, SessionError::TurnLock(reason) if reason == HOST_FAILURE),
            "the typed TurnLock error must be returned as it was, without a permit, got {error}"
        );
    }
}
