//! Session lifecycle: persisted state, history, storage trait, events, cancellation, labels and quality signals.
//! This module holds types and pure functions; the storage I/O belongs to the host.

mod audit;
mod backoff;
mod cancel;
mod edits;
mod error;
mod event;
mod format;
mod history;
mod id;
mod label;
mod native_tool;
mod notice;
mod redact;
mod replay_mode;
mod segment;
mod signals;
mod state;
mod store;
mod tagged_value;
mod title;
mod turn_gate;

pub use audit::{McpInput, McpRecord, ScriptInput, ScriptRecord};
pub use backoff::Backoff;
pub use cancel::{CancelHandle, SleepOutcome};
pub use edits::{PendingEdits, SessionEdits};
pub use error::{SessionError, SessionResult};
pub use event::{Event, EventKind, EventSink, NullSink, RecordingSink};
pub use format::FormatVersion;
pub use history::{History, Message, Reasoning, ToolCall};
pub use id::{RunId, SessionId, TurnId};
pub use label::{Label, LabelAuthor, LabelTarget, Labels, ReviewState, review_state};
pub use native_tool::NativeTool;
pub use notice::{Provenance, restore_notices, unresolved_call_notices};
pub use redact::Redactor;
pub use replay_mode::ReplayMode;
pub use segment::{BoundaryKind, ModelSnapshot, Segment, SegmentContext};
pub use signals::{Outcome, RunSignals};
pub use state::{AgentRef, InspectionTargetRef, SessionKind, SessionState, TokenCounters};
pub use store::{InMemoryStore, SessionStore};
pub use tagged_value::TaggedValue;
pub use title::normalize_title;
pub use turn_gate::{TurnGate, TurnPermit};
