//! Session lifecycle: persisted state, history, storage trait, events, cancellation, labels and quality signals.
//! This module holds types and pure functions; the storage I/O belongs to the host.

mod audit;
mod backoff;
mod cancel;
mod error;
mod event;
mod history;
mod id;
mod label;
mod redact;
mod segment;
mod signals;
mod turn_gate;

pub use audit::{McpRecord, ScriptRecord};
pub use backoff::Backoff;
pub use cancel::{CancelHandle, SleepOutcome};
pub use error::{SessionError, SessionResult};
pub use event::{Event, EventKind, EventSink, NullSink, RecordingSink};
pub use history::{History, Message, Reasoning, ToolCall};
pub use id::{RunId, SessionId, TurnId};
pub use label::{Label, LabelAuthor, LabelTarget, Labels};
pub use redact::Redactor;
pub use segment::{BoundaryKind, ModelProfile, Segment, SegmentContext};
pub use signals::{Outcome, RunSignals};
pub use turn_gate::{TurnGate, TurnPermit};
