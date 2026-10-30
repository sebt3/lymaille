//! Delegation to autonomous sub-agents, watchers, structured results and the run registry.
//!
//! Declares the sub-modules of the domain as they are delivered (`agent-sessions/delegation`). The module `module` arrives
//! with its own lot (`./delegation.sdd`, task #3).

mod capacity;
mod error;
mod result;
mod run;
mod sources;
mod watcher;

pub use capacity::{InferenceLimiter, LocalCounter, Permit, RequestCounter, announced_capacity};
pub use error::{DelegationError, DelegationResult};
pub use result::{DEFAULT_CORRECTIONS, ResultCollector, SubagentOutcome, SubagentStatus, Submission};
pub use run::{LaunchRequest, RunHandle, RunProgress, RunRecord, RunRegistry, SubagentLauncher, run_batch};
pub use sources::{BUILTIN_SOURCES, EventHub, EventSource, Timer, TimerKind, TimerSource, WatchEvent};
pub use watcher::{Notification, Triggered, Watcher, WatcherAction, WatcherSet};
