//! Delegation to autonomous sub-agents, watchers, structured results and the run registry.
//!
//! Declares the sub-modules of the domain as they are delivered (`agent-sessions/delegation`). The typed error and the
//! inference limiter are here for now: the other files arrive with their own lots (`./delegation.sdd`, task #3).

mod capacity;
mod error;

pub use capacity::{InferenceLimiter, LocalCounter, Permit, RequestCounter, announced_capacity};
pub use error::{DelegationError, DelegationResult};
