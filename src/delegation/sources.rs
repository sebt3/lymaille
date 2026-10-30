//! Events that watchers observe: the source trait the host implements, the hub that receives the notifications, the
//! built-in sources and the timers (`agent-sessions/delegation#FR-9`, `FR-10`, `FR-12`, `AC-6`).
//!
//! Nothing here starts a task: the host calls [`TimerSource::tick`] and [`EventHub::poll`] (`agent-sessions#NFR-1`).

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::error::{DelegationError, DelegationResult};
use crate::clock::Timestamp;

/// Names of the four built-in sources: `subagent`, `mcp`, `workspace` (todo and Memory) and `timer`. They are reserved:
/// the host cannot register its own source under one of them.
pub const BUILTIN_SOURCES: &[&str] = &["subagent", "mcp", "workspace", "timer"];

/// Name of the built-in timer source.
const TIMER_SOURCE: &str = "timer";

/// One event observed by a watcher: the source it comes from, its name, its data and the instant it was produced.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WatchEvent {
    source: String,
    name: String,
    data: serde_json::Value,
    at: Timestamp,
}

impl WatchEvent {
    /// Builds an event of the source `source`, named `name`, carrying `data`, produced at `at`.
    #[must_use]
    pub fn new(source: &str, name: &str, data: serde_json::Value, at: Timestamp) -> Self {
        Self {
            source: source.to_owned(),
            name: name.to_owned(),
            data,
            at,
        }
    }

    /// Name of the source that produced the event.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Name of the event.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Data carried by the event.
    #[must_use]
    pub const fn data(&self) -> &serde_json::Value {
        &self.data
    }

    /// Instant the event was produced at.
    #[must_use]
    pub const fn at(&self) -> Timestamp {
        self.at
    }
}

/// Source of events, implemented by the host and shareable between threads (`agent-sessions/delegation#FR-9`).
pub trait EventSource: Send + Sync {
    /// Stable name of the source: a non-empty identifier, unique in a hub.
    fn name(&self) -> &str;

    /// Returns the events waiting in the source. Each event is handed over once: a second call never repeats one.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot hand its events over, for example when its lock is poisoned.
    fn drain(&self) -> DelegationResult<Vec<WatchEvent>>;
}

/// Hub that receives the notifications of the host and the events of the registered sources, in arrival order.
///
/// The built-in sources `subagent`, `mcp` and `workspace` are fed by [`EventHub::notify`] without registration. The
/// timer source is attached with [`EventHub::attach_timers`]; its name cannot be notified.
#[derive(Default)]
pub struct EventHub {
    state: Mutex<HubState>,
}

/// Registered sources and the events waiting to be polled.
#[derive(Default)]
struct HubState {
    sources: Vec<Arc<dyn EventSource>>,
    timers: Option<Arc<TimerSource>>,
    pending: VecDeque<WatchEvent>,
}

impl EventHub {
    /// Builds a hub with no registered source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the hub state. A poisoned lock is refused.
    fn lock(&self) -> DelegationResult<MutexGuard<'_, HubState>> {
        self.state.lock().map_err(|_poisoned| DelegationError::Poisoned)
    }

    /// Registers a source under its name. A name that is empty, reserved (`BUILTIN_SOURCES`) or already taken is
    /// refused.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidWatcher`] for such a name, and [`DelegationError::Poisoned`] when the hub lock
    /// is poisoned.
    pub fn register(&self, source: Arc<dyn EventSource>) -> DelegationResult<()> {
        let name = source.name().to_owned();
        if name.is_empty() {
            return Err(DelegationError::InvalidWatcher {
                id: name,
                reason: "source name is empty",
            });
        }
        if BUILTIN_SOURCES.contains(&name.as_str()) {
            return Err(DelegationError::InvalidWatcher {
                id: name,
                reason: "source name is reserved",
            });
        }
        let mut state = self.lock()?;
        if state.sources.iter().any(|registered| registered.name() == name) {
            return Err(DelegationError::InvalidWatcher {
                id: name,
                reason: "source name is already registered",
            });
        }
        state.sources.push(source);
        drop(state);
        Ok(())
    }

    /// Notifies an event of the source `source` (the host's call). A source that is neither built-in nor registered is
    /// refused, and the error names it.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidWatcher`] naming the source when it is unknown, and
    /// [`DelegationError::Poisoned`] when the hub lock is poisoned.
    pub fn notify(
        &self,
        source: &str,
        name: &str,
        data: serde_json::Value,
        at: Timestamp,
    ) -> DelegationResult<()> {
        if source == TIMER_SOURCE {
            return Err(DelegationError::InvalidWatcher {
                id: source.to_owned(),
                reason: "timer events come from the attached timer source",
            });
        }
        let mut state = self.lock()?;
        let known = BUILTIN_SOURCES.contains(&source)
            || state.sources.iter().any(|registered| registered.name() == source);
        if !known {
            return Err(DelegationError::InvalidWatcher {
                id: source.to_owned(),
                reason: "unknown source",
            });
        }
        state.pending.push_back(WatchEvent::new(source, name, data, at));
        drop(state);
        Ok(())
    }

    /// Attaches the built-in timer source to the hub, so that [`EventHub::poll`] also returns its events. A second
    /// attachment is refused.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidWatcher`] when a timer source is already attached, and
    /// [`DelegationError::Poisoned`] when the hub lock is poisoned.
    pub fn attach_timers(&self, timers: Arc<TimerSource>) -> DelegationResult<()> {
        let mut state = self.lock()?;
        if state.timers.is_some() {
            return Err(DelegationError::InvalidWatcher {
                id: TIMER_SOURCE.to_owned(),
                reason: "timer source already attached",
            });
        }
        state.timers = Some(timers);
        drop(state);
        Ok(())
    }

    /// Returns every event waiting: first the notifications already received, in notification order; then the events
    /// drained from the registered sources, in registration order; then those of the attached timer source. A drained
    /// event arrives when it is drained here.
    ///
    /// # Errors
    ///
    /// Returns the error of a registered source whose drain fails, and [`DelegationError::Poisoned`] when a lock is
    /// poisoned.
    pub fn poll(&self) -> DelegationResult<Vec<WatchEvent>> {
        let mut feeds: Vec<Arc<dyn EventSource>> = self.lock()?.sources.clone();
        let attached = self.lock()?.timers.clone();
        if let Some(timers) = attached {
            feeds.push(timers);
        }
        for source in &feeds {
            let drained = source.drain()?;
            self.lock()?.pending.extend(drained);
        }
        let mut state = self.lock()?;
        let events = std::mem::take(&mut state.pending);
        drop(state);
        Ok(Vec::from(events))
    }
}

impl fmt::Debug for EventHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventHub").finish_non_exhaustive()
    }
}

/// Kind of a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimerKind {
    /// Fires once, when the clock reaches its due instant, then disappears.
    Deadline,
    /// Fires when the clock reaches its next trigger, then re-arms on the next multiple of its period.
    Periodic,
}

/// Timer persisted with the session: its identifier and its schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Timer {
    id: String,
    schedule: Schedule,
}

/// Schedule of a timer: the due instant, or the period and the next trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Schedule {
    Deadline { due: Timestamp },
    Periodic { period_ms: NonZeroU64, next: Timestamp },
}

impl Timer {
    /// Builds a deadline timer named `id`, due at `due`.
    #[must_use]
    pub fn deadline(id: &str, due: Timestamp) -> Self {
        Self {
            id: id.to_owned(),
            schedule: Schedule::Deadline { due },
        }
    }

    /// Builds a periodic timer named `id`, of period `period`, whose next trigger is `next`. A period shorter than one
    /// millisecond is refused.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidTimer`] when the period is shorter than one millisecond.
    pub fn periodic(id: &str, period: Duration, next: Timestamp) -> DelegationResult<Self> {
        let millis = u64::try_from(period.as_millis()).unwrap_or(u64::MAX);
        let period_ms = NonZeroU64::new(millis).ok_or_else(|| DelegationError::InvalidTimer {
            id: id.to_owned(),
            reason: "period must be at least one millisecond",
        })?;
        Ok(Self {
            id: id.to_owned(),
            schedule: Schedule::Periodic { period_ms, next },
        })
    }

    /// Identifier of the timer.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Kind of the timer.
    #[must_use]
    pub const fn kind(&self) -> TimerKind {
        match self.schedule {
            Schedule::Deadline { .. } => TimerKind::Deadline,
            Schedule::Periodic { .. } => TimerKind::Periodic,
        }
    }

    /// Instant the timer is due at (deadline) or next triggers at (periodic).
    #[must_use]
    pub const fn trigger(&self) -> Timestamp {
        match self.schedule {
            Schedule::Deadline { due } => due,
            Schedule::Periodic { next, .. } => next,
        }
    }
}

/// Built-in timer source: deadline and periodic timers, fired by the host's calls to [`TimerSource::tick`].
#[derive(Debug, Default)]
pub struct TimerSource {
    state: Mutex<TimerState>,
}

/// Timers in insertion order, and the events they produced that are not drained yet.
#[derive(Debug, Default)]
struct TimerState {
    timers: Vec<Timer>,
    pending: VecDeque<WatchEvent>,
}

impl TimerSource {
    /// Builds a timer source with no timer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the timer state. A poisoned lock is refused.
    fn lock(&self) -> DelegationResult<MutexGuard<'_, TimerState>> {
        self.state.lock().map_err(|_poisoned| DelegationError::Poisoned)
    }

    /// Adds a timer. An empty identifier or one already in use is refused.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidTimer`] for such an identifier, and [`DelegationError::Poisoned`] when the lock
    /// is poisoned.
    pub fn add(&self, timer: Timer) -> DelegationResult<()> {
        let mut state = self.lock()?;
        check_identifier(&state.timers, timer.id())?;
        state.timers.push(timer);
        drop(state);
        Ok(())
    }

    /// Removes the timer `id`. Returns whether a timer was removed.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the lock is poisoned.
    pub fn remove(&self, id: &str) -> DelegationResult<bool> {
        let mut state = self.lock()?;
        let before = state.timers.len();
        state.timers.retain(|timer| timer.id() != id);
        Ok(state.timers.len() != before)
    }

    /// Advances the timers to `now` and queues their events. Each periodic timer fires at most once per call.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the lock is poisoned.
    pub fn tick(&self, now: Timestamp) -> DelegationResult<()> {
        let mut state = self.lock()?;
        fire_due(&mut state, now);
        Ok(())
    }

    /// Replaces the timers with the persisted `timers`, then fires, at `now`, the ones whose trigger has passed.
    /// Timers with an empty or repeated identifier are refused, and the current timers are then kept.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidTimer`] for such an identifier, and [`DelegationError::Poisoned`] when the
    /// lock is poisoned.
    pub fn restore(&self, timers: Vec<Timer>, now: Timestamp) -> DelegationResult<()> {
        let mut accepted: Vec<Timer> = Vec::with_capacity(timers.len());
        for timer in timers {
            check_identifier(&accepted, timer.id())?;
            accepted.push(timer);
        }
        let mut state = self.lock()?;
        state.timers = accepted;
        fire_due(&mut state, now);
        drop(state);
        Ok(())
    }

    /// Returns the timers, in insertion order, to persist them with the session.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the lock is poisoned.
    pub fn timers(&self) -> DelegationResult<Vec<Timer>> {
        Ok(self.lock()?.timers.clone())
    }
}

impl EventSource for TimerSource {
    fn name(&self) -> &str {
        TIMER_SOURCE
    }

    fn drain(&self) -> DelegationResult<Vec<WatchEvent>> {
        let mut state = self.lock()?;
        let events = std::mem::take(&mut state.pending);
        drop(state);
        Ok(Vec::from(events))
    }
}

/// Refuses an empty identifier, or one that a timer of `timers` already carries.
fn check_identifier(timers: &[Timer], id: &str) -> DelegationResult<()> {
    if id.is_empty() {
        return Err(DelegationError::InvalidTimer {
            id: String::new(),
            reason: "identifier is empty",
        });
    }
    if timers.iter().any(|timer| timer.id() == id) {
        return Err(DelegationError::InvalidTimer {
            id: id.to_owned(),
            reason: "identifier is already in use",
        });
    }
    Ok(())
}

/// Fires the timers whose trigger has passed at `now`. A deadline fires once and disappears. A periodic timer fires
/// once and re-arms on the next multiple of its period strictly after `now`, so the occurrences it missed are skipped.
fn fire_due(state: &mut TimerState, now: Timestamp) {
    let TimerState { timers, pending } = state;
    let mut kept = Vec::with_capacity(timers.len());
    for mut timer in std::mem::take(timers) {
        let schedule = timer.schedule;
        match schedule {
            Schedule::Deadline { due } => {
                if now >= due {
                    pending.push_back(due_event(timer.id(), now, due));
                } else {
                    kept.push(timer);
                }
            }
            Schedule::Periodic { period_ms, next } => {
                if now >= next {
                    pending.push_back(due_event(timer.id(), now, next));
                    timer.schedule = Schedule::Periodic {
                        period_ms,
                        next: next_multiple_after(now, period_ms),
                    };
                }
                kept.push(timer);
            }
        }
    }
    *timers = kept;
}

/// Builds the `due` event of the timer `id`, which was due at `trigger` and is observed at `now`.
fn due_event(id: &str, now: Timestamp, trigger: Timestamp) -> WatchEvent {
    let late_ms = now.as_millis().saturating_sub(trigger.as_millis());
    WatchEvent::new(
        TIMER_SOURCE,
        "due",
        serde_json::json!({"id": id, "late_ms": late_ms}),
        now,
    )
}

/// Smallest multiple of `period_ms` strictly after `now`. Saturates at the maximum timestamp.
fn next_multiple_after(now: Timestamp, period_ms: NonZeroU64) -> Timestamp {
    let period = period_ms.get();
    let next = now
        .as_millis()
        .checked_div(period)
        .and_then(|quotient| quotient.checked_add(1))
        .and_then(|multiples| multiples.checked_mul(period))
        .unwrap_or(u64::MAX);
    Timestamp::from_millis(next)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::json;

    use super::{BUILTIN_SOURCES, EventHub, EventSource, Timer, TimerKind, TimerSource, WatchEvent};
    use crate::clock::Timestamp;
    use crate::delegation::DelegationError;

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    /// A host source that holds no event of its own: its events come through `EventHub::notify`.
    struct SilentSource(&'static str);

    impl EventSource for SilentSource {
        fn name(&self) -> &str {
            self.0
        }

        fn drain(&self) -> crate::delegation::DelegationResult<Vec<WatchEvent>> {
            Ok(Vec::new())
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

    // Scenario: une échéance se déclenche une fois
    #[test]
    fn deadline_fires_once_at_its_due_instant() {
        let source = TimerSource::new();
        source
            .add(Timer::deadline("daily", at(1000)))
            .expect("a deadline timer is accepted");

        source.tick(at(999)).expect("tick succeeds");
        assert!(
            source.drain().expect("drain succeeds").is_empty(),
            "no event may be produced before the due instant"
        );

        source.tick(at(1000)).expect("tick succeeds");
        let events = source.drain().expect("drain succeeds");
        assert_eq!(events.len(), 1, "the deadline must produce exactly one event");
        let event = &events[0];
        assert_eq!(event.name(), "due", "the event of a timer is named `due`");
        assert_eq!(
            event.source(),
            "timer",
            "the event of a timer comes from the timer source"
        );
        assert_eq!(
            event.data()["id"],
            json!("daily"),
            "the event carries the timer identifier"
        );
        assert_eq!(
            event.data()["late_ms"],
            json!(0),
            "a deadline reached on time carries a lateness of 0 ms"
        );

        source.tick(at(1500)).expect("tick succeeds");
        assert!(
            source.drain().expect("drain succeeds").is_empty(),
            "a fired deadline disappears: no second event"
        );
    }

    // Scenario: échéance manquée à la restauration
    #[test]
    fn missed_deadline_fires_once_at_restore_with_its_lateness() {
        let source = TimerSource::new();
        source
            .restore(vec![Timer::deadline("daily", at(1000))], at(5000))
            .expect("restore succeeds");

        let events = source.drain().expect("drain succeeds");
        assert_eq!(
            events.len(),
            1,
            "the missed deadline must fire exactly once at restore"
        );
        assert_eq!(events[0].name(), "due", "the event of a timer is named `due`");
        assert_eq!(
            events[0].data()["late_ms"],
            json!(4000),
            "the lateness is the restore instant minus the due instant"
        );

        source.tick(at(6000)).expect("tick succeeds");
        assert!(
            source.drain().expect("drain succeeds").is_empty(),
            "the missed deadline disappears after firing"
        );
    }

    // Scenario: minuteur périodique manqué plusieurs fois
    #[test]
    fn periodic_missed_many_times_fires_once_and_skips_to_next_trigger() {
        let source = TimerSource::new();
        let heartbeat = Timer::periodic("heartbeat", Duration::from_millis(1000), at(1000))
            .expect("a periodic timer of 1000 ms is accepted");
        source
            .restore(vec![heartbeat], at(4500))
            .expect("restore succeeds");

        let events = source.drain().expect("drain succeeds");
        assert_eq!(
            events.len(),
            1,
            "a periodic timer missed three times must fire once, not replay its missed occurrences"
        );
        let timers = source.timers().expect("timers are readable");
        assert_eq!(
            timers[0].trigger(),
            at(5000),
            "the next trigger is the next multiple of the period after the restore instant"
        );
    }

    // Scenario: un déclenchement par tick
    #[test]
    fn periodic_fires_once_per_tick_and_rearms() {
        let source = TimerSource::new();
        let heartbeat = Timer::periodic("heartbeat", Duration::from_millis(1000), at(1000))
            .expect("a periodic timer of 1000 ms is accepted");
        source.add(heartbeat).expect("the timer is accepted");

        source.tick(at(3500)).expect("tick succeeds");
        let events = source.drain().expect("drain succeeds");
        assert_eq!(events.len(), 1, "one tick fires a periodic timer at most once");

        let timers = source.timers().expect("timers are readable");
        assert_eq!(
            timers[0].trigger(),
            at(4000),
            "after a tick at 3500 ms the next trigger is the multiple 4000 ms"
        );
    }

    // Scenario: notification de l'hôte
    #[test]
    fn host_notification_is_polled_once_with_its_name_and_data() {
        let hub = EventHub::new();
        hub.register(Arc::new(SilentSource("host-alerts")))
            .expect("the host source is registered");
        hub.notify("host-alerts", "fired", json!({"level": "warn"}), at(10))
            .expect("the registered source accepts a notification");

        let events = hub.poll().expect("poll succeeds");
        assert_eq!(events.len(), 1, "the notification must be polled exactly once");
        assert_eq!(events[0].source(), "host-alerts", "the event keeps its source");
        assert_eq!(events[0].name(), "fired", "the event keeps its name");
        assert_eq!(
            events[0].data(),
            &json!({"level": "warn"}),
            "the event keeps its data"
        );

        assert!(
            hub.poll().expect("poll succeeds").is_empty(),
            "a polled event is not polled again"
        );
    }

    // Scenario: source inconnue
    #[test]
    fn notification_of_an_unknown_source_is_refused_and_names_it() {
        let hub = EventHub::new();
        let error = hub
            .notify("ghost", "fired", json!(null), at(10))
            .expect_err("an unknown source must be refused");
        assert!(
            error.to_string().contains("ghost"),
            "the error `{error}` must name the unknown source"
        );
        assert!(
            matches!(error, DelegationError::InvalidWatcher { .. }),
            "the refusal of an unknown source is an invalid watcher source, got `{error}`"
        );
    }

    // Scenario: nom de source pris ou réservé
    #[test]
    fn duplicate_or_reserved_source_name_is_refused() {
        let hub = EventHub::new();
        hub.register(Arc::new(SilentSource("host-alerts")))
            .expect("the first source under a name is accepted");
        assert!(
            hub.register(Arc::new(SilentSource("host-alerts"))).is_err(),
            "a second source under a taken name must be refused"
        );
        assert!(
            hub.register(Arc::new(SilentSource("timer"))).is_err(),
            "a host source under the reserved name `timer` must be refused"
        );
    }

    // Scenario: aller-retour des minuteurs
    #[test]
    fn persisted_timers_restore_without_event_and_equal() {
        let source = TimerSource::new();
        source
            .add(Timer::deadline("daily", at(2000)))
            .expect("the deadline is accepted");
        source
            .add(Timer::periodic("heartbeat", Duration::from_millis(1000), at(2000)).expect("valid period"))
            .expect("the periodic timer is accepted");
        let persisted = source.timers().expect("timers are readable");
        assert_eq!(
            persisted.len(),
            2,
            "both timers must be readable for persistence, so the round trip is not vacuous"
        );

        let json = serde_json::to_string(&persisted).expect("timers serialize");
        let back: Vec<Timer> = serde_json::from_str(&json).expect("timers deserialize");

        let restored = TimerSource::new();
        restored.restore(back, at(1000)).expect("restore succeeds");
        assert!(
            restored.drain().expect("drain succeeds").is_empty(),
            "restoring timers that are not yet due must produce no event"
        );
        assert_eq!(
            restored.timers().expect("timers are readable"),
            persisted,
            "the restored timers must equal the persisted ones"
        );
    }

    // Must: the duplicate and empty identifiers of a timer are refused.
    #[test]
    fn timer_identifiers_must_be_non_empty_and_unique() {
        let source = TimerSource::new();
        assert!(
            source.add(Timer::deadline("", at(1000))).is_err(),
            "an empty timer identifier must be refused"
        );
        source
            .add(Timer::deadline("daily", at(1000)))
            .expect("the first timer is accepted");
        assert!(
            source.add(Timer::deadline("daily", at(2000))).is_err(),
            "a timer identifier already in use must be refused"
        );
    }

    // Must: a periodic timer of zero duration is refused.
    #[test]
    fn zero_period_is_refused() {
        let error = Timer::periodic("heartbeat", Duration::ZERO, at(1000))
            .expect_err("a zero period must be refused");
        assert!(
            matches!(error, DelegationError::InvalidTimer { .. }),
            "a zero period is an invalid timer, got `{error}`"
        );
        assert_eq!(
            Timer::deadline("daily", at(1000)).kind(),
            TimerKind::Deadline,
            "a deadline timer reports its kind"
        );
    }

    // Must: a source drains each event once.
    #[test]
    fn timer_source_never_repeats_an_event() {
        let source = TimerSource::new();
        source
            .add(Timer::deadline("daily", at(1000)))
            .expect("the deadline is accepted");
        source.tick(at(1000)).expect("tick succeeds");
        assert_eq!(
            source.drain().expect("drain succeeds").len(),
            1,
            "one event is waiting"
        );
        assert!(
            source.drain().expect("drain succeeds").is_empty(),
            "a second drain must not repeat the event"
        );
    }

    // Must: the hub keeps arrival order.
    #[test]
    fn hub_polls_in_arrival_order() {
        let hub = EventHub::new();
        for name in ["first", "second", "third"] {
            hub.notify("subagent", name, json!(null), at(1))
                .expect("a built-in source accepts a notification");
        }
        let names: Vec<String> = hub
            .poll()
            .expect("poll succeeds")
            .iter()
            .map(|event| event.name().to_owned())
            .collect();
        assert_eq!(
            names,
            vec!["first".to_owned(), "second".to_owned(), "third".to_owned()],
            "events must be polled in the order they arrived"
        );
    }

    // Must: the built-in names are the four named by the spec.
    #[test]
    fn builtin_sources_are_the_four_named_ones() {
        assert_eq!(
            BUILTIN_SOURCES,
            &["subagent", "mcp", "workspace", "timer"],
            "the built-in sources are subagent, mcp, workspace and timer"
        );
    }

    // Must: the hub and the timer source can be shared between threads.
    #[test]
    fn hub_and_timer_source_are_send_and_sync() {
        assert_send_sync::<EventHub>();
        assert_send_sync::<TimerSource>();
    }

    /// A host source that holds events of its own and hands them over once.
    struct QueueSource {
        name: &'static str,
        events: Mutex<Vec<WatchEvent>>,
    }

    impl EventSource for QueueSource {
        fn name(&self) -> &str {
            self.name
        }

        fn drain(&self) -> crate::delegation::DelegationResult<Vec<WatchEvent>> {
            let mut events = self
                .events
                .lock()
                .map_err(|_poisoned| DelegationError::Poisoned)?;
            Ok(std::mem::take(&mut *events))
        }
    }

    /// Returns the identifier and the reason of an `InvalidWatcher` error, and fails on any other error.
    fn invalid_watcher(error: DelegationError) -> (String, &'static str) {
        let text = error.to_string();
        let DelegationError::InvalidWatcher { id, reason } = error else {
            panic!("expected an invalid watcher error, got `{text}`")
        };
        (id, reason)
    }

    // Scenario: rattachement des minuteurs
    #[test]
    fn attached_timer_source_events_are_polled() {
        let timers = Arc::new(TimerSource::new());
        let hub = EventHub::new();
        hub.attach_timers(Arc::clone(&timers))
            .expect("the first attachment of a timer source is accepted");
        timers
            .add(Timer::deadline("daily", at(1000)))
            .expect("the deadline is accepted");
        timers.tick(at(1000)).expect("tick succeeds");

        let events = hub.poll().expect("poll succeeds");
        assert_eq!(
            events.len(),
            1,
            "poll must return the due event of the attached timers"
        );
        assert_eq!(events[0].name(), "due", "the event of a timer is named `due`");
        assert_eq!(
            events[0].source(),
            "timer",
            "the event comes from the timer source"
        );
        assert_eq!(
            events[0].data()["id"],
            json!("daily"),
            "the event carries the timer identifier"
        );
        assert!(
            hub.poll().expect("poll succeeds").is_empty(),
            "an event drained by poll is not returned again"
        );
    }

    // Scenario: rattachement des minuteurs
    #[test]
    fn second_timer_attachment_is_refused() {
        let hub = EventHub::new();
        hub.attach_timers(Arc::new(TimerSource::new()))
            .expect("the first attachment is accepted");
        let error = hub
            .attach_timers(Arc::new(TimerSource::new()))
            .expect_err("a second timer source must be refused");
        let (id, reason) = invalid_watcher(error);
        assert_eq!(id, "timer", "the refusal names the timer source");
        assert_eq!(
            reason, "timer source already attached",
            "the refusal states why a second attachment is refused"
        );
    }

    // Scenario: notification refusée pour timer
    #[test]
    fn notify_refuses_the_timer_source_name() {
        let hub = EventHub::new();
        let error = hub
            .notify("timer", "due", json!({}), at(1))
            .expect_err("timer events come only from the attached timer source");
        let (id, reason) = invalid_watcher(error);
        assert_eq!(id, "timer", "the refusal names the timer source");
        assert_eq!(
            reason, "timer events come from the attached timer source",
            "the refusal states where timer events come from"
        );
    }

    // Scenario: sources intégrées alimentées sans enregistrement
    #[test]
    fn builtin_sources_are_fed_by_notify_without_registration() {
        let hub = EventHub::new();
        hub.notify("subagent", "ended", json!(1), at(1))
            .expect("subagent is fed by notify without registration");
        hub.notify("mcp", "changed", json!(2), at(2))
            .expect("mcp is fed by notify without registration");
        hub.notify("workspace", "todo", json!(3), at(3))
            .expect("workspace is fed by notify without registration");
        let events = hub.poll().expect("poll succeeds");
        let sources: Vec<&str> = events.iter().map(WatchEvent::source).collect();
        assert_eq!(
            sources,
            vec!["subagent", "mcp", "workspace"],
            "the notifications of the built-in sources keep their arrival order"
        );
    }

    // Scenario: refus d'enregistrement, variante et raison
    #[test]
    fn register_refusals_name_the_variant_and_the_reason() {
        let hub = EventHub::new();
        let (id, reason) = invalid_watcher(
            hub.register(Arc::new(SilentSource("")))
                .expect_err("an empty source name must be refused"),
        );
        assert_eq!(id, "", "the refusal of an empty name names the empty identifier");
        assert_eq!(
            reason, "source name is empty",
            "the reason of an empty name is stated"
        );

        hub.register(Arc::new(SilentSource("host-alerts")))
            .expect("the first source under a name is accepted");
        let (taken, taken_reason) = invalid_watcher(
            hub.register(Arc::new(SilentSource("host-alerts")))
                .expect_err("a taken name must be refused"),
        );
        assert_eq!(taken, "host-alerts", "the refusal names the taken source");
        assert_eq!(
            taken_reason, "source name is already registered",
            "the reason of a taken name is stated"
        );
    }

    // Scenario: noms réservés refusés à l'enregistrement
    #[test]
    fn every_builtin_name_is_reserved_at_registration() {
        for name in BUILTIN_SOURCES {
            let hub = EventHub::new();
            let (id, reason) = invalid_watcher(
                hub.register(Arc::new(SilentSource(name)))
                    .expect_err("a reserved built-in name must be refused"),
            );
            assert_eq!(id, *name, "the refusal names the reserved source `{name}`");
            assert_eq!(
                reason, "source name is reserved",
                "the reason of `{name}` is stated"
            );
        }
    }

    // Scenario: échec de restauration sans perte
    #[test]
    fn failed_restore_keeps_the_previous_timers() {
        let source = TimerSource::new();
        source
            .add(Timer::deadline("daily", at(2000)))
            .expect("the deadline is accepted");
        let before = source.timers().expect("timers are readable");

        let duplicated = source.restore(
            vec![Timer::deadline("a", at(3000)), Timer::deadline("a", at(4000))],
            at(1000),
        );
        assert!(duplicated.is_err(), "a repeated identifier must fail the restore");
        assert_eq!(
            source.timers().expect("timers are readable"),
            before,
            "a failed restore must keep the previous timers"
        );

        let empty = source.restore(vec![Timer::deadline("", at(3000))], at(1000));
        assert!(empty.is_err(), "an empty identifier must fail the restore");
        assert_eq!(
            source.timers().expect("timers are readable"),
            before,
            "a failed restore must keep the previous timers after an empty identifier too"
        );
    }

    // Scenario: retrait d'un minuteur
    #[test]
    fn remove_reports_whether_a_timer_was_removed() {
        let source = TimerSource::new();
        source
            .add(Timer::deadline("daily", at(2000)))
            .expect("the deadline is accepted");
        assert!(
            source.remove("daily").expect("remove succeeds"),
            "removing a present timer reports true"
        );
        assert!(
            source.timers().expect("timers are readable").is_empty(),
            "a removed timer is no longer listed"
        );
        assert!(
            !source.remove("daily").expect("remove succeeds"),
            "removing an absent timer reports false"
        );
    }

    // Scenario: périodique tiré exactement à son prochain déclenchement
    #[test]
    fn periodic_fires_exactly_at_its_next_trigger_with_zero_lateness() {
        let source = TimerSource::new();
        source
            .add(Timer::periodic("heartbeat", Duration::from_millis(1000), at(1000)).expect("valid period"))
            .expect("the timer is accepted");
        source.tick(at(1000)).expect("tick succeeds");

        let events = source.drain().expect("drain succeeds");
        assert_eq!(
            events.len(),
            1,
            "a tick exactly at the next trigger fires the timer"
        );
        assert_eq!(
            events[0].data()["late_ms"],
            json!(0),
            "a timer fired on its trigger instant has no lateness"
        );
        assert_eq!(
            source.timers().expect("timers are readable")[0].trigger(),
            at(2000),
            "after firing at 1000 ms the next trigger is 2000 ms"
        );
    }

    // Scenario: retard d'un minuteur périodique
    #[test]
    fn periodic_lateness_is_measured_from_its_scheduled_trigger() {
        let source = TimerSource::new();
        source
            .add(Timer::periodic("heartbeat", Duration::from_millis(1000), at(1000)).expect("valid period"))
            .expect("the timer is accepted");
        source.tick(at(3500)).expect("tick succeeds");

        let events = source.drain().expect("drain succeeds");
        assert_eq!(events.len(), 1, "one tick fires the periodic timer once");
        assert_eq!(
            events[0].data()["late_ms"],
            json!(2500),
            "the lateness of a periodic timer is the tick instant minus its scheduled trigger"
        );
    }

    // Scenario: ordre d'arrivée de poll
    #[test]
    fn poll_order_is_notifications_then_sources_then_timers() {
        let hub = EventHub::new();
        hub.register(Arc::new(QueueSource {
            name: "host-a",
            events: Mutex::new(vec![WatchEvent::new("host-a", "a1", json!(1), at(1))]),
        }))
        .expect("the first host source is registered");
        hub.register(Arc::new(QueueSource {
            name: "host-b",
            events: Mutex::new(vec![WatchEvent::new("host-b", "b1", json!(2), at(1))]),
        }))
        .expect("the second host source is registered");
        let timers = Arc::new(TimerSource::new());
        hub.attach_timers(Arc::clone(&timers))
            .expect("the timer source is attached");
        timers
            .add(Timer::deadline("daily", at(1000)))
            .expect("the deadline is accepted");
        timers.tick(at(1000)).expect("tick succeeds");
        hub.notify("subagent", "n1", json!(0), at(2))
            .expect("a built-in source accepts a notification");

        let order: Vec<String> = hub
            .poll()
            .expect("poll succeeds")
            .iter()
            .map(|event| format!("{}:{}", event.source(), event.name()))
            .collect();
        assert_eq!(
            order,
            vec![
                "subagent:n1".to_owned(),
                "host-a:a1".to_owned(),
                "host-b:b1".to_owned(),
                "timer:due".to_owned(),
            ],
            "poll must return the notifications, then the registered sources in registration order, then the timers"
        );
    }

    // Scenario: poll draine une source enregistrée
    #[test]
    fn poll_drains_a_registered_source_once() {
        let hub = EventHub::new();
        let queue = QueueSource {
            name: "host-queue",
            events: Mutex::new(vec![
                WatchEvent::new("host-queue", "first", json!(1), at(1)),
                WatchEvent::new("host-queue", "second", json!(2), at(2)),
            ]),
        };
        hub.register(Arc::new(queue))
            .expect("the host source is registered");

        let names: Vec<String> = hub
            .poll()
            .expect("poll succeeds")
            .iter()
            .map(|event| event.name().to_owned())
            .collect();
        assert_eq!(
            names,
            vec!["first".to_owned(), "second".to_owned()],
            "poll must return the events of the registered source, in its order"
        );
        assert!(
            hub.poll().expect("poll succeeds").is_empty(),
            "a drained source does not hand its events over twice"
        );
    }
}
