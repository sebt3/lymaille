//! Session lifecycle: persisted state, history, storage trait, events, cancellation, labels and quality signals.
//! This module holds types and pure functions; the storage I/O belongs to the host.

mod audit;
mod backoff;
mod cancel;
mod error;
mod event;
mod id;
mod redact;
mod signals;
mod turn_gate;

pub use audit::{McpRecord, ScriptRecord};
pub use backoff::Backoff;
pub use cancel::{CancelHandle, SleepOutcome};
pub use error::{SessionError, SessionResult};
pub use event::{Event, EventKind, EventSink, NullSink, RecordingSink};
pub use id::{RunId, SessionId, TurnId};
pub use redact::Redactor;
pub use signals::Outcome;
pub use turn_gate::{TurnGate, TurnPermit};
