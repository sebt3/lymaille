//! lymaille is a library to run an LLM as a durable work agent.
//! It provides persistent session management, a Rhai scratchpad for
//! agent state, and lazily discovered MCP toolsets.
//! See `dsd/agent-sessions.md` for the full design.

mod clock;
mod session;

pub use clock::{Clock, ManualClock, SystemClock, Timestamp};
pub use session::{
    Backoff, BoundaryKind, CancelHandle, Event, EventKind, EventSink, FormatVersion, History, Label,
    LabelAuthor, LabelTarget, Labels, McpRecord, Message, ModelProfile, NullSink, Outcome, Provenance,
    Reasoning, RecordingSink, Redactor, RunId, RunSignals, ScriptRecord, Segment, SegmentContext,
    SessionError, SessionId, SessionResult, SleepOutcome, TaggedValue, ToolCall, TurnGate, TurnId,
    TurnPermit, restore_notices,
};
