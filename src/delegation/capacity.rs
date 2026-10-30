//! Limit of simultaneous requests towards an inference engine, and the capacity of sub-agents announced to the main
//! agent (`agent-sessions/delegation#FR-11`, `FR-13`, `AC-5`, `AC-7`).
//!
//! The limiter counts its places through a host [`RequestCounter`], local to the process by default.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

/// Host-side count of requests in flight towards an inference engine.
///
/// The host implements it; the library calls it from any task, hence `Send + Sync`. A host may share the count between
/// processes (`agent-sessions/delegation#FR-13`).
///
/// The methods are called while the limiter holds its internal queue lock: an implementation must not call back into
/// the limiter.
pub trait RequestCounter: Send + Sync {
    /// Number of requests currently in flight towards the engine, across processes when the count is shared.
    fn in_flight(&self) -> u64;

    /// Called when a request starts.
    fn acquired(&self);

    /// Called when a request ends.
    fn released(&self);
}

/// In-memory count of the requests of this process (`agent-sessions/delegation#FR-13`, the default).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct LocalCounter {
    in_flight: AtomicU64,
}

impl LocalCounter {
    /// Builds a counter in which no request is in flight.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `next` to the count. The closure always answers, so the update cannot fail and the previous value is not
    /// needed.
    fn update(&self, next: impl Fn(u64) -> u64) {
        let _previous = self
            .in_flight
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| Some(next(count)));
    }
}

impl RequestCounter for LocalCounter {
    fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::SeqCst)
    }

    fn acquired(&self) {
        self.update(|count| count.saturating_add(1));
    }

    fn released(&self) {
        self.update(|count| count.saturating_sub(1));
    }
}

/// State shared by a limiter and its permits: the limit, the host counter and the FIFO queue of waiting requests.
struct Shared {
    limit: NonZeroU64,
    counter: Arc<dyn RequestCounter>,
    queue: Mutex<Queue>,
}

/// Waiting requests in arrival order. `next_id` gives each wait an identifier to find it again when it is abandoned.
#[derive(Default)]
struct Queue {
    waiters: VecDeque<Waiter>,
    next_id: u64,
}

/// One waiting request: its identifier and the channel that grants it a place.
struct Waiter {
    id: u64,
    grant: oneshot::Sender<()>,
}

impl Shared {
    /// Locks the queue. A poisoned lock is recovered: the queue is only ever updated by whole operations, and a limiter
    /// has no error to report.
    fn lock_queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gives back one place. If a waiter is still queued, the place passes to the first one, and the counter keeps
    /// counting it. Otherwise the counter is told that the request ended.
    fn release(&self) {
        let mut queue = self.lock_queue();
        while let Some(waiter) = queue.waiters.pop_front() {
            if waiter.grant.send(()).is_ok() {
                return;
            }
        }
        self.counter.released();
    }
}

/// Permit of one place towards the engine. The place is given back when the permit is dropped.
#[must_use = "the place towards the engine is held until the permit is dropped"]
#[non_exhaustive]
pub struct Permit {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit").finish_non_exhaustive()
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.shared.release();
    }
}

/// A request waiting for its place. Dropped before its grant is consumed (the `acquire` future was abandoned), it
/// leaves the queue; if the grant had already arrived, the place is given to the next waiter, so no place is lost.
struct Waiting {
    shared: Arc<Shared>,
    id: u64,
    grant: oneshot::Receiver<()>,
    done: bool,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // The grant is sent under the queue lock, and this receiver is alive until this drop ends. So a waiter no
        // longer queued has been granted, and its place is ours to give back.
        let mut queue = self.shared.lock_queue();
        let position = queue.waiters.iter().position(|waiter| waiter.id == self.id);
        let removed = position.and_then(|index| queue.waiters.remove(index));
        drop(queue);
        if removed.is_none() {
            self.shared.release();
        }
    }
}

/// Limiter of the simultaneous requests towards one inference engine, for one model profile.
#[non_exhaustive]
pub struct InferenceLimiter {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for InferenceLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceLimiter")
            .field("limit", &self.shared.limit)
            .finish_non_exhaustive()
    }
}

impl InferenceLimiter {
    /// Builds a limiter of `limit` simultaneous requests, counting in-flight requests with `counter`.
    #[must_use]
    pub fn new(limit: NonZeroU64, counter: Arc<dyn RequestCounter>) -> Self {
        Self {
            shared: Arc::new(Shared {
                limit,
                counter,
                queue: Mutex::new(Queue::default()),
            }),
        }
    }

    /// Waits for a place towards the engine, in arrival order, and returns its permit. Dropping the future abandons the
    /// wait without losing a place.
    pub async fn acquire(&self) -> Permit {
        loop {
            let waiting = match self.take_or_enqueue() {
                Ok(permit) => return permit,
                Err(waiting) => waiting,
            };
            if let Some(permit) = self.wait(waiting).await {
                return permit;
            }
        }
    }

    /// Waits for the grant of a queued request. `None` means the grant cannot come: the caller queues again.
    async fn wait(&self, mut waiting: Waiting) -> Option<Permit> {
        let outcome = (&mut waiting.grant).await;
        // The grant is consumed, or cannot come: either way the guard must not give a place back.
        waiting.done = true;
        outcome.is_ok().then(|| Permit {
            shared: Arc::clone(&self.shared),
        })
    }

    /// Takes a free place at once when nobody waits ahead, or queues the request behind the waiting ones.
    fn take_or_enqueue(&self) -> Result<Permit, Waiting> {
        let mut queue = self.shared.lock_queue();
        if queue.waiters.is_empty() && self.shared.counter.in_flight() < self.shared.limit.get() {
            self.shared.counter.acquired();
            return Ok(Permit {
                shared: Arc::clone(&self.shared),
            });
        }
        let id = queue.next_id;
        queue.next_id = queue.next_id.saturating_add(1);
        let (grant, received) = oneshot::channel();
        queue.waiters.push_back(Waiter { id, grant });
        drop(queue);
        Err(Waiting {
            shared: Arc::clone(&self.shared),
            id,
            grant: received,
            done: false,
        })
    }

    /// Gives the free places to the waiting requests, in arrival order. The host calls it when the shared count fell
    /// outside the limiter, as when another process ends its requests: without a call, a waiter is woken only by a local
    /// release. Each place granted is counted as a new request. It blocks nothing, sets no timer and starts no task.
    pub fn tick(&self) {
        self.grant_free_places(&mut self.shared.lock_queue());
    }

    /// Grants the places that the count leaves free to the first waiters of `queue`, the queue lock being held by the
    /// caller. The count is read again after each grant, which it counts.
    fn grant_free_places(&self, queue: &mut Queue) {
        while self.shared.counter.in_flight() < self.shared.limit.get() {
            let Some(waiter) = queue.waiters.pop_front() else {
                break;
            };
            // A waiter whose receiver is gone cannot take the place: the next one is tried.
            if waiter.grant.send(()).is_ok() {
                self.shared.counter.acquired();
            }
        }
    }

    /// Number of free places seen by this limiter: the limit minus the in-flight requests, never below zero.
    #[must_use]
    pub fn available(&self) -> u64 {
        self.shared
            .limit
            .get()
            .saturating_sub(self.shared.counter.in_flight())
    }

    /// Number of sub-agents that can run in parallel now: the limit minus the in-flight requests, minus one for the
    /// session of the main agent, never below zero.
    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.available().saturating_sub(1)
    }
}

/// Capacity of sub-agents announced to the main agent at the start of its session.
///
/// It is the limit of the profile minus the requests already consumed at that moment, minus one for the main agent,
/// never below zero. It is a snapshot (`agent-sessions/delegation#FR-13`).
#[must_use]
pub const fn announced_capacity(limit: NonZeroU64, consumed: u64) -> u64 {
    limit.get().saturating_sub(consumed).saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::num::NonZeroU64;
    use std::pin::{Pin, pin};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::thread;

    use super::{InferenceLimiter, LocalCounter, Permit, RequestCounter, announced_capacity};

    /// Polls a future once with a no-op waker. Waking is not needed: the tests poll again after each release.
    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        let mut context = Context::from_waker(Waker::noop());
        future.poll(&mut context)
    }

    /// Takes a place that must be free at once.
    fn acquire_now(limiter: &InferenceLimiter) -> Permit {
        let mut future = pin!(limiter.acquire());
        let Poll::Ready(permit) = poll_once(future.as_mut()) else {
            panic!("a place must be free at once");
        };
        permit
    }

    fn limit(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("the test limits are not zero")
    }

    /// Host counter reporting requests of another process, as a shared count does.
    struct OtherProcesses(u64);

    impl RequestCounter for OtherProcesses {
        fn in_flight(&self) -> u64 {
            self.0
        }

        fn acquired(&self) {}

        fn released(&self) {}
    }

    // Scenario: capacité annoncée
    #[test]
    fn announced_capacity_is_limit_minus_consumed_minus_one() {
        assert_eq!(
            announced_capacity(limit(8), 2),
            5,
            "a limit of 8 with 2 consumed requests announces 8 - 2 - 1 = 5 sub-agents"
        );
    }

    // Scenario: la neuvième requête est mise en file
    #[test]
    fn ninth_request_waits_then_gets_a_released_place() {
        let limiter = InferenceLimiter::new(limit(8), Arc::new(LocalCounter::new()));
        let mut held: Vec<Permit> = (0..8).map(|_| acquire_now(&limiter)).collect();
        assert_eq!(
            limiter.available(),
            0,
            "no place is free while eight permits are held"
        );
        let mut ninth = pin!(limiter.acquire());
        assert!(
            poll_once(ninth.as_mut()).is_pending(),
            "the ninth request must wait while eight permits are held"
        );
        drop(held.pop());
        assert!(
            poll_once(ninth.as_mut()).is_ready(),
            "the ninth request must obtain the place of the released permit"
        );
    }

    // Scenario: capacité jamais négative
    #[test]
    fn capacity_is_never_negative() {
        let counter = LocalCounter::new();
        for _ in 0..5 {
            counter.acquired();
        }
        let limiter = InferenceLimiter::new(limit(2), Arc::new(counter));
        assert_eq!(
            limiter.capacity(),
            0,
            "five requests in flight against a limit of 2 leave no sub-agent capacity, not a negative one"
        );
    }

    // Scenario: décompte partagé de l'hôte
    #[test]
    fn shared_count_of_the_host_is_taken_into_account() {
        let limiter = InferenceLimiter::new(limit(8), Arc::new(OtherProcesses(3)));
        assert_eq!(
            limiter.capacity(),
            4,
            "3 requests of another process reduce the capacity: 8 - 3 - 1 = 4"
        );
    }

    // Scenario: abandon d'une attente
    #[test]
    fn abandoned_wait_loses_no_place() {
        let limiter = InferenceLimiter::new(NonZeroU64::MIN, Arc::new(LocalCounter::new()));
        let held = acquire_now(&limiter);
        {
            let mut abandoned = Box::pin(limiter.acquire());
            assert!(
                poll_once(abandoned.as_mut()).is_pending(),
                "the request must wait while the only place is held"
            );
        }
        drop(held);
        let mut next = pin!(limiter.acquire());
        assert!(
            poll_once(next.as_mut()).is_ready(),
            "after the abandoned wait is dropped, another request obtains the released place"
        );
    }

    // Scenario: abandon d'une attente
    #[test]
    fn grant_unused_by_an_abandoned_wait_goes_to_the_next_waiter() {
        let limiter = InferenceLimiter::new(NonZeroU64::MIN, Arc::new(LocalCounter::new()));
        let held = acquire_now(&limiter);
        let mut first = Box::pin(limiter.acquire());
        let mut second = pin!(limiter.acquire());
        assert!(
            poll_once(first.as_mut()).is_pending(),
            "the first request must wait while the only place is held"
        );
        assert!(
            poll_once(second.as_mut()).is_pending(),
            "the second request must wait behind the first"
        );
        drop(held);
        drop(first);
        assert!(
            poll_once(second.as_mut()).is_ready(),
            "a place granted to a dropped wait must pass to the next waiter, not be lost"
        );
    }

    // Scenario: ordre d'arrivée
    #[test]
    fn waiters_are_served_in_arrival_order() {
        let limiter = InferenceLimiter::new(limit(3), Arc::new(LocalCounter::new()));
        let mut held: Vec<Permit> = (0..3).map(|_| acquire_now(&limiter)).collect();
        let mut first = pin!(limiter.acquire());
        let mut second = pin!(limiter.acquire());
        let mut third = pin!(limiter.acquire());
        assert!(poll_once(first.as_mut()).is_pending(), "A must wait");
        assert!(poll_once(second.as_mut()).is_pending(), "B must wait");
        assert!(poll_once(third.as_mut()).is_pending(), "C must wait");

        drop(held.pop());
        let Poll::Ready(permit_a) = poll_once(first.as_mut()) else {
            panic!("A, the first arrived, must obtain the first released place");
        };
        assert!(poll_once(second.as_mut()).is_pending(), "B must not overtake A");
        assert!(poll_once(third.as_mut()).is_pending(), "C must not overtake B");

        drop(held.pop());
        let Poll::Ready(permit_b) = poll_once(second.as_mut()) else {
            panic!("B must obtain the second released place");
        };
        assert!(
            poll_once(third.as_mut()).is_pending(),
            "C must wait until another place is released"
        );

        drop(held.pop());
        drop(permit_a);
        drop(permit_b);
        assert!(
            poll_once(third.as_mut()).is_ready(),
            "C obtains the last released place"
        );
    }

    /// Host counter whose count the test lowers, as another process ending its requests would.
    struct ExternalCount(AtomicU64);

    impl RequestCounter for ExternalCount {
        fn in_flight(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }

        fn acquired(&self) {}

        fn released(&self) {}
    }

    // Scenario: abandon d'une attente
    #[test]
    fn abandoned_wait_does_not_hold_back_a_free_place() {
        let external = Arc::new(ExternalCount(AtomicU64::new(1)));
        let shared_external = Arc::clone(&external);
        let counter: Arc<dyn RequestCounter> = shared_external;
        let limiter = InferenceLimiter::new(NonZeroU64::MIN, counter);
        {
            let mut abandoned = Box::pin(limiter.acquire());
            assert!(
                poll_once(abandoned.as_mut()).is_pending(),
                "the request must wait while the only place is held elsewhere"
            );
        }
        external.0.store(0, Ordering::SeqCst);
        let mut next = pin!(limiter.acquire());
        assert!(
            poll_once(next.as_mut()).is_ready(),
            "a request that finds a free place must not queue behind an abandoned wait"
        );
    }

    /// Host counter the test lowers, and that counts the places the limiter grants (`acquired`), as a shared count does.
    struct SharedCount {
        in_flight: AtomicU64,
        acquisitions: AtomicU64,
    }

    impl SharedCount {
        fn new(in_flight: u64) -> Arc<Self> {
            Arc::new(Self {
                in_flight: AtomicU64::new(in_flight),
                acquisitions: AtomicU64::new(0),
            })
        }

        /// The host lowers or raises the count, as another process starting or ending its requests would.
        fn set(&self, value: u64) {
            self.in_flight.store(value, Ordering::SeqCst);
        }

        fn acquisitions(&self) -> u64 {
            self.acquisitions.load(Ordering::SeqCst)
        }
    }

    impl RequestCounter for SharedCount {
        fn in_flight(&self) -> u64 {
            self.in_flight.load(Ordering::SeqCst)
        }

        fn acquired(&self) {
            let _previous = self.in_flight.fetch_add(1, Ordering::SeqCst);
            let _counted = self.acquisitions.fetch_add(1, Ordering::SeqCst);
        }

        fn released(&self) {
            let _previous = self
                .in_flight
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                    Some(count.saturating_sub(1))
                });
        }
    }

    // Scenario: le décompte baissé par l'hôte n'attribue la place qu'après l'appel de tick, pas avant
    #[test]
    fn the_first_waiter_gets_its_place_only_after_the_tick() {
        let count = SharedCount::new(1);
        let shared: Arc<dyn RequestCounter> = Arc::<SharedCount>::clone(&count);
        let limiter = InferenceLimiter::new(NonZeroU64::MIN, shared);
        let mut first = pin!(limiter.acquire());
        assert!(
            poll_once(first.as_mut()).is_pending(),
            "the request waits while the only place is held elsewhere"
        );
        count.set(0);
        assert!(
            poll_once(first.as_mut()).is_pending(),
            "a count lowered by the host is not seen before the tick"
        );
        limiter.tick();
        assert!(
            poll_once(first.as_mut()).is_ready(),
            "after the tick the first waiter obtains the free place"
        );
    }

    // Scenario: ordre d'arrivée respecté par tick, et une place par attente libre
    #[test]
    fn the_tick_grants_the_free_places_to_the_first_arrivals() {
        let count = SharedCount::new(3);
        let shared: Arc<dyn RequestCounter> = Arc::<SharedCount>::clone(&count);
        let limiter = InferenceLimiter::new(limit(3), shared);
        let mut first = pin!(limiter.acquire());
        let mut second = pin!(limiter.acquire());
        let mut third = pin!(limiter.acquire());
        let mut fourth = pin!(limiter.acquire());
        for (name, wait) in [
            ("first", &mut first),
            ("second", &mut second),
            ("third", &mut third),
            ("fourth", &mut fourth),
        ] {
            assert!(
                poll_once(wait.as_mut()).is_pending(),
                "{name} must wait while the three places are held"
            );
        }
        count.set(1);
        limiter.tick();
        // The grants are kept: dropping a granted permit would give its place to the next waiter.
        let first_grant = poll_once(first.as_mut());
        let second_grant = poll_once(second.as_mut());
        assert!(first_grant.is_ready(), "the first arrival obtains a free place");
        assert!(
            second_grant.is_ready(),
            "the second arrival obtains the other free place"
        );
        assert!(
            poll_once(third.as_mut()).is_pending(),
            "the third arrival waits: the free places are used up"
        );
        assert!(
            poll_once(fourth.as_mut()).is_pending(),
            "the fourth arrival waits behind the third"
        );
        assert_eq!(
            count.acquisitions(),
            2,
            "each place granted by the tick is counted as a new request"
        );
    }

    // Scenario: sans place libre, l'appel de tick n'attribue rien
    #[test]
    fn a_tick_without_a_free_place_grants_nothing() {
        let count = SharedCount::new(1);
        let shared: Arc<dyn RequestCounter> = Arc::<SharedCount>::clone(&count);
        let limiter = InferenceLimiter::new(NonZeroU64::MIN, shared);
        let mut waiting = pin!(limiter.acquire());
        assert!(
            poll_once(waiting.as_mut()).is_pending(),
            "the request waits while the only place is held"
        );
        limiter.tick();
        assert!(
            poll_once(waiting.as_mut()).is_pending(),
            "a tick with no free place leaves the waiter queued"
        );
        assert_eq!(count.acquisitions(), 0, "no place is granted, so none is counted");
    }

    // Scenario: appel de tick sans attente = sans effet
    #[test]
    fn a_tick_without_waiters_has_no_effect() {
        let count = SharedCount::new(1);
        let shared: Arc<dyn RequestCounter> = Arc::<SharedCount>::clone(&count);
        let limiter = InferenceLimiter::new(limit(2), shared);
        limiter.tick();
        assert_eq!(
            count.acquisitions(),
            0,
            "a tick with an empty queue counts nothing"
        );
        assert_eq!(count.in_flight(), 1, "the host count is left as the host set it");
        assert_eq!(
            limiter.available(),
            1,
            "the free place is still the one of the count"
        );
    }

    // Scenario: une nouvelle arrivée ne double pas la file quand le décompte baisse
    #[test]
    fn a_new_arrival_does_not_overtake_a_waiter_when_the_count_drops() {
        let count = SharedCount::new(1);
        let shared: Arc<dyn RequestCounter> = Arc::<SharedCount>::clone(&count);
        let limiter = InferenceLimiter::new(NonZeroU64::MIN, shared);
        let mut first = pin!(limiter.acquire());
        assert!(
            poll_once(first.as_mut()).is_pending(),
            "the first request waits while the only place is held"
        );
        count.set(0);
        let mut arrival = pin!(limiter.acquire());
        assert!(
            poll_once(arrival.as_mut()).is_pending(),
            "a new arrival must queue behind the waiter even though the count is free"
        );
        limiter.tick();
        // The grant is kept: dropping a granted permit would give its place to the arrival.
        let first_grant = poll_once(first.as_mut());
        assert!(
            first_grant.is_ready(),
            "the tick gives the free place to the waiter, which arrived first"
        );
        assert!(
            poll_once(arrival.as_mut()).is_pending(),
            "the arrival still waits: the place went to the first waiter"
        );
    }

    const STRESS_ROUNDS: usize = 200;
    const STRESS_POLLS: usize = 64;
    const STRESS_HELD: usize = 2;

    /// Polls a wait up to `polls` times, yielding between polls, and returns its permit if it was granted.
    fn poll_bounded(wait: Pin<&mut impl Future<Output = Permit>>, polls: usize) -> Option<Permit> {
        let mut wait = wait;
        for _ in 0..polls {
            if let Poll::Ready(permit) = poll_once(wait.as_mut()) {
                return Some(permit);
            }
            thread::yield_now();
        }
        None
    }

    /// One worker of the stress test: waits, keeps a few permits, and abandons the waits still pending after a bounded
    /// number of polls.
    fn stress_worker(limiter: &InferenceLimiter) {
        let mut held: VecDeque<Permit> = VecDeque::new();
        for _ in 0..STRESS_ROUNDS {
            let mut wait = Box::pin(limiter.acquire());
            if let Some(permit) = poll_bounded(wait.as_mut(), STRESS_POLLS) {
                held.push_back(permit);
            }
            drop(wait);
            if held.len() > STRESS_HELD {
                drop(held.pop_front());
            }
        }
    }

    // Scenario: abandon d'une attente
    #[test]
    fn concurrent_abandons_and_releases_restore_every_place() {
        const LIMIT: u64 = 4;
        const WORKERS: usize = 4;

        let limiter = Arc::new(InferenceLimiter::new(limit(LIMIT), Arc::new(LocalCounter::new())));
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                thread::spawn(move || stress_worker(&limiter))
            })
            .collect();
        for worker in workers {
            assert!(worker.join().is_ok(), "a worker thread must not panic");
        }
        // Every worker has ended: no request is waiting, so every place must be free and obtainable at once.
        let finals: Vec<Permit> = (0..LIMIT).map(|_| acquire_now(&limiter)).collect();
        assert_eq!(
            limiter.available(),
            0,
            "all the places are held by the final permits"
        );
        drop(finals);
        assert_eq!(
            limiter.available(),
            LIMIT,
            "every place is restored once every permit is dropped"
        );
    }
}
