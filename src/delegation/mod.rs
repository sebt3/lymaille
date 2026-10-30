//! Delegation to autonomous sub-agents, watchers, structured results and the run registry.
//!
//! Declares the sub-modules of the domain as they are delivered (`agent-sessions/delegation`). Only the typed error is
//! here for now: the other files arrive with their own lots (`./delegation.sdd`, task #3).

mod error;

pub use error::{DelegationError, DelegationResult};
