//! Session lifecycle: persisted state, history, storage trait, events, cancellation, labels and quality signals.
//! This module holds types and pure functions; the storage I/O belongs to the host.

mod backoff;
mod error;
mod id;
mod redact;
mod turn_gate;

pub use backoff::Backoff;
pub use error::{SessionError, SessionResult};
pub use id::{RunId, SessionId, TurnId};
pub use redact::Redactor;
pub use turn_gate::{TurnGate, TurnPermit};
