//! lymaille is a library to run an LLM as a durable work agent.
//! It provides persistent session management, a Rhai scratchpad for
//! agent state, and lazily discovered MCP toolsets.
//! See `dsd/agent-sessions.md` for the full design.

mod agents;
mod clock;
mod context;
mod delegation;
mod modules;
mod runtime;
mod scratchpad;
mod secret;
mod session;
mod tools;
mod toolsets;

pub use agents::{
    AgentDefinition, AgentKind, AgentStore, AgentsError, AgentsResult, DetectedModel, ErrorClass,
    FinishReason, InMemoryCatalog, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse,
    Layered, ManifestGap, ManifestView, ModelProfile, ProfileStore, ProjectManifest, Provider, ProviderLevel,
    ProviderStore, ReportedUsage, ResolvedProfile, SamplingParams, SandboxLimits, ScriptedProvider,
    classify_status,
};
#[cfg(feature = "rig")]
pub use agents::{RigConfig, RigProvider, detect_models};
pub use clock::{Clock, ManualClock, SystemClock, Timestamp};
pub use context::{
    Budget, CompactionStrategy, ContextError, ContextResult, DEFAULT_SUMMARY_PROMPT, DefaultStrategy, Digest,
    ReplayMode, Summarizer, UsageTracker, estimate_tokens, replayed_reasoning, summary_prompt,
};
pub use delegation::{
    BUILTIN_SOURCES, DEFAULT_BATCH_MAX_TURNS, DEFAULT_CORRECTIONS, DelegationError, DelegationResult,
    EventHub, EventSource, InferenceLimiter, LaunchRequest, LocalCounter, MainAgent, Notification, Permit,
    RequestCounter, ResultCollector, RunHandle, RunProgress, RunRecord, RunRegistry, SubagentLauncher,
    SubagentOutcome, SubagentStatus, Submission, Timer, TimerKind, TimerSource, Triggered, WatchEvent,
    Watcher, WatcherAction, WatcherSet, announced_capacity, delegate_module, result_module, run_batch,
    workspace_notification,
};
pub use modules::{
    InMemoryMemoryStore, McpConnector, McpEnv, McpError, McpLister, McpNotification, McpRetryPolicy, McpTool,
    Memory, MemoryLevel, MemoryScope, MemoryScopes, MemoryStore, MemorySummary, ModulesError, ModulesResult,
    ScriptedConnector, TodoItem, TodoList, TodoStatus, function_name, mcp_module, mcp_shell_descriptor,
    memory_module, memory_module_name, name_tools, session_module, todo_module,
};
#[cfg(feature = "mcp-http")]
pub use modules::{McpAuth, McpClientConfig, McpHeaders, RmcpConnector};
pub use runtime::{
    RHAI_PAD_TOOL, RestoredSession, TurnConfig, TurnOutput, TurnRunner, UserInbox, restore_session,
    rhai_pad_definition,
};
pub use scratchpad::{
    CandidateSet, Catalog, CoreGroup, EngineBuilder, FunctionDescriptor, FunctionEntry, FunctionSource,
    GrantedModule, GrantedSkill, GrantedToolset, Grants, InspectionPad, InspectionTarget,
    IntrospectionSource, LimitKind, ModuleDescriptor, NotFoundKind, Origin, PadSnapshot, PersistWarning,
    Recompiled, Rendered, RestoreWarning, Restored, RunEnv, RunOutcome, RunStatus, ScopeSnapshot, Scratchpad,
    ScratchpadEngine, ScratchpadError, ScratchpadLimits, ScratchpadResult, ScriptErrorClass, ScriptFunction,
    ScriptPosition, UnpersistableReason, VariableInfo, descriptors_from_catalog, extract_function_sources,
    full_text, limits_notice, nearest, recompile_functions, register_format_functions, render_value,
    restore_scope, rhai_prod_inspect_definition, shell_module, snapshot_scope, suggest_for, to_dynamic,
    to_tagged, truncate_middle,
};
pub use secret::{DefaultSecretDetector, SecretDetector};
pub use session::{
    AgentRef, Backoff, BoundaryKind, CancelHandle, DiscardCause, Event, EventKind, EventSink, FormatVersion,
    History, InMemoryStore, InspectionTargetRef, Label, LabelAuthor, LabelTarget, Labels, McpInput,
    McpRecord, Message, ModelSnapshot, NativeTool, NullSink, Outcome, PendingEdits, Provenance, Reasoning,
    RecordingSink, Redactor, ReviewState, RunId, RunSignals, ScriptInput, ScriptRecord, Segment,
    SegmentContext, SessionEdits, SessionError, SessionId, SessionKind, SessionResult, SessionState,
    SessionStore, SleepOutcome, TaggedValue, TokenCounters, ToolCall, TurnGate, TurnId, TurnLock, TurnPermit,
    normalize_title, restore_notices, review_state, unresolved_call_notices,
};
pub use tools::{InspectEnv, InspectTool, InspectionHost, session_tool_definitions};
pub use toolsets::{
    Assembled, Bump, ConfigEnv, ConfigStores, ContextLevel, ContextStore, ContextType, ContextTypeStore,
    Contract, Delegation, DraftEntry, DraftKind, DraftStore, FlatContextType, Gap, GapKind, InMemoryToolsets,
    InspectionSlot, ParamDecl, RemovalReason, RemovedSubagent, ResolvedContext, RoleDecl, RoleSpec,
    ScriptError, SessionFilter, SessionIndex, SessionMeta, SessionSummary, Skill, SkillTool, StartTarget,
    Toolset, ToolsetContext, ToolsetStore, ToolsetsError, ToolsetsResult, TypeRequirement, VersionWarning,
    assemble, builder_registration, builtin_rhai_pad_skill, check_context, check_creation_rule,
    check_declared_version, check_session_module_rule, check_template, config_registration,
    delegable_subagents, extract_contract, render_prompt, render_skill, resolve_start,
    session_context_registration, skill_definition,
};
