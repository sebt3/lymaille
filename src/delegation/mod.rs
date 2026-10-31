//! Delegation to autonomous sub-agents, watchers, structured results and the run registry.
//!
//! Declares the sub-modules of the domain (`agent-sessions/delegation`): every one of them is delivered, and the Rhai modules
//! `delegate` and `result` are built by `module` (`./module.sdd`).

mod capacity;
mod error;
mod module;
mod result;
mod run;
mod sources;
mod watcher;

pub use capacity::{InferenceLimiter, LocalCounter, Permit, RequestCounter, announced_capacity};
pub use error::{DelegationError, DelegationResult};
pub use module::{MainAgent, delegate_module, result_module};
pub use result::{DEFAULT_CORRECTIONS, ResultCollector, SubagentOutcome, SubagentStatus, Submission};
pub use run::{
    DEFAULT_BATCH_MAX_TURNS, LaunchRequest, RunHandle, RunProgress, RunRecord, RunRegistry, SubagentLauncher,
    run_batch,
};
pub use sources::{BUILTIN_SOURCES, EventHub, EventSource, Timer, TimerKind, TimerSource, WatchEvent};
pub use watcher::{Notification, Triggered, Watcher, WatcherAction, WatcherSet};
