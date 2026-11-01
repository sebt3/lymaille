//! Modules of the configuration agent: `contexts` (compatibility and resolved values of a toolset context), `inspect` (the
//! inspection target, shared with the `rhai_prod_inspect` tool), `sessions` (reading past sessions), and the index of
//! sessions that the host implements (`agent-sessions/toolsets#FR-19`, `agent-sessions#BR-8`). The functions are synchronous
//! Rhai functions that call the asynchronous stores through `vynil_core::rt::block_on`. None of them writes to a store.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};
use std::sync::{Arc, Mutex, PoisonError};

use rhai::{
    Array, Dynamic, Engine as RhaiEngine, EvalAltResult, ImmutableString, Map, Module, Position, Shared,
};
use serde_json::Value;
use vynil_core::rt::block_on;

use super::{
    ContextStore, ContextType, ContextTypeStore, ResolvedContext, Toolset, ToolsetContext, ToolsetStore,
    ToolsetsError, delegable_subagents,
};
use crate::agents::{AgentDefinition, AgentStore};
use crate::clock::Clock;
use crate::scratchpad::{ScratchpadResult, ScriptErrorClass};
use crate::session::{
    Event, EventKind, EventSink, InspectionTargetRef, Redactor, SessionId, SessionKind, SessionResult,
    SessionState, SessionStore, TurnId,
};

/// Filter of a listing of past sessions. An absent field does not filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionFilter {
    agent: Option<String>,
    kind: Option<SessionKind>,
    limit: Option<usize>,
}

impl SessionFilter {
    /// A filter that keeps every session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Keeps the sessions of the agent named `agent`.
    #[must_use]
    pub fn with_agent<S: Into<String>>(mut self, agent: S) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Keeps the sessions of the nature `kind`.
    #[must_use]
    pub const fn with_kind(mut self, kind: SessionKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Keeps at most `limit` sessions.
    #[must_use]
    pub const fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The agent the sessions must have, when the filter names one.
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// The nature the sessions must have, when the filter names one.
    #[must_use]
    pub const fn kind(&self) -> Option<SessionKind> {
        self.kind
    }

    /// The largest number of sessions, when the filter sets one.
    #[must_use]
    pub const fn limit(&self) -> Option<usize> {
        self.limit
    }
}

/// A script error of a run of a past session, as the host audit log keeps it: the turn, the class and the message text.
/// The text is masked by the configuration modules before it reaches a script.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScriptError {
    turn: TurnId,
    class: ScriptErrorClass,
    text: String,
}

impl fmt::Debug for ScriptError {
    /// Shows the turn and the class, never the message text: a script error may carry a secret.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptError")
            .field("turn", &self.turn)
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

impl ScriptError {
    /// Builds the error of the run of `turn`, of class `class`, with the message `text`.
    #[must_use]
    pub fn new<S: Into<String>>(turn: TurnId, class: ScriptErrorClass, text: S) -> Self {
        Self {
            turn,
            class,
            text: text.into(),
        }
    }

    /// The turn of the run that failed.
    #[must_use]
    pub const fn turn(&self) -> TurnId {
        self.turn
    }

    /// The class of the error.
    #[must_use]
    pub const fn class(&self) -> ScriptErrorClass {
        self.class
    }

    /// The message of the error, not masked.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Index of the sessions, implemented by the host: it lists the identifiers of the sessions that match a filter, and the
/// script errors of a session. The states are read through [`SessionStore`].
///
/// The returned futures are `Send`. The trait is used through a generic parameter only, never as a trait object.
pub trait SessionIndex: Send + Sync {
    /// Lists the identifiers of the sessions that match `filter`, at most its limit.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Store`](crate::session::SessionError::Store) when the host storage fails.
    fn list(&self, filter: &SessionFilter) -> impl Future<Output = SessionResult<Vec<SessionId>>> + Send;

    /// Lists the script errors of the runs of the session `id`, read from the host audit log: the state of a session does
    /// not keep them.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Store`](crate::session::SessionError::Store) when the host storage fails.
    fn script_errors(&self, id: &SessionId) -> impl Future<Output = SessionResult<Vec<ScriptError>>> + Send;
}

/// Summary of a past session: identifier, agent, nature, the turns that carry signals, the total of their Rhai errors and
/// the names of the functions the LLM defined.
///
/// It never carries the text of a message nor the value of a variable.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionSummary {
    id: SessionId,
    agent: String,
    kind: SessionKind,
    turns: u64,
    rhai_errors: u64,
    functions: Vec<String>,
}

impl SessionSummary {
    /// Summarizes a session state. The function names are the keys of the state without their arity, once each.
    #[must_use]
    pub fn from_state(state: &SessionState) -> Self {
        let names: BTreeSet<String> = state
            .functions()
            .keys()
            .map(|key| {
                key.rsplit_once('/')
                    .map_or_else(|| key.clone(), |(name, _arity)| name.to_owned())
            })
            .collect();
        let signals = state.signals();
        let turns = u64::try_from(signals.len()).unwrap_or(u64::MAX);
        let rhai_errors = signals.values().fold(0u64, |total, run| {
            total.saturating_add(u64::from(run.rhai_errors()))
        });
        Self {
            id: state.id().clone(),
            agent: state.agent().name().to_owned(),
            kind: state.kind(),
            turns,
            rhai_errors,
            functions: names.into_iter().collect(),
        }
    }

    /// The identifier of the session.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// The name of the agent that ran the session.
    #[must_use]
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// The nature of the session.
    #[must_use]
    pub const fn kind(&self) -> SessionKind {
        self.kind
    }

    /// The number of turns that carry quality signals.
    #[must_use]
    pub const fn turns(&self) -> u64 {
        self.turns
    }

    /// The total of the Rhai errors counted by the signals of the turns.
    #[must_use]
    pub const fn rhai_errors(&self) -> u64 {
        self.rhai_errors
    }

    /// The names of the functions the LLM defined in the session, sorted.
    #[must_use]
    pub fn functions(&self) -> &[String] {
        &self.functions
    }
}

/// The stores of the configuration catalog: agent definitions, toolsets, context types and toolset contexts. The host
/// implements them; the configuration modules read them.
#[non_exhaustive]
pub struct ConfigStores<A, T, Y, C> {
    agents: A,
    toolsets: T,
    types: Y,
    contexts: C,
}

impl<A, T, Y, C> fmt::Debug for ConfigStores<A, T, Y, C> {
    /// Opaque: the stores may hold secrets, so none of them is shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigStores").finish_non_exhaustive()
    }
}

impl<A, T, Y, C> ConfigStores<A, T, Y, C> {
    /// The agent store, for the inspection tool of the `tools` domain.
    pub(crate) const fn agents(&self) -> &A {
        &self.agents
    }

    /// The toolset store, for the inspection tool of the `tools` domain.
    pub(crate) const fn toolsets(&self) -> &T {
        &self.toolsets
    }

    /// The toolset context store, for the inspection tool of the `tools` domain.
    pub(crate) const fn contexts(&self) -> &C {
        &self.contexts
    }

    /// Groups the agent store, the toolset store, the context type store and the toolset context store.
    #[must_use]
    pub const fn new(agents: A, toolsets: T, types: Y, contexts: C) -> Self {
        Self {
            agents,
            toolsets,
            types,
            contexts,
        }
    }
}

/// What the configuration modules share with the session they run in.
///
/// It groups the redactor that masks what they return, the sink that receives their events, the clock that dates them, and the identifier of the configuration session.
#[non_exhaustive]
pub struct ConfigEnv {
    redactor: Redactor,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    session: SessionId,
}

impl fmt::Debug for ConfigEnv {
    /// Opaque: the redactor holds the secrets to mask, so nothing is shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigEnv").finish_non_exhaustive()
    }
}

impl ConfigEnv {
    /// Groups the redactor of the session, the event sink, the clock and the identifier of the configuration session.
    #[must_use]
    pub const fn new(
        redactor: Redactor,
        sink: Arc<dyn EventSink>,
        clock: Arc<dyn Clock>,
        session: SessionId,
    ) -> Self {
        Self {
            redactor,
            sink,
            clock,
            session,
        }
    }
}

/// Emits the events of the `inspect` module, dated by the injected clock, in the configuration session.
struct TargetEvents {
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    session: SessionId,
}

impl TargetEvents {
    /// Reports that a target of nature `kind` named `name` was posed.
    fn opened(&self, kind: &str, name: &str) {
        self.sink.emit(Event::new(
            self.session.clone(),
            self.clock.now(),
            EventKind::TargetOpened {
                kind: kind.to_owned(),
                name: name.to_owned(),
            },
        ));
    }
}

/// The inspection target of a configuration session: one at a time, shared between the `inspect` module and the
/// `rhai_prod_inspect` tool. Opening a new target replaces the previous one.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct InspectionSlot {
    target: Mutex<Option<InspectionTargetRef>>,
}

impl InspectionSlot {
    /// A slot without a target.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            target: Mutex::new(None),
        }
    }

    /// The current target, when there is one.
    #[must_use]
    pub fn get(&self) -> Option<InspectionTargetRef> {
        self.target.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Replaces the current target; `None` removes it.
    pub fn set(&self, target: Option<InspectionTargetRef>) {
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) = target;
    }
}

/// A runtime error of a configuration function, carrying its message. Callers box it, as Rhai expects.
fn runtime_error(message: String) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(message), Position::NONE)
}

/// Runs a storage future to completion through the `vynil-core` bridge, on the calling thread. A storage failure and a
/// refusal of the bridge (a `current_thread` runtime, a cancelled run) both become Rhai errors that carry their message.
fn await_store<T, E>(future: impl Future<Output = Result<T, E>>) -> Result<T, Box<EvalAltResult>>
where
    E: Display,
{
    match block_on(future) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Box::new(runtime_error(error.to_string()))),
        Err(error) => Err(vynil_core::rhai_err(error)),
    }
}

/// The name of a nature, as the filter and the summaries write it.
const fn nature_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Work => "work",
        SessionKind::Config => "config",
        SessionKind::Sub => "sub",
    }
}

/// The nature a filter names, or a Rhai error that lists the known natures.
fn kind_of(text: &str) -> Result<SessionKind, Box<EvalAltResult>> {
    match text {
        "work" => Ok(SessionKind::Work),
        "config" => Ok(SessionKind::Config),
        "sub" => Ok(SessionKind::Sub),
        other => Err(Box::new(runtime_error(format!(
            "sessions::list: unknown nature `{other}`; expected work, config or sub"
        )))),
    }
}

/// Builds the filter of a `sessions::list` call from its map. An unknown key is refused and named.
fn filter_of(map: &Map) -> Result<SessionFilter, Box<EvalAltResult>> {
    let mut filter = SessionFilter::new();
    for (key, value) in map {
        filter = match key.as_str() {
            "agent" => filter.with_agent(text_of(key.as_str(), value)?),
            "nature" => filter.with_kind(kind_of(&text_of(key.as_str(), value)?)?),
            "limit" => filter.with_limit(limit_of(value)?),
            other => {
                return Err(Box::new(runtime_error(format!(
                    "sessions::list: unknown filter key `{other}`; expected agent, nature or limit"
                ))));
            }
        };
    }
    Ok(filter)
}

/// The text of a filter value, or a Rhai error that names its key.
fn text_of(key: &str, value: &Dynamic) -> Result<String, Box<EvalAltResult>> {
    value.clone().into_string().map_err(|found| {
        Box::new(runtime_error(format!(
            "sessions::list: `{key}` must be a string, not {found}"
        )))
    })
}

/// The limit of a filter value: a non-negative integer.
fn limit_of(value: &Dynamic) -> Result<usize, Box<EvalAltResult>> {
    let number = value.as_int().map_err(|found| {
        Box::new(runtime_error(format!(
            "sessions::list: `limit` must be a non-negative integer, not {found}"
        )))
    })?;
    usize::try_from(number).map_err(|error| {
        Box::new(runtime_error(format!(
            "sessions::list: `limit` must be a non-negative integer, got {number} ({error})"
        )))
    })
}

/// The value of a summary, masked by the redactor of the session: identifier, agent, nature and function names.
fn summary_value(summary: &SessionSummary, redactor: &Redactor) -> Dynamic {
    let functions: Array = summary
        .functions()
        .iter()
        .map(|name| Dynamic::from(redactor.redact(name)))
        .collect();
    let fields = [
        ("id", Dynamic::from(redactor.redact(summary.id().as_str()))),
        ("agent", Dynamic::from(redactor.redact(summary.agent()))),
        ("nature", Dynamic::from(nature_name(summary.kind()).to_owned())),
        ("turns", count_value(summary.turns())),
        ("rhai_errors", count_value(summary.rhai_errors())),
        ("functions", Dynamic::from(functions)),
    ];
    Dynamic::from(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<Map>(),
    )
}

/// Loads a session, or fails with a Rhai error that names the identifier when there is none.
fn load_session<X: SessionStore>(sessions: &X, id: &str) -> Result<SessionState, Box<EvalAltResult>> {
    let loaded = match SessionId::new(id) {
        Some(session_id) => await_store(sessions.load(&session_id))?,
        None => None,
    };
    loaded.ok_or_else(|| Box::new(runtime_error(format!("unknown session `{id}`"))))
}

/// Lists the summaries of the sessions that match `filter`, in the order of the index.
fn list_sessions<I: SessionIndex, X: SessionStore>(
    index: &I,
    sessions: &X,
    redactor: &Redactor,
    filter: &SessionFilter,
) -> Result<Array, Box<EvalAltResult>> {
    let ids = await_store(index.list(filter))?;
    let mut summaries = Array::new();
    for id in ids {
        if let Some(state) = await_store(sessions.load(&id))? {
            let summary = SessionSummary::from_state(&state);
            summaries.push(summary_value(&summary, redactor));
        }
    }
    Ok(summaries)
}

/// The source texts of the functions of a session, keyed by `name/arity`, masked by the redactor of the session.
fn functions_of<X: SessionStore>(
    sessions: &X,
    redactor: &Redactor,
    id: &str,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let state = load_session(sessions, id)?;
    let mut map = Map::new();
    for (key, source) in state.functions() {
        let _previous = map.insert(
            redactor.redact(key).into(),
            Dynamic::from(redactor.redact(source)),
        );
    }
    Ok(Dynamic::from(map))
}

/// Opens a session as the inspection target, replacing the previous target.
fn open_session<X: SessionStore>(
    sessions: &X,
    slot: &InspectionSlot,
    events: &TargetEvents,
    id: &str,
) -> Result<(), Box<EvalAltResult>> {
    let state = load_session(sessions, id)?;
    slot.set(Some(InspectionTargetRef::Session(state.id().clone())));
    events.opened("session", state.id().as_str());
    Ok(())
}

/// The text that describes an inspection target.
fn describe(target: Option<InspectionTargetRef>) -> String {
    match target {
        None => "no inspection target".to_owned(),
        Some(InspectionTargetRef::Definition { agent, context }) => {
            format!("definition of agent `{agent}` read with context `{context}`")
        }
        Some(InspectionTargetRef::Session(id)) => format!("session `{id}`"),
    }
}

/// A count as a Rhai integer. A count above the integer range saturates.
fn count_value(count: u64) -> Dynamic {
    Dynamic::from(i64::try_from(count).unwrap_or(i64::MAX))
}

/// The name of a class of script error, as the `errors` function writes it.
const fn class_name(class: ScriptErrorClass) -> &'static str {
    match class {
        ScriptErrorClass::Syntax => "syntax",
        ScriptErrorClass::Runtime => "runtime",
        ScriptErrorClass::NotFound => "not_found",
        ScriptErrorClass::Limit => "limit",
        ScriptErrorClass::Cancelled => "cancelled",
        ScriptErrorClass::Engine => "engine",
        ScriptErrorClass::Internal => "internal",
    }
}

/// The value of a script error, its message masked by the redactor of the session: turn, class and text.
fn error_value(error: &ScriptError, redactor: &Redactor) -> Dynamic {
    let fields = [
        ("turn", count_value(error.turn().get())),
        ("class", Dynamic::from(class_name(error.class()).to_owned())),
        ("text", Dynamic::from(redactor.redact(error.text()))),
    ];
    Dynamic::from(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<Map>(),
    )
}

/// The script errors of a session, read from the index, masked by the redactor of the session. The session must exist.
fn errors_of<I: SessionIndex, X: SessionStore>(
    index: &I,
    sessions: &X,
    redactor: &Redactor,
    id: &str,
) -> Result<Array, Box<EvalAltResult>> {
    let state = load_session(sessions, id)?;
    let errors = await_store(index.script_errors(state.id()))?;
    Ok(errors.iter().map(|error| error_value(error, redactor)).collect())
}

/// The agent `name`, or a Rhai error that names it when there is none.
fn agent_of<A: AgentStore>(agents: &A, name: &str) -> Result<AgentDefinition, Box<EvalAltResult>> {
    await_store(agents.get_agent(name))?
        .ok_or_else(|| Box::new(runtime_error(format!("unknown agent `{name}`"))))
}

/// The toolset context `name`, or a Rhai error that names it when there is none.
fn context_of<C: ContextStore>(contexts: &C, name: &str) -> Result<ToolsetContext, Box<EvalAltResult>> {
    await_store(contexts.get_context(name))?
        .ok_or_else(|| Box::new(runtime_error(format!("unknown toolset context `{name}`"))))
}

/// The active toolsets that the agent names, in the order of its definition. A name that is not available is left out:
/// the check reports it as a gap.
fn toolsets_of<T: ToolsetStore>(
    toolsets: &T,
    agent: &AgentDefinition,
) -> Result<Vec<Toolset>, Box<EvalAltResult>> {
    let mut found = Vec::new();
    for name in agent.toolsets() {
        if let Some(toolset) = await_store(toolsets.get_active(name))? {
            found.push(toolset);
        }
    }
    Ok(found)
}

/// Keeps the first storage failure of a lookup, to raise it once the lookup is over.
fn record_failure(failure: &Mutex<Option<String>>, message: String) {
    let mut kept = failure.lock().unwrap_or_else(PoisonError::into_inner);
    let _first = kept.get_or_insert(message);
}

/// Takes the storage failure kept by [`record_failure`], when there is one.
fn take_failure(failure: &Mutex<Option<String>>) -> Option<String> {
    failure.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Finds a context type through its store. A storage failure is kept in `failure` and the type is taken as absent.
fn find_type<Y: ContextTypeStore>(
    types: &Y,
    name: &str,
    failure: &Mutex<Option<String>>,
) -> Option<ContextType> {
    match block_on(types.get_type(name)) {
        Ok(Ok(found)) => found,
        Ok(Err(error)) => {
            record_failure(failure, error.to_string());
            None
        }
        Err(error) => {
            record_failure(failure, error.to_string());
            None
        }
    }
}

/// The gaps of the active toolsets of `agent` against `context`, as texts. Only the declared requirements are read: no
/// toolset module is evaluated. The gaps are those that the start of a work agent would refuse, through
/// [`delegable_subagents`], which returns them in its refusal.
fn gaps_of<T: ToolsetStore, Y: ContextTypeStore>(
    toolsets: &T,
    types: &Y,
    agent: &AgentDefinition,
    context: &ToolsetContext,
) -> Result<Vec<String>, Box<EvalAltResult>> {
    let found = toolsets_of(toolsets, agent)?;
    let failure = Mutex::new(None);
    let lookup = |name: &str| find_type(types, name, &failure);
    let outcome = delegable_subagents(agent, &[], &found, context, &lookup);
    if let Some(message) = take_failure(&failure) {
        return Err(Box::new(runtime_error(message)));
    }
    match outcome {
        Ok(_) => Ok(Vec::new()),
        Err(ToolsetsError::Incompatible { issues }) => Ok(issues),
        Err(error) => Err(Box::new(runtime_error(error.to_string()))),
    }
}

/// The default values declared by the toolsets, as `(toolset, parameter, value)`.
fn defaults_of(toolsets: &[Toolset]) -> Vec<(String, String, Value)> {
    toolsets
        .iter()
        .flat_map(|toolset| {
            toolset.params().values().filter_map(move |param| {
                param
                    .default()
                    .map(|value| (toolset.name().to_owned(), param.name().to_owned(), value.clone()))
            })
        })
        .collect()
}

/// A map of JSON values, each converted to its Rhai value.
fn json_map(values: &BTreeMap<String, Value>) -> Result<Map, Box<EvalAltResult>> {
    let mut map = Map::new();
    for (name, value) in values {
        let converted = rhai::serde::to_dynamic(value)?;
        let _previous = map.insert(name.as_str().into(), converted);
    }
    Ok(map)
}

/// The resolved values of a context: the parameters of each toolset, the shared ones, and the sorted names of the bound
/// roles. A reference never comes out.
fn resolved_value(resolved: &ResolvedContext) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut toolsets = Map::new();
    for (toolset, parameters) in resolved.toolsets() {
        let _previous = toolsets.insert(toolset.as_str().into(), Dynamic::from(json_map(parameters)?));
    }
    let roles: Array = resolved
        .roles()
        .iter()
        .map(|role| Dynamic::from(role.clone()))
        .collect();
    let mut result = Map::new();
    let _toolsets = result.insert("toolsets".into(), Dynamic::from(toolsets));
    let _shared = result.insert("shared".into(), Dynamic::from(json_map(resolved.shared())?));
    let _roles = result.insert("roles".into(), Dynamic::from(roles));
    Ok(Dynamic::from(result))
}

/// The `contexts` module: `check(agent, context)` lists the gaps, `resolve(agent, context)` gives the resolved values.
fn contexts_module<A, T, Y, C>(agents: Arc<A>, toolsets: Arc<T>, types: Arc<Y>, contexts: Arc<C>) -> Module
where
    A: AgentStore + 'static,
    T: ToolsetStore + 'static,
    Y: ContextTypeStore + 'static,
    C: ContextStore + 'static,
{
    let mut module = Module::new();
    let check_agents = Arc::clone(&agents);
    let check_contexts = Arc::clone(&contexts);
    let check_toolsets = Arc::clone(&toolsets);
    let _check = module.set_native_fn(
        "check",
        move |agent: ImmutableString, context: ImmutableString| -> Result<Array, Box<EvalAltResult>> {
            let definition = agent_of(check_agents.as_ref(), agent.as_str())?;
            let stored = context_of(check_contexts.as_ref(), context.as_str())?;
            let gaps = gaps_of(check_toolsets.as_ref(), types.as_ref(), &definition, &stored)?;
            Ok(gaps.into_iter().map(Dynamic::from).collect())
        },
    );
    let _resolve = module.set_native_fn(
        "resolve",
        move |agent: ImmutableString, context: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let definition = agent_of(agents.as_ref(), agent.as_str())?;
            let found = toolsets_of(toolsets.as_ref(), &definition)?;
            let stored = context_of(contexts.as_ref(), context.as_str())?;
            let resolved = stored
                .resolve(defaults_of(&found))
                .map_err(|error| Box::new(runtime_error(error.to_string())))?;
            resolved_value(&resolved)
        },
    );
    module
}

/// The `inspect` module over the inspection slot: `open_session(id)`, `open_context(agent, context)`, `close()`, `target()`.
fn inspect_module<A, C, X>(
    agents: Arc<A>,
    contexts: Arc<C>,
    sessions: Arc<X>,
    slot: Arc<InspectionSlot>,
    events: Arc<TargetEvents>,
) -> Module
where
    A: AgentStore + 'static,
    C: ContextStore + 'static,
    X: SessionStore + 'static,
{
    let mut module = Module::new();
    let open_slot = Arc::clone(&slot);
    let open_events = Arc::clone(&events);
    let _open = module.set_native_fn("open_session", move |id: ImmutableString| {
        open_session(
            sessions.as_ref(),
            open_slot.as_ref(),
            open_events.as_ref(),
            id.as_str(),
        )
    });
    let context_slot = Arc::clone(&slot);
    let _open_context = module.set_native_fn(
        "open_context",
        move |agent: ImmutableString, context: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            let _definition = agent_of(agents.as_ref(), agent.as_str())?;
            let _stored = context_of(contexts.as_ref(), context.as_str())?;
            context_slot.set(Some(InspectionTargetRef::Definition {
                agent: agent.as_str().to_owned(),
                context: context.as_str().to_owned(),
            }));
            events.opened("definition", &format!("{}/{}", agent.as_str(), context.as_str()));
            Ok(())
        },
    );
    let close_slot = Arc::clone(&slot);
    let _close = module.set_native_fn("close", move || -> Result<(), Box<EvalAltResult>> {
        close_slot.set(None);
        Ok(())
    });
    let _target = module.set_native_fn("target", move || -> Result<String, Box<EvalAltResult>> {
        Ok(describe(slot.get()))
    });
    module
}

/// The `sessions` module over the index and the store, with the redactor of the session: `list()`, `list(filter)`,
/// `functions(id)` and `errors(id)`.
fn sessions_module<I: SessionIndex + 'static, X: SessionStore + 'static>(
    index: Arc<I>,
    sessions: Arc<X>,
    redactor: Arc<Redactor>,
) -> Module {
    let mut module = Module::new();
    let all_index = Arc::clone(&index);
    let all_sessions = Arc::clone(&sessions);
    let all_redactor = Arc::clone(&redactor);
    let _list_all = module.set_native_fn("list", move || {
        list_sessions(
            all_index.as_ref(),
            all_sessions.as_ref(),
            all_redactor.as_ref(),
            &SessionFilter::new(),
        )
    });
    let function_sessions = Arc::clone(&sessions);
    let function_redactor = Arc::clone(&redactor);
    let _functions = module.set_native_fn("functions", move |id: ImmutableString| {
        functions_of(
            function_sessions.as_ref(),
            function_redactor.as_ref(),
            id.as_str(),
        )
    });
    let errors_index = Arc::clone(&index);
    let errors_sessions = Arc::clone(&sessions);
    let errors_redactor = Arc::clone(&redactor);
    let _errors = module.set_native_fn("errors", move |id: ImmutableString| {
        errors_of(
            errors_index.as_ref(),
            errors_sessions.as_ref(),
            errors_redactor.as_ref(),
            id.as_str(),
        )
    });
    let _list_filtered = module.set_native_fn("list", move |filter: Map| {
        let filter = filter_of(&filter)?;
        list_sessions(index.as_ref(), sessions.as_ref(), redactor.as_ref(), &filter)
    });
    module
}

/// Registration step of the configuration modules, for `EngineBuilder::with_registration`.
///
/// It registers the modules `contexts`, `inspect` and `sessions`. The stores are the host's, and the slot is shared with
/// the `rhai_prod_inspect` tool. `env` carries the redactor, the event sink, the clock and the identifier of the
/// configuration session; opening a target emits an [`EventKind::TargetOpened`] there.
pub fn config_registration<A, T, Y, C, I, X>(
    stores: ConfigStores<A, T, Y, C>,
    index: I,
    sessions: X,
    slot: Arc<InspectionSlot>,
    env: ConfigEnv,
) -> impl FnOnce(&mut RhaiEngine) -> ScratchpadResult<()> + 'static
where
    A: AgentStore + 'static,
    T: ToolsetStore + 'static,
    Y: ContextTypeStore + 'static,
    C: ContextStore + 'static,
    I: SessionIndex + 'static,
    X: SessionStore + 'static,
{
    let ConfigStores {
        agents,
        toolsets,
        types,
        contexts,
    } = stores;
    let agents = Arc::new(agents);
    let toolsets = Arc::new(toolsets);
    let types = Arc::new(types);
    let contexts = Arc::new(contexts);
    let index = Arc::new(index);
    let sessions = Arc::new(sessions);
    let ConfigEnv {
        redactor,
        sink,
        clock,
        session,
    } = env;
    let redactor = Arc::new(redactor);
    let events = Arc::new(TargetEvents { sink, clock, session });
    move |engine: &mut RhaiEngine| {
        let _contexts = engine.register_static_module(
            "contexts",
            Shared::new(contexts_module(
                Arc::clone(&agents),
                toolsets,
                types,
                Arc::clone(&contexts),
            )),
        );
        let _inspect = engine.register_static_module(
            "inspect",
            Shared::new(inspect_module(
                agents,
                contexts,
                Arc::clone(&sessions),
                slot,
                events,
            )),
        );
        let _sessions = engine.register_static_module(
            "sessions",
            Shared::new(sessions_module(index, sessions, redactor)),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vynil_core::Semver;

    use super::*;
    use crate::agents::{AgentKind, InMemoryCatalog};
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        Grants, RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits,
    };
    use crate::session::{
        AgentRef, CancelHandle, History, InMemoryStore, Message, RecordingSink, RunSignals, SessionError,
        TaggedValue,
    };
    use crate::toolsets::{
        ContextLevel, InMemoryToolsets, ParamDecl, RoleSpec, ToolsetsResult, TypeRequirement,
    };

    /// The stores, the index and the shared state of a configuration pad under test. The three toolset stores are separate
    /// instances: each test fills the one it reads.
    struct Host<I, X> {
        agents: InMemoryCatalog,
        toolsets: InMemoryToolsets,
        types: InMemoryToolsets,
        contexts: InMemoryToolsets,
        index: I,
        sessions: X,
        slot: Arc<InspectionSlot>,
        redactor: Redactor,
        sink: Arc<RecordingSink>,
    }

    impl<I: SessionIndex + 'static, X: SessionStore + 'static> Host<I, X> {
        /// A host with empty stores, an empty slot and a redactor that masks nothing.
        fn new(index: I, sessions: X) -> Self {
            Self {
                agents: InMemoryCatalog::new(),
                toolsets: InMemoryToolsets::new(1_000),
                types: InMemoryToolsets::new(1_000),
                contexts: InMemoryToolsets::new(1_000),
                index,
                sessions,
                slot: Arc::new(InspectionSlot::new()),
                redactor: Redactor::new(),
                sink: Arc::new(RecordingSink::new()),
            }
        }

        /// The configuration pad of this host.
        fn pad(self) -> Scratchpad {
            let stores = ConfigStores::new(self.agents, self.toolsets, self.types, self.contexts);
            let env = test_env(self.redactor, Arc::clone(&self.sink));
            build_pad(stores, self.index, self.sessions, self.slot, env)
        }
    }

    /// An index that lists the given identifiers whatever the filter, and keeps the given script errors.
    struct Index {
        ids: Vec<SessionId>,
        errors: Vec<ScriptError>,
    }

    impl Index {
        /// An index that lists `ids` and has no script error.
        fn listing(ids: Vec<SessionId>) -> Self {
            Self {
                ids,
                errors: Vec::new(),
            }
        }
    }

    impl SessionIndex for Index {
        fn list(
            &self,
            _filter: &SessionFilter,
        ) -> impl Future<Output = SessionResult<Vec<SessionId>>> + Send {
            std::future::ready(Ok(self.ids.clone()))
        }

        fn script_errors(
            &self,
            _id: &SessionId,
        ) -> impl Future<Output = SessionResult<Vec<ScriptError>>> + Send {
            std::future::ready(Ok(self.errors.clone()))
        }
    }

    /// A session store whose storage always fails.
    struct BrokenStore;

    impl SessionStore for BrokenStore {
        fn load(&self, _id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send {
            std::future::ready(Err(SessionError::Store("disk gone".to_owned())))
        }

        fn save(&self, _state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send {
            std::future::ready(Err(SessionError::Store("disk gone".to_owned())))
        }
    }

    /// Runs a script in the pad with a fresh clock and a handle that is not cancelled.
    async fn run(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("config-agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    /// A work session with no content.
    fn past_session(id: &str, agent: &str) -> SessionState {
        SessionState::new(
            SessionId::new(id).expect("a session id"),
            AgentRef::new(agent, "fingerprint"),
            "ctx",
            BTreeMap::new(),
            Timestamp::from_millis(0),
        )
    }

    /// The state with some of its fields replaced, through the JSON format it is read with.
    fn edited(state: &SessionState, edit: impl FnOnce(&mut Value)) -> SessionState {
        let text = state.to_json().expect("the state serializes");
        let mut value: Value = serde_json::from_str(&text).expect("the state is JSON");
        edit(&mut value);
        SessionState::from_json(&value.to_string()).expect("the edited state reads back")
    }

    /// A session store holding the given states.
    async fn store_with(states: Vec<SessionState>) -> InMemoryStore {
        let store = InMemoryStore::new();
        for state in &states {
            store.save(state).await.expect("the state is saved");
        }
        store
    }

    fn version(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    /// A work agent that lists the toolsets `toolsets`.
    fn work_agent(id: &str, toolsets: &[&str]) -> AgentDefinition {
        AgentDefinition::new(
            id.to_owned(),
            AgentKind::Work,
            "You are a work agent.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_toolsets(toolsets.iter().map(|name| (*name).to_owned()).collect())
    }

    /// The `cluster` type of version 1.0.0: the parameter `namespace` and the role `k8s`, both required.
    fn cluster_type() -> ContextType {
        ContextType::new(
            "cluster".to_owned(),
            version("1.0.0"),
            json!({
                "type": "object",
                "required": ["namespace"],
                "properties": { "namespace": { "type": "string" } }
            }),
        )
        .expect("the sample type builds")
        .with_role(RoleSpec::new("k8s".to_owned(), true))
        .expect("the role is added")
    }

    /// A toolset `grafana` that requires the type `cluster`. Its source throws when it is evaluated: a check that ran the
    /// module would fail.
    fn grafana_requiring_cluster() -> Toolset {
        Toolset::new(
            "grafana".to_owned(),
            "Grafana queries".to_owned(),
            version("1.0.0"),
            "throw \"the toolset module was evaluated\";".to_owned(),
        )
        .expect("the toolset builds")
        .with_type_requirement(TypeRequirement::parse("cluster@1.0").expect("the requirement parses"))
        .expect("the requirement is added")
    }

    /// A context with no parameter and no role.
    fn empty_context(name: &str) -> ToolsetContext {
        ToolsetContext::new(name.to_owned(), ContextLevel::Project).expect("a context")
    }

    // Scenario: check signale les écarts sans exécuter
    #[tokio::test(flavor = "multi_thread")]
    async fn check_reports_gaps_without_executing() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.types
            .put_type(&cluster_type())
            .await
            .expect("the type is stored");
        host.toolsets
            .put_draft(&grafana_requiring_cluster())
            .await
            .expect("the draft is stored");
        host.toolsets.activate("grafana").expect("the draft is activated");
        host.contexts
            .put_context(&empty_context("prod"))
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &["grafana"]))
            .await
            .expect("the agent is stored");
        let outcome = run(&host.pad(), r#"contexts::check("work-agent", "prod")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds, the module was not evaluated: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("requires role `k8s` of context type `cluster`"),
            "the missing role is listed: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("requires parameter `namespace` of context type `cluster`"),
            "the missing parameter is listed: {}",
            outcome.text()
        );
    }

    // Not a Scenario: a compatible context gives no gap.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_of_a_compatible_context_gives_no_gap() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.types
            .put_type(&cluster_type())
            .await
            .expect("the type is stored");
        host.toolsets
            .put_draft(&grafana_requiring_cluster())
            .await
            .expect("the draft is stored");
        host.toolsets.activate("grafana").expect("the draft is activated");
        let context = empty_context("prod")
            .with_parameter("namespace".to_owned(), json!("apps"))
            .expect("the parameter is added")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("the role is bound");
        host.contexts
            .put_context(&context)
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &["grafana"]))
            .await
            .expect("the agent is stored");
        let outcome = run(&host.pad(), r#"contexts::check("work-agent", "prod")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("requires"),
            "no gap is listed: {}",
            outcome.text()
        );
    }

    // Not a Scenario: a toolset that the agent names and that is not available is a gap.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_reports_an_unavailable_toolset() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.contexts
            .put_context(&empty_context("prod"))
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &["absent"]))
            .await
            .expect("the agent is stored");
        let outcome = run(&host.pad(), r#"contexts::check("work-agent", "prod")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("toolset `absent` is not available"),
            "the unavailable toolset is listed: {}",
            outcome.text()
        );
    }

    // Not a Scenario: `contexts::check` names an unknown agent.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_names_an_unknown_agent() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let outcome = run(&host.pad(), r#"contexts::check("missing-agent", "prod")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("missing-agent"),
            "the error names the agent: {}",
            outcome.text()
        );
    }

    // Not a Scenario: `contexts::resolve` gives the values, the role names and the defaults of the agent's toolsets, never
    // a reference.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_returns_values_and_role_names_only() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let context = empty_context("ctx")
            .with_parameter("region".to_owned(), json!("eu-west"))
            .expect("a parameter")
            .with_role("grafana".to_owned(), "prod-grafana-reference".to_owned())
            .expect("a role");
        host.contexts
            .put_context(&context)
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let outcome = run(&host.pad(), r#"contexts::resolve("work-agent", "ctx")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("eu-west"),
            "the value is resolved: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("grafana"),
            "the role name comes out: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("prod-grafana-reference"),
            "the reference never comes out: {}",
            outcome.text()
        );
    }

    // Not a Scenario: the defaults of the toolsets of the agent apply, and a context parameter wins over a default.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_applies_the_agent_defaults() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let grafana = Toolset::new(
            "grafana".to_owned(),
            "Grafana queries".to_owned(),
            version("1.0.0"),
            "fn query() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_param(
            ParamDecl::new("region".to_owned(), json!({ "type": "string" })).with_default(json!("eu-west")),
        )
        .expect("the parameter is declared")
        .with_param(
            ParamDecl::new("timeout".to_owned(), json!({ "type": "integer" })).with_default(json!(30)),
        )
        .expect("the parameter is declared");
        host.toolsets
            .put_draft(&grafana)
            .await
            .expect("the draft is stored");
        host.toolsets.activate("grafana").expect("the draft is activated");
        let context = empty_context("ctx")
            .with_parameter("region".to_owned(), json!("us-east"))
            .expect("a parameter");
        host.contexts
            .put_context(&context)
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &["grafana"]))
            .await
            .expect("the agent is stored");
        let outcome = run(&host.pad(), r#"contexts::resolve("work-agent", "ctx")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("timeout"),
            "the default of the toolset applies: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("us-east"),
            "the context parameter is resolved: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("eu-west"),
            "the context parameter wins over the default: {}",
            outcome.text()
        );
    }

    // Not a Scenario: `contexts::resolve` names an unknown agent.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_names_an_unknown_agent() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.contexts
            .put_context(&empty_context("ctx"))
            .await
            .expect("the context is stored");
        let outcome = run(&host.pad(), r#"contexts::resolve("missing-agent", "ctx")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("missing-agent"),
            "the error names the agent: {}",
            outcome.text()
        );
    }

    // Not a Scenario: `inspect::open_context` poses the definition as the inspection target.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_context_targets_the_definition() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.contexts
            .put_context(&empty_context("ctx"))
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let slot = Arc::clone(&host.slot);
        let outcome = run(
            &host.pad(),
            r#"inspect::open_context("work-agent", "ctx"); inspect::target()"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("definition of agent `work-agent` read with context `ctx`"),
            "the target describes the definition: {}",
            outcome.text()
        );
        assert_eq!(
            slot.get(),
            Some(InspectionTargetRef::Definition {
                agent: "work-agent".to_owned(),
                context: "ctx".to_owned(),
            }),
            "the slot holds the definition"
        );
    }

    // Scenario (config.sdd #3): opening a definition emits TargetOpened once the target is posed.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_context_emits_target_opened() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.contexts
            .put_context(&empty_context("ctx"))
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let sink = Arc::clone(&host.sink);
        let outcome = run(&host.pad(), r#"inspect::open_context("work-agent", "ctx")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert_eq!(
            sink.events(),
            vec![Event::new(
                config_session(),
                Timestamp::from_millis(42),
                EventKind::TargetOpened {
                    kind: "definition".to_owned(),
                    name: "work-agent/ctx".to_owned(),
                },
            )],
            "one event, dated by the injected clock, in the configuration session"
        );
    }

    // Scenario (config.sdd #3): opening a session emits TargetOpened with its identifier.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_session_emits_target_opened() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let host = Host::new(Index::listing(Vec::new()), sessions);
        let sink = Arc::clone(&host.sink);
        let outcome = run(&host.pad(), r#"inspect::open_session("past-1")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert_eq!(
            sink.events(),
            vec![Event::new(
                config_session(),
                Timestamp::from_millis(42),
                EventKind::TargetOpened {
                    kind: "session".to_owned(),
                    name: "past-1".to_owned(),
                },
            )],
            "one event naming the session"
        );
    }

    /// An event kind with the inspection target seen in the slot when it was emitted.
    type Seen = (EventKind, Option<InspectionTargetRef>);

    /// A sink that records, at each emission, the inspection target the shared slot holds at that moment.
    struct SlotWatchingSink {
        slot: Arc<InspectionSlot>,
        seen: Mutex<Vec<Seen>>,
    }

    impl EventSink for SlotWatchingSink {
        fn emit(&self, event: Event) {
            let target = self.slot.get();
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((event.kind, target));
        }
    }

    /// The configuration pad of `host`, whose events go to `sink` instead of the host's recording sink.
    fn pad_with_sink<I: SessionIndex + 'static, X: SessionStore + 'static>(
        host: Host<I, X>,
        sink: Arc<dyn EventSink>,
    ) -> Scratchpad {
        let stores = ConfigStores::new(host.agents, host.toolsets, host.types, host.contexts);
        let env = ConfigEnv::new(
            host.redactor,
            sink,
            Arc::new(ManualClock::new(Timestamp::from_millis(42))),
            config_session(),
        );
        build_pad(stores, host.index, host.sessions, host.slot, env)
    }

    // Scenario (config.sdd #3): the event is emitted once the target is posed, for both openings.
    #[tokio::test(flavor = "multi_thread")]
    async fn target_opened_is_emitted_after_the_target_is_posed() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let host = Host::new(Index::listing(Vec::new()), sessions);
        host.contexts
            .put_context(&empty_context("ctx"))
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let sink = Arc::new(SlotWatchingSink {
            slot: Arc::clone(&host.slot),
            seen: Mutex::new(Vec::new()),
        });
        let dyn_sink: Arc<dyn EventSink> = Arc::<SlotWatchingSink>::clone(&sink);
        let pad = pad_with_sink(host, dyn_sink);
        let context = run(&pad, r#"inspect::open_context("work-agent", "ctx")"#).await;
        assert_eq!(context.status(), RunStatus::Ok, "{}", context.text());
        let session = run(&pad, r#"inspect::open_session("past-1")"#).await;
        assert_eq!(session.status(), RunStatus::Ok, "{}", session.text());
        let seen = sink.seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(
            seen,
            vec![
                (
                    EventKind::TargetOpened {
                        kind: "definition".to_owned(),
                        name: "work-agent/ctx".to_owned(),
                    },
                    Some(InspectionTargetRef::Definition {
                        agent: "work-agent".to_owned(),
                        context: "ctx".to_owned(),
                    }),
                ),
                (
                    EventKind::TargetOpened {
                        kind: "session".to_owned(),
                        name: "past-1".to_owned(),
                    },
                    Some(InspectionTargetRef::Session(
                        SessionId::new("past-1").expect("a session id")
                    )),
                ),
            ],
            "each event sees the target already posed"
        );
    }

    // Scenario (config.sdd #3): a definition then a session, in one script, emit in that order.
    #[tokio::test(flavor = "multi_thread")]
    async fn definition_then_session_events_keep_their_order() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let host = Host::new(Index::listing(Vec::new()), sessions);
        host.contexts
            .put_context(&empty_context("ctx"))
            .await
            .expect("the context is stored");
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let sink = Arc::clone(&host.sink);
        let outcome = run(
            &host.pad(),
            r#"inspect::open_context("work-agent", "ctx"); inspect::open_session("past-1")"#,
        )
        .await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        let kinds: Vec<EventKind> = sink.events().iter().map(|event| event.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::TargetOpened {
                    kind: "definition".to_owned(),
                    name: "work-agent/ctx".to_owned(),
                },
                EventKind::TargetOpened {
                    kind: "session".to_owned(),
                    name: "past-1".to_owned(),
                },
            ],
            "definition first, then session"
        );
    }

    // Scenario (config.sdd #3): a refused session opening names the identifier and emits nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn refused_session_opening_names_the_identifier() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let sink = Arc::clone(&host.sink);
        let outcome = run(&host.pad(), r#"inspect::open_session("missing-session")"#).await;
        assert_ne!(outcome.status(), RunStatus::Ok, "the unknown session is refused");
        assert!(
            outcome.text().contains("missing-session"),
            "the error names the identifier: {}",
            outcome.text()
        );
        assert!(sink.events().is_empty(), "no event: {:?}", sink.events());
    }

    // Scenario (config.sdd #3): a refused opening emits nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn refused_openings_emit_nothing() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let sink = Arc::clone(&host.sink);
        let pad = host.pad();
        let session = run(&pad, r#"inspect::open_session("missing-session")"#).await;
        assert_ne!(session.status(), RunStatus::Ok, "the unknown session is refused");
        let context = run(&pad, r#"inspect::open_context("work-agent", "missing")"#).await;
        assert_ne!(context.status(), RunStatus::Ok, "the unknown context is refused");
        let agent = run(&pad, r#"inspect::open_context("missing", "ctx")"#).await;
        assert_ne!(agent.status(), RunStatus::Ok, "the unknown agent is refused");
        assert!(
            sink.events().is_empty(),
            "no event for a refused opening: {:?}",
            sink.events()
        );
    }

    // Not a Scenario: `ConfigEnv` is opaque in `Debug`.
    #[test]
    fn config_env_debug_is_opaque() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123456").expect("the secret is accepted");
        let env = test_env(redactor, Arc::new(RecordingSink::new()));
        let text = format!("{env:?}");
        assert!(!text.contains("tok-123456"), "no secret in Debug: {text}");
        assert!(text.contains("ConfigEnv"), "{text}");
    }

    // Not a Scenario: `inspect::open_context` names an unknown context and leaves the target alone.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_context_names_the_unknown_context() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        let slot = Arc::clone(&host.slot);
        slot.set(Some(InspectionTargetRef::Session(
            SessionId::new("past-1").expect("a session id"),
        )));
        let outcome = run(
            &host.pad(),
            r#"inspect::open_context("work-agent", "missing-context")"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("missing-context"),
            "the error names the context: {}",
            outcome.text()
        );
        assert_eq!(
            slot.get(),
            Some(InspectionTargetRef::Session(
                SessionId::new("past-1").expect("a session id")
            )),
            "the target is unchanged"
        );
    }

    // Not a Scenario: `inspect::open_context` names an unknown agent.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_context_names_the_unknown_agent() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        host.contexts
            .put_context(&empty_context("ctx"))
            .await
            .expect("the context is stored");
        let slot = Arc::clone(&host.slot);
        let outcome = run(&host.pad(), r#"inspect::open_context("missing-agent", "ctx")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("missing-agent"),
            "the error names the agent: {}",
            outcome.text()
        );
        assert_eq!(slot.get(), None, "an unknown agent leaves the target alone");
    }

    // Scenario: l'ouverture d'une cible remplace la précédente
    #[tokio::test(flavor = "multi_thread")]
    async fn opening_a_target_replaces_the_previous_one() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let host = Host::new(Index::listing(Vec::new()), sessions);
        let slot = Arc::clone(&host.slot);
        slot.set(Some(InspectionTargetRef::Definition {
            agent: "work-agent".to_owned(),
            context: "ctx".to_owned(),
        }));
        let outcome = run(
            &host.pad(),
            r#"inspect::open_session("past-1"); inspect::target()"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("session `past-1`"),
            "the target describes the session: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("definition"),
            "the previous definition is gone: {}",
            outcome.text()
        );
        assert_eq!(
            slot.get(),
            Some(InspectionTargetRef::Session(
                SessionId::new("past-1").expect("a session id")
            )),
            "the slot holds the session only"
        );
    }

    // Scenario: session inconnue est une erreur explicite
    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_is_an_explicit_error() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let slot = Arc::clone(&host.slot);
        let outcome = run(&host.pad(), r#"inspect::open_session("missing-session")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("missing-session"),
            "the error names the identifier: {}",
            outcome.text()
        );
        assert_eq!(slot.get(), None, "an unknown session leaves the target alone");
    }

    // Scenario: les erreurs passées sont masquées
    #[tokio::test(flavor = "multi_thread")]
    async fn past_errors_are_masked() {
        let sessions = store_with(vec![past_session("past-5", "work-agent")]).await;
        let index = Index {
            ids: Vec::new(),
            errors: vec![ScriptError::new(
                TurnId::new(3),
                ScriptErrorClass::Runtime,
                "the call failed with tok-123456 in its arguments",
            )],
        };
        let mut host = Host::new(index, sessions);
        host.redactor
            .add_secret("tok-123456")
            .expect("a long enough secret");
        let outcome = run(&host.pad(), r#"sessions::errors("past-5")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("runtime"),
            "the class of the error is listed: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("***"),
            "the secret is masked: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("tok-123456"),
            "the secret does not reach the script: {}",
            outcome.text()
        );
    }

    // Not a Scenario: `sessions::errors` names an unknown session.
    #[tokio::test(flavor = "multi_thread")]
    async fn errors_names_an_unknown_session() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let outcome = run(&host.pad(), r#"sessions::errors("missing-session")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("missing-session"),
            "the error names the identifier: {}",
            outcome.text()
        );
    }

    // Scenario: le résumé ne porte aucun texte de message
    #[tokio::test(flavor = "multi_thread")]
    async fn summary_carries_no_message_text() {
        let state = edited(&past_session("past-2", "work-agent"), |value| {
            let mut history = History::new();
            history.push(Message::user("MESSAGE-TEXT-ALPHA"));
            value["history"] = serde_json::to_value(&history).expect("the history serializes");
            value["variables"] = json!({
                "held": serde_json::to_value(TaggedValue::String("VARIABLE-VALUE-BETA".to_owned()))
                    .expect("the value serializes"),
            });
        });
        let sessions = store_with(vec![state]).await;
        let host = Host::new(
            Index::listing(vec![SessionId::new("past-2").expect("a session id")]),
            sessions,
        );
        let outcome = run(&host.pad(), "sessions::list()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("past-2"),
            "the summary names the session: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("work-agent"),
            "the summary names the agent: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("MESSAGE-TEXT-ALPHA"),
            "the summary carries no message text: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("VARIABLE-VALUE-BETA"),
            "the summary carries no variable value: {}",
            outcome.text()
        );
    }

    // Not a Scenario: a summary counts the turns that carry signals and their Rhai errors.
    #[test]
    fn summary_counts_turns_and_rhai_errors() {
        let mut failing = RunSignals::new("work-agent", "profile-a", BTreeMap::new());
        failing.record_rhai_error();
        failing.record_rhai_error();
        let clean = RunSignals::new("work-agent", "profile-a", BTreeMap::new());
        let state = edited(&past_session("past-6", "work-agent"), |value| {
            value["signals"] = json!({
                "1": serde_json::to_value(&failing).expect("the signals serialize"),
                "2": serde_json::to_value(&clean).expect("the signals serialize"),
            });
        });
        let summary = SessionSummary::from_state(&state);
        assert_eq!(summary.turns(), 2, "two turns carry signals");
        assert_eq!(summary.rhai_errors(), 2, "the Rhai errors of the turns add up");
    }

    // Not a Scenario: a summary names each function once, without its arity, and carries no source text.
    #[test]
    fn summary_names_each_function_once_without_arity() {
        let state = edited(&past_session("past-4", "work-agent"), |value| {
            value["functions"] = json!({
                "fetch/0": "fn fetch() { 1 }",
                "fetch/1": "fn fetch(x) { x }",
                "other/0": "fn other() { 2 }",
            });
        });
        let summary = SessionSummary::from_state(&state);
        assert_eq!(
            summary.functions(),
            ["fetch".to_owned(), "other".to_owned()],
            "one name per function"
        );
        assert_eq!(summary.agent(), "work-agent", "the agent of the session");
        assert_eq!(summary.kind(), SessionKind::Work, "the nature of the session");
        assert!(
            !format!("{summary:?}").contains("fn fetch"),
            "no source text in the summary"
        );
    }

    // Scenario: aucun module n'écrit
    #[test]
    fn no_module_writes() {
        let contexts = contexts_module(
            Arc::new(InMemoryCatalog::new()),
            Arc::new(InMemoryToolsets::new(1_000)),
            Arc::new(InMemoryToolsets::new(1_000)),
            Arc::new(InMemoryToolsets::new(1_000)),
        );
        let inspect = inspect_module(
            Arc::new(InMemoryCatalog::new()),
            Arc::new(InMemoryToolsets::new(1_000)),
            Arc::new(InMemoryStore::new()),
            Arc::new(InspectionSlot::new()),
            Arc::new(TargetEvents {
                sink: Arc::new(RecordingSink::new()),
                clock: Arc::new(ManualClock::new(Timestamp::from_millis(0))),
                session: config_session(),
            }),
        );
        let sessions = sessions_module(
            Arc::new(Index::listing(Vec::new())),
            Arc::new(InMemoryStore::new()),
            Arc::new(Redactor::new()),
        );
        let mut names = Vec::new();
        for (module, functions) in [
            ("contexts", contexts),
            ("inspect", inspect),
            ("sessions", sessions),
        ] {
            for signature in functions.gen_fn_signatures_with_mapper(Into::into) {
                let name = signature.split('(').next().unwrap_or_default();
                names.push(format!("{module}::{}", name.trim_start_matches("fn ").trim()));
            }
        }
        names.sort();
        assert_eq!(
            names,
            vec![
                "contexts::check",
                "contexts::resolve",
                "inspect::close",
                "inspect::open_context",
                "inspect::open_session",
                "inspect::target",
                "sessions::errors",
                "sessions::functions",
                "sessions::list",
                "sessions::list",
            ],
            "the modules hold exactly their read-only functions"
        );
    }

    // Not a Scenario: a failing storage is an explicit Rhai error that carries the store message.
    #[tokio::test(flavor = "multi_thread")]
    async fn store_failure_is_an_explicit_error() {
        let host = Host::new(Index::listing(Vec::new()), BrokenStore);
        let outcome = run(&host.pad(), r#"inspect::open_session("past-1")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("disk gone"),
            "the error carries the store message: {}",
            outcome.text()
        );
    }

    // Not a Scenario: on a `current_thread` runtime, the bridge refuses the storage call explicitly.
    #[tokio::test]
    async fn current_thread_runtime_renders_the_explicit_error() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let host = Host::new(Index::listing(Vec::new()), sessions);
        let outcome = run(&host.pad(), r#"inspect::open_session("past-1")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("multi-thread"),
            "the explicit bridge error is rendered: {}",
            outcome.text()
        );
    }

    // Not a Scenario: an unknown key of the filter is refused and named.
    #[tokio::test(flavor = "multi_thread")]
    async fn filter_refuses_an_unknown_key() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let outcome = run(&host.pad(), r#"sessions::list(#{ agnet: "work-agent" })"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("agnet"),
            "the error names the key: {}",
            outcome.text()
        );
    }

    // Not a Scenario: the source texts of the functions are masked by the redactor of the session.
    #[tokio::test(flavor = "multi_thread")]
    async fn functions_lists_the_masked_sources() {
        let state = edited(&past_session("past-3", "work-agent"), |value| {
            value["functions"] = json!({ "fetch/0": "fn fetch() { \"tok-123456\" }" });
        });
        let sessions = store_with(vec![state]).await;
        let mut host = Host::new(Index::listing(Vec::new()), sessions);
        host.redactor
            .add_secret("tok-123456")
            .expect("a long enough secret");
        let outcome = run(&host.pad(), r#"sessions::functions("past-3")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("fetch/0"),
            "the key is listed: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("***"),
            "the source is masked: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("tok-123456"),
            "the secret is gone: {}",
            outcome.text()
        );
    }

    // Not a Scenario: `inspect::close` removes the target.
    #[tokio::test(flavor = "multi_thread")]
    async fn close_removes_the_target() {
        let host = Host::new(Index::listing(Vec::new()), InMemoryStore::new());
        let slot = Arc::clone(&host.slot);
        slot.set(Some(InspectionTargetRef::Session(
            SessionId::new("past-1").expect("a session id"),
        )));
        let outcome = run(&host.pad(), "inspect::close(); inspect::target()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("no inspection target"),
            "the target is described as absent: {}",
            outcome.text()
        );
        assert_eq!(slot.get(), None, "the slot is empty");
    }

    /// The session in which the configuration pads under test run.
    fn config_session() -> SessionId {
        SessionId::new("config-session").expect("a session id")
    }

    /// The environment of a configuration pad under test: the clock stands at 42 ms.
    fn test_env(redactor: Redactor, sink: Arc<RecordingSink>) -> ConfigEnv {
        ConfigEnv::new(
            redactor,
            sink,
            Arc::new(ManualClock::new(Timestamp::from_millis(42))),
            config_session(),
        )
    }

    /// The environment of a pad whose events are not read.
    fn quiet_env() -> ConfigEnv {
        test_env(Redactor::new(), Arc::new(RecordingSink::new()))
    }

    /// Builds the configuration pad over the given stores, index, sessions, slot and environment.
    fn build_pad<A, T, Y, C, I, X>(
        stores: ConfigStores<A, T, Y, C>,
        index: I,
        sessions: X,
        slot: Arc<InspectionSlot>,
        env: ConfigEnv,
    ) -> Scratchpad
    where
        A: AgentStore + 'static,
        T: ToolsetStore + 'static,
        Y: ContextTypeStore + 'static,
        C: ContextStore + 'static,
        I: SessionIndex + 'static,
        X: SessionStore + 'static,
    {
        let registration = config_registration(stores, index, sessions, slot, env);
        match ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .with_registration(registration)
            .build()
        {
            Ok(engine) => Scratchpad::new(Arc::new(engine)),
            Err(error) => panic!("the configuration engine must build: {error}"),
        }
    }

    /// An index that lists `ids` whatever the filter, keeps no script error, and records every filter it receives. The
    /// record is shared: [`Recording::seen`] reads it once the index is moved into a pad.
    struct Recording {
        ids: Vec<SessionId>,
        seen: Arc<Mutex<Vec<SessionFilter>>>,
    }

    impl Recording {
        /// An index that lists `ids` and records the filters.
        fn listing(ids: Vec<SessionId>) -> Self {
            Self {
                ids,
                seen: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// The shared record of the filters received so far.
        fn seen(&self) -> Arc<Mutex<Vec<SessionFilter>>> {
            Arc::clone(&self.seen)
        }
    }

    impl SessionIndex for Recording {
        fn list(&self, filter: &SessionFilter) -> impl Future<Output = SessionResult<Vec<SessionId>>> + Send {
            self.seen
                .lock()
                .expect("the record is not poisoned")
                .push(filter.clone());
            std::future::ready(Ok(self.ids.clone()))
        }

        fn script_errors(
            &self,
            _id: &SessionId,
        ) -> impl Future<Output = SessionResult<Vec<ScriptError>>> + Send {
            std::future::ready(Ok(Vec::new()))
        }
    }

    /// The filters recorded in `seen`, copied out.
    fn recorded(seen: &Arc<Mutex<Vec<SessionFilter>>>) -> Vec<SessionFilter> {
        seen.lock().expect("the record is not poisoned").clone()
    }

    /// A context type store whose storage always fails.
    struct BrokenTypes;

    impl ContextTypeStore for BrokenTypes {
        fn get_type(&self, _name: &str) -> impl Future<Output = ToolsetsResult<Option<ContextType>>> + Send {
            std::future::ready(Err(ToolsetsError::Store("types gone".to_owned())))
        }

        fn put_type(&self, _context_type: &ContextType) -> impl Future<Output = ToolsetsResult<()>> + Send {
            std::future::ready(Err(ToolsetsError::Store("types gone".to_owned())))
        }
    }

    /// The `cluster` type of version 1.0.0 that includes the type `base`, which the stores do not hold.
    fn cluster_including_missing_base() -> ContextType {
        cluster_type().with_include(TypeRequirement::parse("base@1.0").expect("the requirement parses"))
    }

    /// Panics while holding the lock of the target of `slot`: the lock is poisoned.
    fn poison(slot: &InspectionSlot) {
        let _guard = slot.target.lock();
        panic!("poisoning the inspection slot on purpose");
    }

    /// The stores of a configuration pad with empty stores.
    fn empty_stores() -> ConfigStores<InMemoryCatalog, InMemoryToolsets, InMemoryToolsets, InMemoryToolsets> {
        ConfigStores::new(
            InMemoryCatalog::new(),
            InMemoryToolsets::new(1_000),
            InMemoryToolsets::new(1_000),
            InMemoryToolsets::new(1_000),
        )
    }

    // Not a Scenario: the Debug of a script error shows its turn and its class, never its text.
    #[test]
    fn script_error_debug_never_shows_the_text() {
        let error = ScriptError::new(
            TurnId::new(4),
            ScriptErrorClass::Limit,
            "the call failed with tok-123456 in its arguments",
        );
        let shown = format!("{error:?}");
        assert!(!shown.contains("tok-123456"), "the secret is not shown: {shown}");
        assert!(
            !shown.contains("the call failed"),
            "the text is not shown: {shown}"
        );
        assert!(
            shown.contains(&format!("{:?}", TurnId::new(4))),
            "the turn is shown: {shown}"
        );
        assert!(shown.contains("Limit"), "the class is shown: {shown}");
    }

    // Not a Scenario: a script error keeps its turn, its class and its text, reachable through its accessors.
    #[test]
    fn script_error_accessors_give_back_what_was_given() {
        let error = ScriptError::new(TurnId::new(4), ScriptErrorClass::Limit, "too many operations");
        assert_eq!(error.turn(), TurnId::new(4), "the turn");
        assert_eq!(error.class(), ScriptErrorClass::Limit, "the class");
        assert_eq!(error.text(), "too many operations", "the text, not masked");
    }

    // Not a Scenario: the configuration stores are opaque: their Debug shows no store.
    #[test]
    fn config_stores_debug_is_opaque() {
        let stores = ConfigStores::new(
            "AGENT-STORE-SECRET".to_owned(),
            "TOOLSET-STORE-SECRET".to_owned(),
            "TYPE-STORE-SECRET".to_owned(),
            "CONTEXT-STORE-SECRET".to_owned(),
        );
        let shown = format!("{stores:?}");
        for hidden in [
            "AGENT-STORE-SECRET",
            "TOOLSET-STORE-SECRET",
            "TYPE-STORE-SECRET",
            "CONTEXT-STORE-SECRET",
        ] {
            assert!(!shown.contains(hidden), "the store is not shown: {shown}");
        }
        assert!(shown.contains("ConfigStores"), "the type is named: {shown}");
    }

    /// A store that is not `Debug`: the opaque Debug of the stores must not require one.
    struct Opaque;

    // Not a Scenario: the stores need no `Debug` of their own to be shown: the Debug of `ConfigStores` has no bound.
    #[test]
    fn config_stores_debug_needs_no_store_debug() {
        let stores = ConfigStores::new(Opaque, Opaque, Opaque, Opaque);
        assert_eq!(
            format!("{stores:?}"),
            "ConfigStores { .. }",
            "the stores are shown opaquely"
        );
    }

    // Not a Scenario: a count above the integer range of Rhai saturates to the largest integer, and is not wrapped to zero.
    #[test]
    fn count_above_the_integer_range_saturates() {
        assert_eq!(count_value(3).as_int(), Ok(3), "a small count is kept");
        assert_eq!(
            count_value(u64::MAX).as_int(),
            Ok(i64::MAX),
            "a huge count saturates"
        );
    }

    // Not a Scenario: the filter accessors give back what the builders set, and nothing by default.
    #[test]
    fn session_filter_accessors_give_back_what_was_set() {
        let empty = SessionFilter::new();
        assert_eq!(empty.agent(), None, "no agent by default");
        assert_eq!(empty.kind(), None, "no nature by default");
        assert_eq!(empty.limit(), None, "no limit by default");
        assert_eq!(SessionFilter::default(), empty, "the default is the empty filter");
        let filter = SessionFilter::new()
            .with_agent("work-agent")
            .with_kind(SessionKind::Sub)
            .with_limit(2);
        assert_eq!(filter.agent(), Some("work-agent"), "the agent");
        assert_eq!(filter.kind(), Some(SessionKind::Sub), "the nature");
        assert_eq!(filter.limit(), Some(2), "the limit");
    }

    // Not a Scenario: agent, nature and limit reach the index, as the filter it receives.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_hands_the_filter_to_the_index() {
        let index = Recording::listing(Vec::new());
        let seen = index.seen();
        let pad = Host::new(index, InMemoryStore::new()).pad();
        let outcome = run(
            &pad,
            r#"sessions::list(#{ agent: "a1", nature: "config", limit: 3 })"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert_eq!(
            recorded(&seen),
            vec![
                SessionFilter::new()
                    .with_agent("a1")
                    .with_kind(SessionKind::Config)
                    .with_limit(3)
            ],
            "the index receives the filter of the script"
        );
    }

    // Not a Scenario: `sessions::list()` without argument hands the empty filter to the index.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_without_argument_hands_the_empty_filter() {
        let index = Recording::listing(Vec::new());
        let seen = index.seen();
        let pad = Host::new(index, InMemoryStore::new()).pad();
        let outcome = run(&pad, "sessions::list()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert_eq!(
            recorded(&seen),
            vec![SessionFilter::new()],
            "the index receives the empty filter"
        );
    }

    // Not a Scenario: every nature name is accepted, and each reaches the index as its nature.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_accepts_every_nature_name() {
        let index = Recording::listing(Vec::new());
        let seen = index.seen();
        let pad = Host::new(index, InMemoryStore::new()).pad();
        for name in ["work", "config", "sub"] {
            let script = format!(r#"sessions::list(#{{ nature: "{name}" }})"#);
            let outcome = run(&pad, &script).await;
            assert_eq!(
                outcome.status(),
                RunStatus::Ok,
                "{name} is accepted: {}",
                outcome.text()
            );
        }
        let natures: Vec<Option<SessionKind>> = recorded(&seen).iter().map(SessionFilter::kind).collect();
        assert_eq!(
            natures,
            vec![
                Some(SessionKind::Work),
                Some(SessionKind::Config),
                Some(SessionKind::Sub)
            ],
            "each nature reaches the index"
        );
    }

    // Not a Scenario: a bad filter is refused and named before the index is asked anything.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_refuses_a_bad_filter_before_listing() {
        let index = Recording::listing(Vec::new());
        let seen = index.seen();
        let pad = Host::new(index, InMemoryStore::new()).pad();
        for (script, fragment) in [
            (r#"sessions::list(#{ agnet: "a1" })"#, "agnet"),
            (r#"sessions::list(#{ nature: "bogus" })"#, "bogus"),
            ("sessions::list(#{ limit: -1 })", "-1"),
            (r#"sessions::list(#{ limit: "3" })"#, "limit"),
            ("sessions::list(#{ agent: 4 })", "agent"),
        ] {
            let outcome = run(&pad, script).await;
            assert_eq!(
                outcome.status(),
                RunStatus::Failed(ScriptErrorClass::Runtime),
                "{script} fails: {}",
                outcome.text()
            );
            assert!(
                outcome.text().contains(fragment),
                "{script} names `{fragment}`: {}",
                outcome.text()
            );
        }
        assert!(recorded(&seen).is_empty(), "no refused filter reaches the index");
    }

    // Not a Scenario: a session of the list that the store does not hold is left out; the others keep their order.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_skips_a_session_absent_from_the_store() {
        let sessions = store_with(vec![
            past_session("past-a", "work-agent"),
            past_session("past-b", "work-agent"),
        ])
        .await;
        let index = Recording::listing(vec![
            SessionId::new("past-a").expect("a session id"),
            SessionId::new("missing").expect("a session id"),
            SessionId::new("past-b").expect("a session id"),
        ]);
        let pad = Host::new(index, sessions).pad();
        let outcome = run(
            &pad,
            r#"let found = sessions::list(); found.len().to_string() + " " + found[0].id + " " + found[1].id"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("2 past-a past-b"),
            "two sessions, in the order of the index: {}",
            outcome.text()
        );
    }

    // Not a Scenario: the summary gives the nature of each session: config and sub, as well as work.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_gives_the_nature_of_each_session() {
        let config = past_session("cfg-1", "config-agent").with_kind(SessionKind::Config);
        let sub = past_session("sub-1", "sub-agent").with_kind(SessionKind::Sub);
        let sessions = store_with(vec![config, sub]).await;
        let index = Recording::listing(vec![
            SessionId::new("cfg-1").expect("a session id"),
            SessionId::new("sub-1").expect("a session id"),
        ]);
        let pad = Host::new(index, sessions).pad();
        let outcome = run(
            &pad,
            r#"let found = sessions::list(); found[0].nature + " " + found[1].nature"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("config sub"),
            "the natures are config then sub: {}",
            outcome.text()
        );
    }

    // Not a Scenario: the summary masks the identifier, the agent and the function names with the redactor.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_masks_identifier_agent_and_function_names() {
        let state = edited(&past_session("s-tok-123456", "agent-tok-123456"), |value| {
            value["functions"] = json!({ "fetch-tok-123456/0": "fn fetch() { 1 }" });
        });
        let sessions = store_with(vec![state]).await;
        let index = Recording::listing(vec![SessionId::new("s-tok-123456").expect("a session id")]);
        let mut host = Host::new(index, sessions);
        host.redactor
            .add_secret("tok-123456")
            .expect("a long enough secret");
        let pad = host.pad();
        let outcome = run(&pad, "sessions::list()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("***"),
            "the secret is masked: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("tok-123456"),
            "no secret in the summary: {}",
            outcome.text()
        );
    }

    // Not a Scenario: the keys of `functions` are masked as well as the sources.
    #[tokio::test(flavor = "multi_thread")]
    async fn functions_masks_the_keys() {
        let state = edited(&past_session("past-8", "work-agent"), |value| {
            value["functions"] = json!({ "fetch-tok-123456/0": "fn fetch() { 1 }" });
        });
        let sessions = store_with(vec![state]).await;
        let mut host = Host::new(Index::listing(Vec::new()), sessions);
        host.redactor
            .add_secret("tok-123456")
            .expect("a long enough secret");
        let pad = host.pad();
        let outcome = run(&pad, r#"sessions::functions("past-8")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("***"),
            "the key is masked: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("tok-123456"),
            "no secret in the keys: {}",
            outcome.text()
        );
    }

    // Not a Scenario: the errors give the turn and the class of each script error, in the order of the index.
    #[tokio::test(flavor = "multi_thread")]
    async fn errors_give_the_turn_and_the_class_of_each_error() {
        let sessions = store_with(vec![past_session("past-9", "work-agent")]).await;
        let index = Index {
            ids: Vec::new(),
            errors: vec![
                ScriptError::new(TurnId::new(3), ScriptErrorClass::Limit, "too many operations"),
                ScriptError::new(TurnId::new(5), ScriptErrorClass::Syntax, "unexpected token"),
                ScriptError::new(
                    TurnId::new(7),
                    ScriptErrorClass::NotFound,
                    "no function named fetch",
                ),
                ScriptError::new(TurnId::new(9), ScriptErrorClass::Cancelled, "the run was stopped"),
                ScriptError::new(TurnId::new(11), ScriptErrorClass::Engine, "the engine failed"),
                ScriptError::new(TurnId::new(13), ScriptErrorClass::Internal, "an internal failure"),
            ],
        };
        let pad = Host::new(index, sessions).pad();
        let outcome = run(
            &pad,
            r#"let found = sessions::errors("past-9"); let text = ""; for error in found { text += error.turn.to_string() + " " + error.class + ";"; } text"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("3 limit;5 syntax;7 not_found;9 cancelled;11 engine;13 internal;"),
            "turn and class of each error, every class: {}",
            outcome.text()
        );
    }

    // Not a Scenario: an empty session identifier is no identifier: the functions that take one name it as unknown.
    #[tokio::test(flavor = "multi_thread")]
    async fn empty_session_identifier_is_an_unknown_session() {
        let pad = Host::new(Index::listing(Vec::new()), InMemoryStore::new()).pad();
        let outcome = run(&pad, r#"inspect::open_session("")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the run fails at run time: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("unknown session"),
            "the error names the unknown session: {}",
            outcome.text()
        );
    }

    // Not a Scenario: on a `current_thread` runtime, every storage-backed function refuses explicitly and opens nothing.
    #[tokio::test]
    async fn current_thread_runtime_refuses_every_config_function() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let stores = empty_stores();
        stores
            .agents
            .put_agent(&work_agent("work-agent", &[]))
            .await
            .expect("the agent is stored");
        stores
            .contexts
            .put_context(&empty_context("prod"))
            .await
            .expect("the context is stored");
        let slot = Arc::new(InspectionSlot::new());
        let index = Recording::listing(vec![SessionId::new("past-1").expect("a session id")]);
        let pad = build_pad(stores, index, sessions, Arc::clone(&slot), quiet_env());
        for script in [
            r#"contexts::check("work-agent", "prod")"#,
            r#"contexts::resolve("work-agent", "prod")"#,
            r#"inspect::open_context("work-agent", "prod")"#,
            r#"inspect::open_session("past-1")"#,
            "sessions::list()",
            r#"sessions::errors("past-1")"#,
            r#"sessions::functions("past-1")"#,
        ] {
            let outcome = run(&pad, script).await;
            assert_eq!(
                outcome.status(),
                RunStatus::Failed(ScriptErrorClass::Runtime),
                "{script} fails at run time: {}",
                outcome.text()
            );
            assert!(
                outcome.text().contains("multi-thread"),
                "{script} names the runtime it needs: {}",
                outcome.text()
            );
        }
        assert_eq!(slot.get(), None, "no inspection target was opened");
    }

    // Not a Scenario: a poisoned inspection slot keeps working: its target is read, replaced and removed.
    #[test]
    fn poisoned_slot_keeps_working() {
        let slot = Arc::new(InspectionSlot::new());
        let poisoner = Arc::clone(&slot);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || poison(&poisoner)));
        assert!(outcome.is_err(), "the poisoning panic was caught");
        assert!(slot.target.is_poisoned(), "the slot is poisoned");
        let target = InspectionTargetRef::Session(SessionId::new("p-1").expect("a session id"));
        slot.set(Some(target.clone()));
        assert_eq!(slot.get(), Some(target), "set and get work on a poisoned slot");
        slot.set(None);
        assert_eq!(slot.get(), None, "a poisoned slot can be cleared");
    }

    // Not a Scenario: the inspection pad works over a poisoned slot: `inspect::target()` reads it, and the functions write it.
    #[tokio::test(flavor = "multi_thread")]
    async fn poisoned_slot_serves_the_inspection_pad() {
        let sessions = store_with(vec![past_session("past-1", "work-agent")]).await;
        let slot = Arc::new(InspectionSlot::new());
        let poisoner = Arc::clone(&slot);
        let _poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || poison(&poisoner)));
        let pad = build_pad(
            empty_stores(),
            Recording::listing(Vec::new()),
            sessions,
            Arc::clone(&slot),
            quiet_env(),
        );
        let opened = run(&pad, r#"inspect::open_session("past-1"); inspect::target()"#).await;
        assert_eq!(
            opened.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            opened.text()
        );
        assert!(
            opened.text().contains("session `past-1`"),
            "the target is read over the poisoned slot: {}",
            opened.text()
        );
        let closed = run(&pad, "inspect::close(); inspect::target()").await;
        assert_eq!(
            closed.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            closed.text()
        );
        assert!(
            closed.text().contains("no inspection target"),
            "the target is removed over the poisoned slot: {}",
            closed.text()
        );
    }

    // Not a Scenario: a failing context type store is an explicit error of the check, never a gap that it would invent.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_names_a_failing_type_store_instead_of_inventing_a_gap() {
        let stores = empty_stores();
        stores
            .agents
            .put_agent(&work_agent("work-agent", &["grafana"]))
            .await
            .expect("the agent is stored");
        stores
            .toolsets
            .put_draft(&grafana_requiring_cluster())
            .await
            .expect("the draft is stored");
        stores
            .toolsets
            .activate("grafana")
            .expect("the draft is activated");
        stores
            .contexts
            .put_context(&empty_context("prod"))
            .await
            .expect("the context is stored");
        let stores = ConfigStores::new(stores.agents, stores.toolsets, BrokenTypes, stores.contexts);
        let pad = build_pad(
            stores,
            Recording::listing(Vec::new()),
            InMemoryStore::new(),
            Arc::new(InspectionSlot::new()),
            quiet_env(),
        );
        let outcome = run(&pad, r#"contexts::check("work-agent", "prod")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the check fails: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("types gone"),
            "the storage message comes out: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("requires"),
            "no gap is invented: {}",
            outcome.text()
        );
    }

    // Not a Scenario: a context type that includes a missing type is an error of the check, not a gap.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_refuses_an_inconsistent_type_as_an_error() {
        let stores = empty_stores();
        stores
            .agents
            .put_agent(&work_agent("work-agent", &["grafana"]))
            .await
            .expect("the agent is stored");
        stores
            .types
            .put_type(&cluster_including_missing_base())
            .await
            .expect("the type is stored");
        stores
            .toolsets
            .put_draft(&grafana_requiring_cluster())
            .await
            .expect("the draft is stored");
        stores
            .toolsets
            .activate("grafana")
            .expect("the draft is activated");
        stores
            .contexts
            .put_context(&empty_context("prod"))
            .await
            .expect("the context is stored");
        let pad = build_pad(
            stores,
            Recording::listing(Vec::new()),
            InMemoryStore::new(),
            Arc::new(InspectionSlot::new()),
            quiet_env(),
        );
        let outcome = run(&pad, r#"contexts::check("work-agent", "prod")"#).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "the check fails: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("invalid context type"),
            "the refusal of the type comes out: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("requires"),
            "no gap is invented: {}",
            outcome.text()
        );
    }
}
