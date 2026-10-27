//! lymaille is a library to run an LLM as a durable work agent.
//! It provides persistent session management, a Rhai scratchpad for
//! agent state, and lazily discovered MCP toolsets.
//! See `dsd/agent-sessions.md` for the full design.

mod agents;
mod clock;
mod context;
mod session;

pub use agents::{
    AgentDefinition, AgentKind, AgentStore, AgentsError, AgentsResult, DetectedModel, ErrorClass,
    InMemoryCatalog, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse, MemoryRights,
    ModelProfile, ProfileStore, Provider, ProviderLevel, ProviderStore, ReportedUsage, ResolvedProfile,
    SamplingParams, SandboxLimits, ScriptedProvider, classify_status,
};
pub use clock::{Clock, ManualClock, SystemClock, Timestamp};
pub use context::{
    Budget, CompactionStrategy, ContextError, ContextResult, DefaultStrategy, Digest, ReplayMode, Summarizer,
    UsageTracker, estimate_tokens, replayed_reasoning,
};
pub use session::{
    AgentRef, Backoff, BoundaryKind, CancelHandle, Event, EventKind, EventSink, FormatVersion, History,
    InMemoryStore, Label, LabelAuthor, LabelTarget, Labels, McpInput, McpRecord, Message, ModelSnapshot,
    NullSink, Outcome, Provenance, Reasoning, RecordingSink, Redactor, RunId, RunSignals, ScriptInput,
    ScriptRecord, Segment, SegmentContext, SessionError, SessionId, SessionResult, SessionState,
    SessionStore, SleepOutcome, TaggedValue, TokenCounters, ToolCall, TurnGate, TurnId, TurnPermit,
    restore_notices,
};
