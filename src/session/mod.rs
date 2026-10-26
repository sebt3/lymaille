//! Session lifecycle: persisted state, history, storage trait, events, cancellation, labels and quality signals.
//! This module holds types and pure functions; the storage I/O belongs to the host.

mod error;
mod id;

pub use error::{SessionError, SessionResult};
pub use id::{RunId, SessionId, TurnId};
