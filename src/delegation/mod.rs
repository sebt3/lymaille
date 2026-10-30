//! Delegation to autonomous sub-agents, watchers, structured results and the run registry.
//!
//! Declares the sub-modules of the domain as they are delivered (`agent-sessions/delegation`). The typed error, the
//! inference limiter and the event sources are here for now: the other files arrive with their own lots
//! (`./delegation.sdd`, task #3).

mod capacity;
mod error;
mod result;
mod sources;
mod watcher;

pub use capacity::{InferenceLimiter, LocalCounter, Permit, RequestCounter, announced_capacity};
pub use error::{DelegationError, DelegationResult};
pub use result::{DEFAULT_CORRECTIONS, ResultCollector, SubagentOutcome, SubagentStatus, Submission};
pub use sources::{BUILTIN_SOURCES, EventHub, EventSource, Timer, TimerKind, TimerSource, WatchEvent};
pub use watcher::{Notification, Triggered, Watcher, WatcherAction, WatcherSet};
