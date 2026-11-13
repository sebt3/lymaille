//! Rhai modules of the delegation domain: `delegate` for the main agent (list, start, follow, wait, cancel, capacity) and
//! `result` for the sub-agent (submit, fail), with the proof `MainAgent` that only a work definition gives
//! (`agent-sessions/delegation#FR-1`, `FR-2`, `FR-4`, `FR-6`, `FR-11`, `FR-13`, `AC-1`, `AC-5`, `AC-7`). See `./module.sdd`.

use std::sync::Arc;

use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Array, Dynamic, EvalAltResult, ImmutableString, Map, Module, Position};
use serde_json::Value;
use tokio::runtime::Handle;
use vynil_core::rt::block_on;

use super::{
    DelegationError, DelegationResult, InferenceLimiter, ResultCollector, RunProgress, RunRegistry,
    SubagentLauncher, SubagentOutcome, SubagentStatus, Submission,
};
use crate::agents::{AgentDefinition, AgentKind};
use crate::session::{CancelHandle, Redactor, RunId};

/// Proof that a session is a work session, and so may delegate: the only way to build the `delegate` module.
///
/// Only a definition of nature `Work` gives it. A sub-agent or a configuration agent gets none, so no code path builds the
/// `delegate` module for them (`agent-sessions/delegation#FR-2`).
///
/// The proof is consumed by [`delegate_module`]: it is not `Clone`, so one definition gives one proof, and one proof builds
/// one `delegate` module.
///
/// ```compile_fail,E0599
/// fn duplicate(main: lymaille::MainAgent) {
///     let _copy = main.clone();
/// }
/// ```
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MainAgent {
    delegable: Vec<String>,
}

impl MainAgent {
    /// Gives the proof for a work definition, and `None` for a configuration or a sub-agent definition.
    #[must_use]
    pub fn from_definition(definition: &AgentDefinition) -> Option<Self> {
        (definition.kind() == AgentKind::Work).then(|| Self {
            delegable: definition.delegable().to_vec(),
        })
    }
}

/// What the functions of `delegate` share: the sub-agents the definition allows, the registry, the limiter and the
/// cancellation of the main session.
struct Delegate<L> {
    allowed: Vec<String>,
    registry: Arc<RunRegistry<L>>,
    limiter: Arc<InferenceLimiter>,
    cancel: CancelHandle,
    redactor: Arc<Redactor>,
}

/// Builds the Rhai module `delegate` of the main agent.
///
/// `agents()` lists the sub-agents the definition authorises; `start`, `status`, `wait` and `cancel` act on the registry,
/// and `capacity` reads the limiter at each call. The proof `main` is consumed: it is what lets a work session delegate.
#[must_use]
pub fn delegate_module<L: SubagentLauncher>(
    main: MainAgent,
    registry: Arc<RunRegistry<L>>,
    limiter: Arc<InferenceLimiter>,
    cancel: CancelHandle,
    redactor: Arc<Redactor>,
) -> Module {
    let shared = Arc::new(Delegate {
        allowed: main.delegable,
        registry,
        limiter,
        cancel,
        redactor,
    });
    let mut module = Module::new();
    let agents = Arc::clone(&shared);
    let _agents = module.set_native_fn("agents", move || -> Result<Array, Box<EvalAltResult>> {
        Ok(agents
            .allowed
            .iter()
            .map(|name| Dynamic::from(name.clone()))
            .collect())
    });
    let start = Arc::clone(&shared);
    let _start = module.set_native_fn(
        "start",
        move |name: ImmutableString, task: ImmutableString| -> Result<String, Box<EvalAltResult>> {
            start.start(name.as_str(), task.as_str())
        },
    );
    let status = Arc::clone(&shared);
    let _status = module.set_native_fn(
        "status",
        move |id: ImmutableString| -> Result<Map, Box<EvalAltResult>> { status.status(id.as_str()) },
    );
    let wait = Arc::clone(&shared);
    let _wait = module.set_native_fn(
        "wait",
        move |id: ImmutableString| -> Result<Map, Box<EvalAltResult>> { wait.wait(id.as_str()) },
    );
    let stop = Arc::clone(&shared);
    let _cancel = module.set_native_fn(
        "cancel",
        move |id: ImmutableString| -> Result<(), Box<EvalAltResult>> { stop.cancel(id.as_str()) },
    );
    let capacity = shared;
    let _capacity = module.set_native_fn("capacity", move || -> Result<i64, Box<EvalAltResult>> {
        Ok(i64::try_from(capacity.limiter.capacity()).unwrap_or(i64::MAX))
    });
    module
}

impl<L: SubagentLauncher> Delegate<L> {
    /// Starts a run of an allowed sub-agent, under the cancellation of the main session. The name is checked against the
    /// definition before the registry is asked, so a refused name never creates a run.
    fn start(&self, name: &str, task: &str) -> Result<String, Box<EvalAltResult>> {
        require_runtime()?;
        if !self.allowed.iter().any(|allowed| allowed == name) {
            return Err(Box::new(refused(&DelegationError::UnknownSubagent {
                name: name.to_owned(),
            })));
        }
        let handle = bridge(async { self.registry.start(name, task, &self.cancel) })?;
        Ok(handle.run().as_str().to_owned())
    }

    /// State of a run: `running`, or the table of its outcome once it is concluded.
    fn status(&self, id: &str) -> Result<Map, Box<EvalAltResult>> {
        require_runtime()?;
        let run = run_id(id)?;
        match bridge(async { self.registry.status(&run) })? {
            RunProgress::Running => Ok(table([("state", text("running"))])),
            RunProgress::Ended(outcome) => outcome_table(&outcome, &self.redactor),
        }
    }

    /// Waits for the end of a run and gives the table of its outcome.
    fn wait(&self, id: &str) -> Result<Map, Box<EvalAltResult>> {
        require_runtime()?;
        let run = run_id(id)?;
        let outcome = bridge(async { self.registry.wait(&run).await })?;
        outcome_table(&outcome, &self.redactor)
    }

    /// Cancels a run and, through its children, the sub-agents it runs.
    fn cancel(&self, id: &str) -> Result<(), Box<EvalAltResult>> {
        require_runtime()?;
        let run = run_id(id)?;
        bridge(async { self.registry.cancel(&run) })
    }
}

/// Builds the Rhai module `result` of a sub-agent run, over the collector of that run.
///
/// `submit(value)` answers the validation at once, so the sub-agent corrects in the same run; `fail(reason)` concludes the
/// run as failed.
#[must_use]
pub fn result_module(collector: Arc<ResultCollector>) -> Module {
    let mut module = Module::new();
    let submit = Arc::clone(&collector);
    let _submit = module.set_native_fn(
        "submit",
        move |value: Dynamic| -> Result<Map, Box<EvalAltResult>> { submit_value(&submit, &value) },
    );
    let fail = collector;
    let _fail = module.set_native_fn(
        "fail",
        move |reason: ImmutableString| -> Result<(), Box<EvalAltResult>> { fail_run(&fail, reason.as_str()) },
    );
    module
}

/// Submits a value and gives the answer of the collector: `accepted`, or the errors and the corrections left.
fn submit_value(collector: &ResultCollector, value: &Dynamic) -> Result<Map, Box<EvalAltResult>> {
    let value: Value = from_dynamic(value)
        .map_err(|_refusal| Box::new(text_error("the submitted value is not a JSON value")))?;
    match collector
        .submit(value)
        .map_err(|error| Box::new(refused(&error)))?
    {
        Submission::Accepted { .. } => Ok(table([("accepted", Dynamic::from(true))])),
        Submission::Rejected { errors, remaining } => Ok(rejection(errors, i64::from(remaining))),
        Submission::Exhausted { errors } => Ok(rejection(errors, 0)),
        Submission::Closed => Err(Box::new(text_error(CONCLUDED))),
    }
}

/// Concludes the run as failed with the reason of the sub-agent. A run already concluded refuses the call.
fn fail_run(collector: &ResultCollector, reason: &str) -> Result<(), Box<EvalAltResult>> {
    let concluded = collector
        .fail(reason)
        .map_err(|error| Box::new(refused(&error)))?;
    if concluded {
        Ok(())
    } else {
        Err(Box::new(text_error(CONCLUDED)))
    }
}

/// Refusal of a submission or a failure once the result is concluded.
const CONCLUDED: &str = "the result is already concluded";

/// The answer of a rejected submission: `accepted` at false, the errors, and the corrections left.
fn rejection(errors: Vec<String>, corrections_left: i64) -> Map {
    let errors: Array = errors.into_iter().map(Dynamic::from).collect();
    table([
        ("accepted", Dynamic::from(false)),
        ("errors", Dynamic::from(errors)),
        ("corrections_left", Dynamic::from(corrections_left)),
    ])
}

/// Text of the refusal of a `delegate` call made on a thread that has no runtime: the registry needs the runtime of the host.
const NO_RUNTIME: &str =
    "no tokio runtime on this thread: the delegate functions need the runtime of the host";

/// Refuses a `delegate` call made without a runtime. Without one, a run would be spawned on a runtime that is dropped at
/// once, and the wait would never end. The scratchpad evaluates on a thread that enters the runtime of the host, so the
/// production calls pass (`scratchpad/run.rs`).
fn require_runtime() -> Result<(), Box<EvalAltResult>> {
    Handle::try_current()
        .map(|_handle| ())
        .map_err(|_absent| Box::new(text_error(NO_RUNTIME)))
}

/// Runs a registry call to completion through the `vynil-core` bridge. A refusal of the registry and a refusal of the bridge
/// (a `current_thread` runtime) both become Rhai runtime errors that carry their message.
fn bridge<T>(future: impl Future<Output = DelegationResult<T>>) -> Result<T, Box<EvalAltResult>> {
    match block_on(future) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Box::new(refused(&error))),
        Err(error) => Err(vynil_core::rhai_err(error)),
    }
}

/// Parses a run identifier. An empty one is unknown.
fn run_id(id: &str) -> Result<RunId, Box<EvalAltResult>> {
    RunId::new(id).ok_or_else(|| Box::new(refused(&DelegationError::UnknownRun { id: id.to_owned() })))
}

/// The table of a concluded outcome: `state`, `result`, `errors` and `reason`.
fn outcome_table(outcome: &SubagentOutcome, redactor: &Redactor) -> Result<Map, Box<EvalAltResult>> {
    let result = match outcome.result() {
        Some(value) => to_dynamic(value)?,
        None => Dynamic::UNIT,
    };
    let errors: Array = outcome
        .errors()
        .iter()
        .map(|error| Dynamic::from(error.clone()))
        .collect();
    let reason = outcome
        .reason()
        .map_or(Dynamic::UNIT, |reason| Dynamic::from(redactor.redact(reason)));
    Ok(table([
        ("state", text(state_label(outcome.status()))),
        ("result", result),
        ("errors", Dynamic::from(errors)),
        ("reason", reason),
    ]))
}

/// Name of the state of a concluded run in the Rhai tables.
const fn state_label(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Valid => "valid",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Cancelled => "cancelled",
        SubagentStatus::Interrupted => "interrupted",
    }
}

/// Builds a Rhai map from its named fields.
fn table<const N: usize>(fields: [(&str, Dynamic); N]) -> Map {
    fields
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
}

/// A Rhai text value.
fn text(value: &str) -> Dynamic {
    Dynamic::from(value.to_owned())
}

/// Wraps a registry refusal as a Rhai runtime error that carries its message, which names no content.
fn refused(error: &DelegationError) -> EvalAltResult {
    text_error(&error.to_string())
}

/// Wraps a fixed message as a Rhai runtime error.
fn text_error(message: &str) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(text(message), Position::NONE)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt::Debug;
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
    use std::time::Duration;

    use rhai::{Array, Dynamic, Engine as RhaiEngine, EvalAltResult, Map};
    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::{MainAgent, delegate_module, result_module};
    use crate::agents::{
        AgentDefinition, AgentKind, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse,
        ModelProfile,
    };
    use crate::clock::{ManualClock, Timestamp};
    use crate::context::{Budget, ContextResult, DefaultStrategy, Digest, ReplayMode, Summarizer};
    use crate::delegation::{
        DEFAULT_BATCH_MAX_TURNS, DEFAULT_CORRECTIONS, DelegationError, DelegationResult, EventHub,
        InferenceLimiter, LaunchRequest, LocalCounter, RequestCounter, ResultCollector, RunRecord,
        RunRegistry, SubagentLauncher, SubagentOutcome, SubagentStatus, run_batch,
    };
    use crate::runtime::{RHAI_PAD_TOOL, TurnConfig, TurnRunner};
    use crate::scratchpad::{GrantedModule, Grants, Scratchpad, ScratchpadEngine, ScratchpadLimits};
    use crate::session::{
        AgentRef, CancelHandle, EventSink, InMemoryStore, Message, ModelSnapshot, RecordingSink, Redactor,
        RunId, SegmentContext, SessionId, SessionState, ToolCall,
    };

    /// Identifier of the main session of the tests.
    const MAIN: &str = "main-session";

    /// Scripts of the sub-agent model of the end-to-end test: a rejected submission, then a valid one. The first one carries
    /// an undeclared key with a secret-looking value, which must reach neither the answer nor an error.
    const SCRIPTS: [&str; 2] = [
        r#"result::submit(#{secret_key: "tok-e2e-7f"})"#,
        r#"result::submit(#{answer: "ok"})"#,
    ];

    fn main_session() -> SessionId {
        SessionId::new(MAIN).expect("a non-empty text makes a session identifier")
    }

    fn answer_schema() -> Value {
        json!({"type": "object", "required": ["answer"], "properties": {"answer": {"type": "string"}}})
    }

    fn definition(id: &str, kind: AgentKind) -> AgentDefinition {
        AgentDefinition::new(id.to_owned(), kind, "prompt".to_owned())
    }

    /// The proof of a work agent that may delegate to the given sub-agents.
    fn main_agent(delegable: &[&str]) -> MainAgent {
        let owned: Vec<String> = delegable.iter().map(|name| (*name).to_owned()).collect();
        let work = definition("main", AgentKind::Work).with_delegable(owned);
        MainAgent::from_definition(&work).expect("a work definition gives the proof")
    }

    /// Locks a test double, recovering a poisoned lock: the record of a test survives a failing assertion.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Launcher double: counts its launches, answers `{"answer": "ok"}` once released, and gives up when its run is
    /// cancelled.
    struct Answering {
        release: Arc<Notify>,
        launches: AtomicUsize,
    }

    impl Answering {
        fn new() -> Self {
            Self {
                release: Arc::new(Notify::new()),
                launches: AtomicUsize::new(0),
            }
        }

        /// Lets the run answer. A release given before the launch is kept.
        fn release(&self) {
            self.release.notify_one();
        }

        fn launches(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }
    }

    impl SubagentLauncher for Answering {
        fn launch(
            &self,
            request: LaunchRequest,
        ) -> impl Future<Output = DelegationResult<SubagentOutcome>> + Send {
            let _before = self.launches.fetch_add(1, Ordering::SeqCst);
            let release = Arc::clone(&self.release);
            async move {
                tokio::select! {
                    () = release.notified() => {
                        let _submission = request.collector().submit(json!({"answer": "ok"}))?;
                        request.collector().outcome()?.ok_or(DelegationError::Launch)
                    }
                    () = request.cancel().cancelled() => Err(DelegationError::Launch),
                }
            }
        }
    }

    /// Launcher double that concludes its run as failed with `reason`, as a sub-agent calling `result::fail` does.
    struct Failing {
        reason: String,
    }

    impl SubagentLauncher for Failing {
        fn launch(
            &self,
            request: LaunchRequest,
        ) -> impl Future<Output = DelegationResult<SubagentOutcome>> + Send {
            let reason = self.reason.clone();
            async move {
                let _concluded = request.collector().fail(&reason)?;
                request.collector().outcome()?.ok_or(DelegationError::Launch)
            }
        }
    }

    /// The content of each tool result among `messages`, in order.
    fn tool_result_texts(messages: &[Message]) -> Vec<String> {
        messages.iter().filter_map(tool_result_text).collect()
    }

    /// The content of a tool result, and `None` for any other message.
    fn tool_result_text(message: &Message) -> Option<String> {
        let Message::ToolResult { content, .. } = message else {
            return None;
        };
        Some(content.clone())
    }

    /// Model double of the sub-agent: one `rhai_pad` call per turn, following `SCRIPTS`, then a text answer. It keeps the
    /// tool results that each call sees.
    struct ScriptedModel {
        calls: AtomicUsize,
        seen: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl InferenceProvider for ScriptedModel {
        fn complete(
            &self,
            request: InferenceRequest,
        ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send {
            let results = tool_result_texts(request.messages());
            lock(&self.seen).push(results);
            let step = self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = SCRIPTS.get(step).map_or_else(
                || Message::assistant("done", None, Vec::new()),
                |script| {
                    Message::assistant(
                        "",
                        None,
                        vec![ToolCall::new(
                            format!("call-{step}"),
                            RHAI_PAD_TOOL,
                            json!({"script": script}),
                        )],
                    )
                },
            );
            std::future::ready(Ok(InferenceResponse::new(answer, None)))
        }
    }

    /// Summarizer that is never needed by these tests.
    struct NoSummary;

    impl Summarizer for NoSummary {
        fn summarize(
            &self,
            _transcript: &str,
            _digest: Option<&Digest>,
        ) -> impl Future<Output = ContextResult<String>> + Send {
            std::future::ready(Ok(String::new()))
        }
    }

    /// A turn runner of the sub-agent session `session`, driven by `provider` and granted `grants`.
    fn batch_runner<P: InferenceProvider>(
        session: &str,
        provider: P,
        grants: Grants,
    ) -> TurnRunner<P, DefaultStrategy<NoSummary>, InMemoryStore> {
        let profile = ModelProfile::new("profile-batch".to_owned(), "prov".to_owned(), "model".to_owned())
            .with_context_size(100_000)
            .with_replay(ReplayMode::None)
            .resolve(None)
            .expect("the profile resolves");
        let context = SegmentContext::new(
            "system prompt",
            Vec::new(),
            ModelSnapshot::new("model", ReplayMode::None),
        );
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), grants)
            .build()
            .expect("the engine builds");
        let config = TurnConfig::new(
            Arc::new(ManualClock::new(Timestamp::from_millis(0))),
            Redactor::new(),
            Arc::new(RecordingSink::new()),
            CancelHandle::new(),
            Budget::new(100_000, 100, 1000, 1000),
        );
        let state = SessionState::new(
            SessionId::new(session).expect("a non-empty text makes a session identifier"),
            AgentRef::new("sub-agent", "fp-1"),
            "ctx",
            BTreeMap::new(),
            Timestamp::from_millis(0),
        );
        TurnRunner::new(
            provider,
            DefaultStrategy::new(NoSummary, ReplayMode::None),
            profile,
            context,
            Scratchpad::new(Arc::new(engine)),
            InMemoryStore::new(),
            state,
            config,
        )
    }

    /// Launcher double of a real sub-agent: a turn runner granted the `result` module over the collector of the request,
    /// driven by `run_batch` (`agent-sessions/delegation#FR-4`).
    struct Batch {
        seen: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl SubagentLauncher for Batch {
        fn launch(
            &self,
            request: LaunchRequest,
        ) -> impl Future<Output = DelegationResult<SubagentOutcome>> + Send {
            let seen = Arc::clone(&self.seen);
            async move {
                let module = result_module(Arc::clone(request.collector()));
                let granted =
                    GrantedModule::new("result", Arc::new(module)).expect("result is a Rhai identifier");
                let grants = Grants::new()
                    .with_module(granted)
                    .expect("the module is granted once");
                let model = ScriptedModel {
                    calls: AtomicUsize::new(0),
                    seen,
                };
                let mut runner = batch_runner(request.session().as_str(), model, grants);
                run_batch(
                    &mut runner,
                    request.task(),
                    request.collector(),
                    request.cancel(),
                    DEFAULT_BATCH_MAX_TURNS,
                )
                .await
            }
        }
    }

    /// A registry over `names` answering through `launcher`.
    fn registry_over<L: SubagentLauncher>(launcher: Arc<L>, names: &[&str]) -> RunRegistry<L> {
        let declared: BTreeMap<String, (Value, u32)> = names
            .iter()
            .map(|name| ((*name).to_owned(), (answer_schema(), DEFAULT_CORRECTIONS)))
            .collect();
        let sink: Arc<dyn EventSink> = Arc::new(RecordingSink::new());
        RunRegistry::new(
            launcher,
            declared,
            sink,
            Arc::new(ManualClock::new(Timestamp::from_millis(1_000))),
            Arc::new(EventHub::new()),
            main_session(),
        )
        .expect("the answer schema compiles")
    }

    /// A registry over `names` with an answering launcher, kept by the test.
    struct Rig {
        registry: Arc<RunRegistry<Answering>>,
        launcher: Arc<Answering>,
    }

    fn rig(names: &[&str]) -> Rig {
        let launcher = Arc::new(Answering::new());
        let registry = Arc::new(registry_over(Arc::clone(&launcher), names));
        Rig { registry, launcher }
    }

    /// A limiter of `limit` requests counted by `counter`.
    fn limiter_over(counter: &Arc<LocalCounter>, limit: u64) -> Arc<InferenceLimiter> {
        let shared: Arc<dyn RequestCounter> = Arc::<LocalCounter>::clone(counter);
        Arc::new(InferenceLimiter::new(
            NonZeroU64::new(limit).expect("a non-zero limit"),
            shared,
        ))
    }

    /// A Rhai engine with the `delegate` module of `main` over `registry`, under a fresh cancellation and no secret.
    fn delegate_engine<L: SubagentLauncher>(
        main: MainAgent,
        registry: &Arc<RunRegistry<L>>,
        limiter: Arc<InferenceLimiter>,
    ) -> RhaiEngine {
        engine_with(main, registry, limiter, &CancelHandle::new(), Redactor::new())
    }

    /// A Rhai engine with the `delegate` module of `main` over `registry`, under `cancel`, masking the secrets of `redactor`.
    fn engine_with<L: SubagentLauncher>(
        main: MainAgent,
        registry: &Arc<RunRegistry<L>>,
        limiter: Arc<InferenceLimiter>,
        cancel: &CancelHandle,
        redactor: Redactor,
    ) -> RhaiEngine {
        let module = delegate_module(
            main,
            Arc::clone(registry),
            limiter,
            cancel.clone(),
            Arc::new(redactor),
        );
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("delegate", Arc::new(module));
        engine
    }

    /// A Rhai engine with the `result` module over `collector`.
    fn result_engine(collector: &Arc<ResultCollector>) -> RhaiEngine {
        let module = result_module(Arc::clone(collector));
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("result", Arc::new(module));
        engine
    }

    /// The text of the error of a failed script. The script must fail.
    fn error_text<T: Debug>(result: Result<T, Box<EvalAltResult>>) -> String {
        match result {
            Ok(value) => panic!("the script must fail, it returned {value:?}"),
            Err(error) => error.to_string(),
        }
    }

    /// The text stored under `key` in a Rhai map.
    fn text_field(table: &Map, key: &str) -> String {
        table
            .get(key)
            .expect("the key is present")
            .clone()
            .into_string()
            .expect("the field is a text")
    }

    fn bool_field(table: &Map, key: &str) -> bool {
        table
            .get(key)
            .expect("the key is present")
            .as_bool()
            .expect("the field is a boolean")
    }

    fn array_field(table: &Map, key: &str) -> Array {
        table
            .get(key)
            .expect("the key is present")
            .clone()
            .try_cast::<Array>()
            .expect("the field is an array")
    }

    /// Starts a run of `reviewer` on the script engine, and gives its identifier.
    fn start_reviewer(engine: &RhaiEngine) -> String {
        engine
            .eval::<String>(r#"delegate::start("reviewer", "task")"#)
            .expect("start returns the identifier of the run")
    }

    // Scenario: un sous-agent n'a pas de module de délégation
    #[test]
    fn a_sub_agent_and_a_configuration_agent_have_no_delegation_module() {
        let sub = definition("reviewer", AgentKind::Sub);
        let config = definition("configurator", AgentKind::Config);
        assert!(
            MainAgent::from_definition(&sub).is_none(),
            "a sub-agent gets no proof"
        );
        assert!(
            MainAgent::from_definition(&config).is_none(),
            "a configuration agent gets no proof"
        );
    }

    // Not a Scenario: the proof exists for a work definition, otherwise no delegation module could be built.
    #[test]
    fn a_work_definition_gives_the_proof() {
        assert!(MainAgent::from_definition(&definition("main", AgentKind::Work)).is_some());
    }

    // Scenario: l'agent principal ne désigne que les sous-agents autorisés
    #[tokio::test(flavor = "multi_thread")]
    async fn the_main_agent_names_only_its_allowed_sub_agents() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        let message = error_text(engine.eval::<Dynamic>(r#"delegate::start("deployer", "task")"#));
        assert!(
            message.contains("deployer"),
            "the error names the refused sub-agent: {message}"
        );
        assert_eq!(
            rig.launcher.launches(),
            0,
            "the refused sub-agent is never launched"
        );
        assert!(
            rig.registry.records().expect("records reads").is_empty(),
            "no run exists"
        );
    }

    // Not a Scenario: the module keeps its own check of `agents()` before it asks the registry, so a name that the
    // registry declares but the definition does not allow is refused too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_the_definition_does_not_allow_is_refused_even_if_the_registry_declares_it() {
        let launcher = Arc::new(Answering::new());
        let registry = Arc::new(registry_over(Arc::clone(&launcher), &["reviewer", "deployer"]));
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(main_agent(&["reviewer"]), &registry, limiter_over(&counter, 8));
        let message = error_text(engine.eval::<Dynamic>(r#"delegate::start("deployer", "task")"#));
        assert!(
            message.contains("deployer"),
            "the error names the refused sub-agent: {message}"
        );
        assert_eq!(launcher.launches(), 0, "the refused sub-agent is never launched");
        assert!(
            registry.records().expect("records reads").is_empty(),
            "no run exists"
        );
    }

    // Scenario: démarrer rend un handle sans attendre
    #[tokio::test(flavor = "multi_thread")]
    async fn start_gives_a_handle_before_the_run_ends() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        let id = start_reviewer(&engine);
        assert!(!id.is_empty(), "the identifier is a text");
        let status = engine
            .eval::<Map>(&format!(r#"delegate::status("{id}")"#))
            .expect("status reads the run");
        assert_eq!(
            text_field(&status, "state"),
            "running",
            "the run is still going on"
        );
        rig.launcher.release();
    }

    // Scenario: attendre rend le résultat
    #[tokio::test(flavor = "multi_thread")]
    async fn wait_gives_the_result() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        rig.launcher.release();
        let table = engine
            .eval::<Map>(r#"let id = delegate::start("reviewer", "task"); delegate::wait(id)"#)
            .expect("wait returns the final table");
        assert_eq!(text_field(&table, "state"), "valid");
        let result = table
            .get("result")
            .expect("the table carries the result")
            .clone()
            .try_cast::<Map>()
            .expect("the result is a map");
        assert_eq!(text_field(&result, "answer"), "ok");
    }

    // Scenario: capacité relue à chaque appel
    #[test]
    fn capacity_is_read_at_each_call() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let limiter = limiter_over(&counter, 8);
        let shared: &dyn RequestCounter = counter.as_ref();
        shared.acquired();
        shared.acquired();
        let engine = delegate_engine(main_agent(&["reviewer"]), &rig.registry, limiter);
        let before = engine
            .eval::<i64>("delegate::capacity()")
            .expect("capacity reads");
        assert_eq!(before, 5, "limit 8, two in flight, one for the main agent");
        shared.acquired();
        let after = engine
            .eval::<i64>("delegate::capacity()")
            .expect("capacity reads again");
        assert_eq!(after, 4, "the capacity is read again at the second call");
    }

    // Scenario: soumettre un résultat invalide puis valide
    #[test]
    fn submit_rejects_then_accepts_in_the_same_run() {
        let collector = Arc::new(
            ResultCollector::with_default_corrections("reviewer", &answer_schema())
                .expect("the schema compiles"),
        );
        let engine = result_engine(&collector);
        let rejected = engine
            .eval::<Map>("result::submit(#{})")
            .expect("submit answers a rejection");
        assert!(
            !bool_field(&rejected, "accepted"),
            "an empty map does not validate"
        );
        let errors = array_field(&rejected, "errors");
        assert!(
            errors.iter().any(|error| error.to_string().contains("answer")),
            "an error names the missing field"
        );
        let remaining = rejected
            .get("corrections_left")
            .expect("the rejection counts the corrections left")
            .as_int()
            .expect("the count is an integer");
        assert_eq!(remaining, 3, "three corrections remain after the first rejection");
        let accepted = engine
            .eval::<Map>(r#"result::submit(#{answer: "ok"})"#)
            .expect("submit answers an acceptance");
        assert!(bool_field(&accepted, "accepted"), "the corrected value validates");
    }

    // Not a Scenario: four invalid submissions through the module: the fourth one has no correction left, and the run
    // fails (`Must`, `agent-sessions/delegation#FR-4`, `AC-8`).
    #[test]
    fn corrections_left_is_zero_on_exhaustion() {
        let collector = Arc::new(
            ResultCollector::with_default_corrections("reviewer", &answer_schema())
                .expect("the schema compiles"),
        );
        let engine = result_engine(&collector);
        let mut left = Vec::new();
        for _attempt in 0..4 {
            let answer = engine
                .eval::<Map>("result::submit(#{})")
                .expect("submit answers a rejection");
            assert!(!bool_field(&answer, "accepted"), "an empty map never validates");
            left.push(
                answer
                    .get("corrections_left")
                    .expect("the count is given")
                    .as_int()
                    .expect("an integer"),
            );
        }
        assert_eq!(
            left,
            vec![3, 2, 1, 0],
            "the fourth submission has no correction left"
        );
        let outcome = collector
            .outcome()
            .expect("the outcome reads")
            .expect("the exhausted collector is concluded");
        assert_eq!(outcome.status(), SubagentStatus::Failed);
    }

    // Not a Scenario: a rejection carries the places and the rules, never the submitted value (`Must`).
    #[test]
    fn a_rejection_does_not_echo_the_submitted_value() {
        let schema = json!({"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}});
        let collector =
            Arc::new(ResultCollector::with_default_corrections("counter", &schema).expect("compiles"));
        let engine = result_engine(&collector);
        let rejected = engine
            .eval::<Map>(r#"result::submit(#{n: "s3cr3t"})"#)
            .expect("submit answers a rejection");
        assert!(!bool_field(&rejected, "accepted"));
        for error in &array_field(&rejected, "errors") {
            assert!(
                !error.to_string().contains("s3cr3t"),
                "the error `{error}` must not carry the value"
            );
        }
    }

    // Scenario: soumettre après la conclusion
    #[test]
    fn submit_after_conclusion_is_a_runtime_error() {
        let collector = Arc::new(
            ResultCollector::with_default_corrections("reviewer", &answer_schema())
                .expect("the schema compiles"),
        );
        let _accepted = collector
            .submit(json!({"answer": "ok"}))
            .expect("the collector answers");
        let engine = result_engine(&collector);
        let message = error_text(engine.eval::<Map>(r#"result::submit(#{answer: "x"})"#));
        assert!(
            message.contains("already concluded"),
            "the error says the result is concluded: {message}"
        );
        let outcome = collector
            .outcome()
            .expect("the outcome reads")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.result(),
            Some(&json!({"answer": "ok"})),
            "the accepted result is unchanged"
        );
    }

    // Scenario: échec explicite
    #[test]
    fn fail_concludes_the_run_with_its_reason() {
        let collector = Arc::new(
            ResultCollector::with_default_corrections("reviewer", &answer_schema())
                .expect("the schema compiles"),
        );
        let _rejected = collector.submit(json!({})).expect("the collector answers");
        let engine = result_engine(&collector);
        engine
            .eval::<()>(r#"result::fail("missing API key")"#)
            .expect("fail concludes the collector");
        let outcome = collector
            .outcome()
            .expect("the outcome reads")
            .expect("the collector is concluded");
        assert_eq!(outcome.status(), SubagentStatus::Failed);
        assert_eq!(outcome.reason(), Some("missing API key"));
    }

    // Not a Scenario: a `fail` on a concluded result is refused, and the outcome does not change (`Raises`, `result.sdd`).
    #[test]
    fn fail_after_conclusion_is_a_runtime_error() {
        let collector = Arc::new(
            ResultCollector::with_default_corrections("reviewer", &answer_schema())
                .expect("the schema compiles"),
        );
        let _accepted = collector
            .submit(json!({"answer": "ok"}))
            .expect("the collector answers");
        let engine = result_engine(&collector);
        let message = error_text(engine.eval::<()>(r#"result::fail("late")"#));
        assert!(
            message.contains("already concluded"),
            "the error says the result is concluded: {message}"
        );
        let outcome = collector
            .outcome()
            .expect("the outcome reads")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Valid,
            "the accepted outcome is unchanged"
        );
    }

    // Scenario: runtime current_thread
    #[tokio::test]
    async fn a_current_thread_runtime_renders_the_explicit_error() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        for script in [
            r#"delegate::start("reviewer", "task")"#,
            r#"delegate::wait("run-1")"#,
            r#"delegate::status("run-1")"#,
            r#"delegate::cancel("run-1")"#,
        ] {
            let message = error_text(engine.eval::<Dynamic>(script));
            assert!(
                message.contains("multi-thread"),
                "{script} names the runtime it needs: {message}"
            );
        }
        assert_eq!(rig.launcher.launches(), 0, "no call reaches the registry");
        assert!(
            rig.registry.records().expect("records reads").is_empty(),
            "no run exists"
        );
    }

    // Not a Scenario: without any runtime on the thread, every function refuses at once with a fixed text, and no call
    // hangs (`Must`: the module refuses cleanly when the host gives no runtime). The engine runs on its own thread, with a
    // bound: a call that hangs fails the test instead of blocking it.
    #[test]
    fn without_a_runtime_every_function_refuses_without_hanging() {
        let rig = rig(&["reviewer"]);
        let registry = Arc::clone(&rig.registry);
        let (sender, receiver) = mpsc::channel();
        let _thread = std::thread::spawn(move || {
            let counter = Arc::new(LocalCounter::new());
            let engine = delegate_engine(main_agent(&["reviewer"]), &registry, limiter_over(&counter, 8));
            let answers: Vec<String> = [
                r#"delegate::start("reviewer", "task")"#,
                r#"delegate::wait("run-1")"#,
                r#"delegate::status("run-1")"#,
                r#"delegate::cancel("run-1")"#,
            ]
            .iter()
            .map(|script| match engine.eval::<Dynamic>(script) {
                Ok(value) => format!("returned: {value}"),
                Err(error) => error.to_string(),
            })
            .collect();
            let _sent = sender.send(answers);
        });
        let answers = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("no call hangs without a runtime: every answer comes within five seconds");
        for answer in &answers {
            assert!(
                answer.contains("no tokio runtime"),
                "the refusal names the missing runtime: {answer}"
            );
        }
        assert_eq!(rig.launcher.launches(), 0, "no call reaches the launcher");
        assert!(
            rig.registry.records().expect("records reads").is_empty(),
            "no run exists"
        );
    }

    // Not a Scenario: an unknown run is named by its identifier (`Raises`).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_run_is_named_by_its_identifier() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        let message = error_text(engine.eval::<Dynamic>(r#"delegate::status("run-9")"#));
        assert!(message.contains("run-9"), "the error names the run: {message}");
    }

    // Not a Scenario: the task never reaches an error message (`Must`, `agent-sessions/delegation#FR-1`).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_task_never_reaches_an_error_message() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        let message = error_text(engine.eval::<Dynamic>(r#"delegate::start("ghost", "tok-live-4f2a9c")"#));
        assert!(
            !message.contains("tok-live-4f2a9c"),
            "the error `{message}` must not carry the task"
        );
    }

    // Not a Scenario: `cancel` cancels the run through the registry; the end is awaited on the Rust side under a bound, so a
    // `cancel` that does nothing fails the test instead of hanging it. Then `status` gives the cancelled table.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_ends_the_run_as_cancelled() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        let id = start_reviewer(&engine);
        engine
            .eval::<()>(&format!(r#"delegate::cancel("{id}")"#))
            .expect("cancel is accepted");
        let run = RunId::new(id.clone()).expect("a non-empty identifier");
        let outcome = tokio::time::timeout(Duration::from_secs(5), rig.registry.wait(&run))
            .await
            .expect("the cancelled run ends within five seconds")
            .expect("the run is known");
        assert_eq!(outcome.status(), SubagentStatus::Cancelled);
        let status = engine
            .eval::<Map>(&format!(r#"delegate::status("{id}")"#))
            .expect("status reads the concluded run");
        assert_eq!(text_field(&status, "state"), "cancelled");
        assert_eq!(text_field(&status, "reason"), "the run was cancelled");
        assert!(
            status.get("result").is_some_and(Dynamic::is_unit),
            "a cancelled run has no result"
        );
    }

    // Not a Scenario: cancelling the main session cancels the runs it started (`FR-7`).
    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_the_main_session_cancels_the_started_runs() {
        let rig = rig(&["reviewer"]);
        let counter = Arc::new(LocalCounter::new());
        let cancel = CancelHandle::new();
        let engine = engine_with(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
            &cancel,
            Redactor::new(),
        );
        let id = start_reviewer(&engine);
        cancel.cancel();
        let run = RunId::new(id).expect("a non-empty identifier");
        let outcome = tokio::time::timeout(Duration::from_secs(5), rig.registry.wait(&run))
            .await
            .expect("the run ends within five seconds")
            .expect("the run is known");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "the run follows its caller"
        );
    }

    // Not a Scenario: `agents()` lists the names the definition allows, in its order (`Returns`).
    #[test]
    fn agents_lists_the_names_the_definition_allows() {
        let rig = rig(&["reviewer", "deployer"]);
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        let names = engine.eval::<Array>("delegate::agents()").expect("agents lists");
        let names: Vec<String> = names.iter().map(ToString::to_string).collect();
        assert_eq!(names, vec!["reviewer".to_owned()]);
    }

    // Not a Scenario: the reason of a sub-agent failure reaches the script masked of the secrets of the session (`Must`).
    #[tokio::test(flavor = "multi_thread")]
    async fn the_reason_of_a_failure_is_masked_before_the_script_reads_it() {
        let registry = Arc::new(registry_over(
            Arc::new(Failing {
                reason: "missing key tok-mask-91".to_owned(),
            }),
            &["reviewer"],
        ));
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-mask-91")
            .expect("the secret is accepted");
        let counter = Arc::new(LocalCounter::new());
        let engine = engine_with(
            main_agent(&["reviewer"]),
            &registry,
            limiter_over(&counter, 8),
            &CancelHandle::new(),
            redactor,
        );
        let id = start_reviewer(&engine);
        let table = engine
            .eval::<Map>(&format!(r#"delegate::wait("{id}")"#))
            .expect("wait returns the final table");
        let reason = text_field(&table, "reason");
        assert!(
            !reason.contains("tok-mask-91"),
            "the reason `{reason}` must not carry the secret"
        );
        assert!(
            reason.contains("missing key"),
            "the rest of the reason stays: {reason}"
        );
        let status = engine
            .eval::<Map>(&format!(r#"delegate::status("{id}")"#))
            .expect("status reads the concluded run");
        assert!(
            !text_field(&status, "reason").contains("tok-mask-91"),
            "status masks the reason too"
        );
    }

    // Not a Scenario, documented limit (`Must` of module.sdd): the @Redactor is a snapshot taken when the module is built, and
    // the module holds it behind an `Arc`, so a secret declared later is not masked in `reason`. This test pins that limit: it
    // is not a guarantee.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_secret_absent_at_the_construction_is_not_masked_documented_limit() {
        let registry = Arc::new(registry_over(
            Arc::new(Failing {
                reason: "missing key tok-late-5d".to_owned(),
            }),
            &["reviewer"],
        ));
        let counter = Arc::new(LocalCounter::new());
        let engine = engine_with(
            main_agent(&["reviewer"]),
            &registry,
            limiter_over(&counter, 8),
            &CancelHandle::new(),
            Redactor::new(),
        );
        let id = start_reviewer(&engine);
        let table = engine
            .eval::<Map>(&format!(r#"delegate::wait("{id}")"#))
            .expect("wait returns the final table");
        assert_eq!(
            text_field(&table, "reason"),
            "missing key tok-late-5d",
            "a secret the redactor did not hold at its construction stays in the reason: the snapshot is the limit"
        );
    }

    // Not a Scenario: a failed run gives its state, its reason, no errors and no result to `wait` and to `status`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_run_gives_its_state_and_reason_to_wait_and_status() {
        let registry = Arc::new(registry_over(
            Arc::new(Failing {
                reason: "missing API key".to_owned(),
            }),
            &["reviewer"],
        ));
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(main_agent(&["reviewer"]), &registry, limiter_over(&counter, 8));
        let id = start_reviewer(&engine);
        for script in [
            format!(r#"delegate::wait("{id}")"#),
            format!(r#"delegate::status("{id}")"#),
        ] {
            let table = engine.eval::<Map>(&script).expect("the concluded run is read");
            assert_eq!(text_field(&table, "state"), "failed", "{script}");
            assert_eq!(text_field(&table, "reason"), "missing API key", "{script}");
            assert!(
                array_field(&table, "errors").is_empty(),
                "{script}: no validation error"
            );
            assert!(
                table.get("result").is_some_and(Dynamic::is_unit),
                "{script}: no result"
            );
        }
    }

    // Not a Scenario: an interrupted run, restored with its session, gives its state and reason to `wait` and `status`
    // (`Returns`, `agent-sessions/delegation#FR-14`).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_interrupted_run_gives_its_state_and_reason() {
        let rig = rig(&["reviewer"]);
        let record = RunRecord::new(
            RunId::new("run-1").expect("a non-empty identifier"),
            "reviewer".to_owned(),
            SessionId::new("sub-session").expect("a non-empty identifier"),
        );
        rig.registry
            .restore(vec![record])
            .expect("restore accepts a known sub-agent");
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(
            main_agent(&["reviewer"]),
            &rig.registry,
            limiter_over(&counter, 8),
        );
        for script in [r#"delegate::wait("run-1")"#, r#"delegate::status("run-1")"#] {
            let table = engine.eval::<Map>(script).expect("the restored run is read");
            assert_eq!(text_field(&table, "state"), "interrupted", "{script}");
            assert_eq!(
                text_field(&table, "reason"),
                "the run was interrupted by a restore of the session",
                "{script}"
            );
            assert!(
                table.get("result").is_some_and(Dynamic::is_unit),
                "{script}: no result"
            );
        }
    }

    // Not a Scenario, end to end: a real sub-agent (a turn runner granted `result`, driven by `run_batch`) submits an invalid
    // result through `rhai_pad`, sees the refusal at its second model call without the submitted key or value, corrects it in
    // the same run, and `delegate::wait` gives the corrected result (`FR-4`, `AC-2`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sub_agent_corrects_its_result_in_the_same_run_through_the_modules() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(registry_over(
            Arc::new(Batch {
                seen: Arc::clone(&seen),
            }),
            &["reviewer"],
        ));
        let counter = Arc::new(LocalCounter::new());
        let engine = delegate_engine(main_agent(&["reviewer"]), &registry, limiter_over(&counter, 8));
        let table = engine
            .eval::<Map>(r#"let id = delegate::start("reviewer", "task"); delegate::wait(id)"#)
            .expect("wait returns the final table");
        assert_eq!(text_field(&table, "state"), "valid");
        let result = table
            .get("result")
            .expect("the table carries the result")
            .clone()
            .try_cast::<Map>()
            .expect("the result is a map");
        assert_eq!(
            text_field(&result, "answer"),
            "ok",
            "the corrected value is the result"
        );
        let calls = lock(&seen).clone();
        let second = calls.get(1).expect("the model is called a second time");
        let shown = second.join("\n");
        assert!(
            shown.contains(r#""accepted":false"#),
            "the refusal reaches the model: {shown}"
        );
        assert!(
            shown.contains(r#""corrections_left":3"#),
            "the corrections left reach the model: {shown}"
        );
        assert!(
            !shown.contains("secret_key") && !shown.contains("tok-e2e-7f"),
            "neither the key nor the value of the submission reaches the model: {shown}"
        );
    }
}
