//! lymaille is a library to run an LLM as a durable work agent.
//! It provides persistent session management, a Rhai scratchpad for
//! agent state, and lazily discovered MCP toolsets.
//! See `dsd/agent-sessions.md` for the full design.

mod clock;
mod session;

pub use clock::{Clock, ManualClock, SystemClock, Timestamp};
pub use session::{
    Backoff, BoundaryKind, CancelHandle, Event, EventKind, EventSink, History, Label, LabelAuthor,
    LabelTarget, Labels, McpRecord, Message, ModelProfile, NullSink, Outcome, Reasoning, RecordingSink,
    Redactor, RunId, RunSignals, ScriptRecord, Segment, SegmentContext, SessionError, SessionId,
    SessionResult, SleepOutcome, ToolCall, TurnGate, TurnId, TurnPermit,
};
