//! The pad of a session and the execution of one call of the `rhai_pad` tool: the scope and the functions kept from one
//! call to the next, the evaluation on a thread of its own, the cancellation, the rendered result, the persistence
//! warnings, the masking of the secrets and the snapshot kept for the persistence. See `src/scratchpad/run.sdd`.

use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rhai::{AST, Dynamic, EvalAltResult, FnAccess, Scope};
use tokio::runtime::Handle;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, oneshot};
use vynil_core::cancel::{self as vynil_cancel, CancelToken};

use super::engine::{RunScope, ScratchpadEngine, captured_output};
use super::error::{ScratchpadError, ScriptErrorClass};
use super::introspect::{ScriptFunction, VariableInfo};
use super::persist::{
    PersistWarning, RestoreWarning, extract_function_sources, key_of, recompile_functions, restore_scope,
    snapshot_scope, to_dynamic,
};
use super::render::{full_text, truncate_middle};
use super::suggest::{CandidateSet, suggest_for};
use crate::clock::Clock;
use crate::session::{CancelHandle, Redactor, ScriptInput, ScriptRecord, TaggedValue};

/// The environment of one run: the agent that runs the script, the redactor of its session and the clock that dates it.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct RunEnv<'env> {
    agent: &'env str,
    redactor: &'env Redactor,
    clock: &'env dyn Clock,
}

impl<'env> RunEnv<'env> {
    /// The environment of a run of the agent `agent`.
    #[must_use]
    pub const fn new(agent: &'env str, redactor: &'env Redactor, clock: &'env dyn Clock) -> Self {
        Self {
            agent,
            redactor,
            clock,
        }
    }
}

impl std::fmt::Debug for RunEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunEnv")
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

/// The persistable state of a pad: its variables as tagged values, its functions as source texts keyed `name/arity`, and
/// the warnings of the variables that were excluded.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct PadSnapshot {
    variables: BTreeMap<String, TaggedValue>,
    functions: BTreeMap<String, String>,
    warnings: Vec<PersistWarning>,
}

/// The Debug of a snapshot shows the names of its variables and of its function keys, and the number of its warnings. It
/// never shows a value nor a source text, which hold the secrets the snapshot was masked of.
impl std::fmt::Debug for PadSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PadSnapshot")
            .field("variables", &self.variables.keys().collect::<Vec<_>>())
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .field("warnings", &self.warnings.len())
            .finish()
    }
}

impl PadSnapshot {
    /// A snapshot with nothing in it: what an inspection pad carries, since it never persists.
    const fn empty() -> Self {
        Self {
            variables: BTreeMap::new(),
            functions: BTreeMap::new(),
            warnings: Vec::new(),
        }
    }

    /// The variables that can be persisted, ordered by name.
    #[must_use]
    pub const fn variables(&self) -> &BTreeMap<String, TaggedValue> {
        &self.variables
    }

    /// The source texts of the functions, keyed `name/arity`.
    #[must_use]
    pub const fn functions(&self) -> &BTreeMap<String, String> {
        &self.functions
    }

    /// The warnings of the variables that were excluded from the persistence.
    #[must_use]
    pub fn warnings(&self) -> &[PersistWarning] {
        &self.warnings
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunStatus {
    /// The script ran to its end; a persistence warning does not change this status.
    Ok,
    /// The script failed, with the class of its error.
    Failed(ScriptErrorClass),
    /// The run was cancelled before it ended.
    Cancelled,
}

/// What a run returns to the turn loop.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOutcome {
    text: String,
    status: RunStatus,
    truncated: bool,
    warnings: Vec<PersistWarning>,
    record: ScriptRecord,
    snapshot: PadSnapshot,
}

impl RunOutcome {
    /// The text the LLM receives: masked and capped.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How the run ended.
    #[must_use]
    pub const fn status(&self) -> RunStatus {
        self.status
    }

    /// Whether the value of the script lost characters to the render cap.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// The persistence warnings raised by this run, one per variable and per state.
    #[must_use]
    pub fn warnings(&self) -> &[PersistWarning] {
        &self.warnings
    }

    /// The audit record of the script.
    #[must_use]
    pub const fn record(&self) -> &ScriptRecord {
        &self.record
    }

    /// The persistable state of the pad after this run, with the secrets masked.
    #[must_use]
    pub const fn snapshot(&self) -> &PadSnapshot {
        &self.snapshot
    }
}

/// A pad restored from a saved state, with the names of the function sources that no longer compile or define what
/// they are keyed by.
#[derive(Debug)]
#[non_exhaustive]
pub struct Restored {
    pad: Scratchpad,
    broken: Vec<String>,
    warnings: Vec<RestoreWarning>,
}

impl Restored {
    /// The restored pad.
    #[must_use]
    pub fn into_pad(self) -> Scratchpad {
        self.pad
    }

    /// The keys of the broken function sources, sorted.
    #[must_use]
    pub fn broken(&self) -> &[String] {
        &self.broken
    }

    /// The calls that match no known name, in the order of the recompilation. Best effort: the call may still work.
    #[must_use]
    pub fn warnings(&self) -> &[RestoreWarning] {
        &self.warnings
    }
}

/// The scope and the functions of one session, shared by the calls of its `rhai_pad` tool.
#[non_exhaustive]
pub struct Scratchpad {
    engine: Arc<ScratchpadEngine>,
    state: Arc<AsyncMutex<PadState>>,
    snapshot: Mutex<PadSnapshot>,
    /// Whether the pad keeps a snapshot for the persistence. An inspection pad never does: its runs carry an empty one.
    persistent: bool,
}

/// What a run changes: the live scope, the source texts of the functions and their compiled form.
struct PadState {
    scope: Scope<'static>,
    sources: BTreeMap<String, String>,
    functions: AST,
    /// Functions listed for the introspection without being callable: those of an inspected session.
    listed: Vec<ScriptFunction>,
    warned: BTreeMap<String, String>,
}

impl std::fmt::Debug for Scratchpad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scratchpad").finish_non_exhaustive()
    }
}

impl Scratchpad {
    /// An empty pad on this engine: no variable, no function.
    #[must_use]
    pub fn new(engine: Arc<ScratchpadEngine>) -> Self {
        Self {
            engine,
            state: Arc::new(AsyncMutex::new(PadState {
                scope: Scope::new(),
                sources: BTreeMap::new(),
                functions: AST::empty(),
                listed: Vec::new(),
                warned: BTreeMap::new(),
            })),
            snapshot: Mutex::new(PadSnapshot::empty()),
            persistent: true,
        }
    }

    /// Restores a pad from a saved state: the variables are put back in the scope and the function sources are compiled
    /// again. No script is replayed.
    #[must_use]
    pub fn restore(
        engine: Arc<ScratchpadEngine>,
        variables: &BTreeMap<String, TaggedValue>,
        sources: &BTreeMap<String, String>,
    ) -> Restored {
        let scope = restore_scope(variables);
        let recompiled = recompile_functions(&engine, sources);
        let broken = recompiled.broken().to_vec();
        let warnings = recompiled.warnings().to_vec();
        let state = PadState {
            scope,
            sources: sources.clone(),
            functions: recompiled.ast().clone(),
            listed: Vec::new(),
            warned: BTreeMap::new(),
        };
        let snapshot = PadSnapshot {
            variables: variables.clone(),
            functions: sources.clone(),
            warnings: Vec::new(),
        };
        Restored {
            pad: Self {
                engine,
                state: Arc::new(AsyncMutex::new(state)),
                snapshot: Mutex::new(snapshot),
                persistent: true,
            },
            broken,
            warnings,
        }
    }

    /// Runs a script in this pad, under the cancellation handle of the run. The evaluation happens on a thread of its
    /// own; the pad lock is held for the whole run, so concurrent runs on one pad are applied one after the other.
    pub async fn run(&self, script: &str, cancel: &CancelHandle, env: &RunEnv<'_>) -> RunOutcome {
        let started = env.clock.now();
        if cancel.is_cancelled() {
            return self.refused(script, env);
        }
        let token = CancelToken::new();
        let bridge = tokio::spawn(bridge(cancel.clone(), token.clone()));
        let settled = self.evaluate_on_thread(script, cancel, token).await;
        bridge.abort();
        let finished = env.clock.now();
        let duration = Duration::from_millis(finished.as_millis().saturating_sub(started.as_millis()));
        match settled {
            Some((mut held, evaluated)) => {
                let settlement = Settlement {
                    script,
                    cancel,
                    env,
                    evaluated,
                    duration,
                };
                self.settle(&mut held, settlement)
            }
            None => self.lost(script, env, duration),
        }
    }

    /// The pad of an inspection. The variables of the target are constants of the scope. Its function sources are
    /// recompiled and listed for the introspection, but they are not merged into the functions the scripts can call,
    /// and no source is kept in the state: the pad never persists.
    pub(in crate::scratchpad) fn inspection(
        engine: Arc<ScratchpadEngine>,
        variables: &BTreeMap<String, TaggedValue>,
        sources: &BTreeMap<String, String>,
    ) -> Self {
        let mut scope = Scope::new();
        for (name, value) in variables {
            let _pushed = scope.push_constant_dynamic(name.as_str(), to_dynamic(value));
        }
        let listed = script_functions(recompile_functions(&engine, sources).ast());
        Self {
            engine,
            state: Arc::new(AsyncMutex::new(PadState {
                scope,
                sources: BTreeMap::new(),
                functions: AST::empty(),
                listed,
                warned: BTreeMap::new(),
            })),
            snapshot: Mutex::new(PadSnapshot::empty()),
            persistent: false,
        }
    }

    /// A run refused before it starts: cancelled at entry, the pad is left as it was and the script did not execute.
    fn refused(&self, script: &str, env: &RunEnv<'_>) -> RunOutcome {
        let text = ScratchpadError::Cancelled.to_string();
        let input = ScriptInput::new(env.agent, script, &text, Duration::ZERO).with_error(&text);
        let record = ScriptRecord::new(env.redactor, self.engine.limits().render_cap_chars(), input);
        RunOutcome {
            text,
            status: RunStatus::Cancelled,
            truncated: false,
            warnings: Vec::new(),
            record,
            snapshot: self.snapshot(),
        }
    }

    /// A run whose evaluation thread could not start or could not answer: an internal failure, the pad unchanged.
    fn lost(&self, script: &str, env: &RunEnv<'_>, duration: Duration) -> RunOutcome {
        let error = ScratchpadError::Internal {
            message: "the evaluation thread could not run the script".to_owned(),
        };
        let text = error.to_string();
        let input = ScriptInput::new(env.agent, script, &text, duration).with_error(&text);
        let record = ScriptRecord::new(env.redactor, self.engine.limits().render_cap_chars(), input);
        RunOutcome {
            text,
            status: RunStatus::Failed(ScriptErrorClass::Internal),
            truncated: false,
            warnings: Vec::new(),
            record,
            snapshot: self.snapshot(),
        }
    }

    /// Takes the pad lock, hands it to a dedicated thread that evaluates the script, and waits for the lock and the result
    /// to come back. `None` when the thread could not start or did not answer.
    async fn evaluate_on_thread(
        &self,
        script: &str,
        cancel: &CancelHandle,
        token: CancelToken,
    ) -> Option<(OwnedMutexGuard<PadState>, Evaluated)> {
        let guard = Arc::clone(&self.state).lock_owned().await;
        let (sender, receiver) = oneshot::channel();
        let engine = Arc::clone(&self.engine);
        let thread_cancel = cancel.clone();
        let script = script.to_owned();
        // The runtime of the host, entered on the evaluation thread so that a native call reads the same flavor as the
        // host: a `current_thread` host makes `vynil-core` refuse the call explicitly.
        let host = Handle::try_current().ok();
        let spawned = std::thread::Builder::new()
            .name("lymaille-scratchpad".to_owned())
            .stack_size(EVAL_STACK_BYTES)
            .spawn(move || {
                let _host = host.as_ref().map(Handle::enter);
                let mut guard = guard;
                let cap = engine.limits().render_cap_chars();
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    let _run = RunScope::enter(&thread_cancel, cap);
                    vynil_cancel::scope(&token, || evaluate(&engine, &mut guard, &script, &thread_cancel))
                }));
                let evaluated = outcome.unwrap_or_else(|_| Evaluated::panicked());
                let _sent = sender.send((guard, evaluated));
            });
        if spawned.is_err() {
            return None;
        }
        receiver.await.ok()
    }

    /// Turns the evaluation into the outcome: the text, the status, the persistence warnings, the audit record and the
    /// snapshot, all of them masked of the secrets.
    fn settle(&self, state: &mut PadState, settlement: Settlement<'_>) -> RunOutcome {
        let Settlement {
            script,
            cancel,
            env,
            evaluated,
            duration,
        } = settlement;
        let cap = self.engine.limits().render_cap_chars();
        let (snapshot, excluded) = if self.persistent {
            build_snapshot(&self.engine, state, env.redactor)
        } else {
            (PadSnapshot::empty(), Vec::new())
        };
        let candidates = candidates_of(&self.engine, state);
        let Warned {
            fresh,
            mut messages,
            current,
        } = warnings_of(state, snapshot.warnings(), &excluded);
        // An inspection pad never persists: the notice that its functions were not persisted would be false.
        if self.persistent {
            messages.extend(evaluated.notices);
        }
        state.warned = current;
        let notices = notices_text(&messages, env.redactor);
        let (text, truncated, status, warnings, full, error) = match evaluated.result {
            Ok(value) => {
                let (text, truncated) = ok_text(&evaluated.captured, &value, cap, &notices, env.redactor);
                (text, truncated, RunStatus::Ok, fresh, full_text(&value), None)
            }
            Err(error) => {
                let error = if cancel.is_cancelled() {
                    ScratchpadError::Cancelled
                } else {
                    error
                };
                let class = error.class();
                let status = if class == ScriptErrorClass::Cancelled {
                    RunStatus::Cancelled
                } else {
                    RunStatus::Failed(class)
                };
                let rendered = suggest_for(error, &candidates).to_string();
                let (text, truncated) = error_text(&rendered, cap, &notices, env.redactor);
                (text, truncated, status, fresh, rendered.clone(), Some(rendered))
            }
        };
        let input = ScriptInput::new(env.agent, script, &full, duration);
        let input = error.as_deref().into_iter().fold(input, ScriptInput::with_error);
        let record = ScriptRecord::new(env.redactor, cap, input);
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = snapshot.clone();
        RunOutcome {
            text,
            status,
            truncated,
            warnings,
            record,
            snapshot,
        }
    }

    /// The persistable state of the pad as the last run left it, with the secrets masked.
    #[must_use]
    pub fn snapshot(&self) -> PadSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// What `settle` turns into the outcome of a run: the script, its cancellation handle and environment, and what the
/// evaluation thread handed back, with the duration of the run.
struct Settlement<'run> {
    script: &'run str,
    cancel: &'run CancelHandle,
    env: &'run RunEnv<'run>,
    evaluated: Evaluated,
    duration: Duration,
}

/// Stack of the thread that evaluates a script, in bytes, for a release build: 256 MiB. The worst case measured for the
/// call depth and the expression depth is 125 MiB (`limits.sdd` #6); the stack is reserved, not committed.
const EVAL_STACK_RELEASE: usize = 268_435_456;

/// Stack of the thread that evaluates a script, in bytes, for a debug build: 1 GiB. The worst case measured is 606 MiB.
const EVAL_STACK_DEBUG: usize = 1_073_741_824;

/// The stack of the evaluation thread, chosen by the build profile.
const EVAL_STACK_BYTES: usize = if cfg!(debug_assertions) {
    EVAL_STACK_DEBUG
} else {
    EVAL_STACK_RELEASE
};

/// What the evaluation thread hands back: the value of the script or its error, the text its `print` and `debug`
/// captured, and the notices that the functions of the script were not persisted.
struct Evaluated {
    result: Result<Dynamic, ScratchpadError>,
    captured: String,
    notices: Vec<String>,
}

impl Evaluated {
    /// The outcome of an evaluation that panicked: contained, reported as an internal failure.
    fn panicked() -> Self {
        Self {
            result: Err(ScratchpadError::Internal {
                message: "the evaluation of the script panicked".to_owned(),
            }),
            captured: String::new(),
            notices: Vec::new(),
        }
    }
}

/// The notice given when the functions of a script could not be extracted: they stay callable in the pad, but no source
/// of them is kept in the state.
const UNPERSISTED_FUNCTIONS: &str =
    "warning: the functions of this script were not persisted; they stay callable in this session only.";

/// The persistence warnings a run raises, the messages it shows, and the warnings it leaves behind for the next run.
struct Warned {
    fresh: Vec<PersistWarning>,
    messages: Vec<String>,
    current: BTreeMap<String, String>,
}

/// Cancels the token of the evaluation thread once the handle of the run is cancelled.
async fn bridge(cancel: CancelHandle, token: CancelToken) {
    cancel.cancelled().await;
    token.cancel();
}

/// Evaluates a script on the calling thread, under the run's scope: compiles it, merges it with the functions of the pad,
/// evaluates it on the scope, then keeps the functions it defined. A script that fails to compile changes nothing.
fn evaluate(
    engine: &ScratchpadEngine,
    state: &mut PadState,
    script: &str,
    cancel: &CancelHandle,
) -> Evaluated {
    if cancel.is_cancelled() {
        return Evaluated {
            result: Err(ScratchpadError::Cancelled),
            captured: String::new(),
            notices: Vec::new(),
        };
    }
    let rhai = engine.rhai();
    let ast = match rhai.compile(script) {
        Ok(ast) => ast,
        Err(error) => {
            let boxed: Box<EvalAltResult> = error.into();
            return Evaluated {
                result: Err(ScratchpadError::from_rhai(&boxed)),
                captured: String::new(),
                notices: Vec::new(),
            };
        }
    };
    let mut functions = script_functions(&state.functions);
    functions.extend(state.listed.iter().cloned());
    engine
        .introspection()
        .update(variables_of(&state.scope), functions);
    let merged = state.functions.merge(&ast);
    let result = rhai
        .eval_ast_with_scope::<Dynamic>(&mut state.scope, &merged)
        .map_err(|error| ScratchpadError::from_rhai(&error));
    let captured = captured_output();
    let notices = if persist_functions(engine, state, script, &ast) {
        Vec::new()
    } else {
        vec![UNPERSISTED_FUNCTIONS.to_owned()]
    };
    Evaluated {
        result,
        captured,
        notices,
    }
}

/// Keeps the functions of a script that compiled: their compiled form for the next runs, and their source texts by key.
/// The functions are kept in the pad even when the script failed at run time, and even when their extraction fails. A
/// failed extraction keeps no source text of them: the stale entries of their keys are removed, so the state never holds
/// a text that differs from the pad. Returns whether the sources were kept.
fn persist_functions(engine: &ScratchpadEngine, state: &mut PadState, script: &str, ast: &AST) -> bool {
    state.functions = state.functions.merge(&ast.clone_functions_only());
    if let Ok(sources) = extract_function_sources(engine, script) {
        for source in sources {
            let _previous = state
                .sources
                .insert(source.key().to_owned(), source.text().to_owned());
        }
        true
    } else {
        for function in ast
            .iter_functions()
            .filter(|function| !function.name.starts_with("anon$"))
        {
            let _stale = state
                .sources
                .remove(&key_of(function.name, function.params.len()));
        }
        false
    }
}

/// The variables of a scope, for the introspection.
fn variables_of(scope: &Scope<'_>) -> Vec<VariableInfo> {
    scope
        .iter()
        .map(|(name, _constant, value)| VariableInfo::new(name, &value))
        .collect()
}

/// Whether a function of a script can be called by name by the LLM: public, and not a closure.
fn is_script_public(access: FnAccess, name: &str) -> bool {
    matches!(access, FnAccess::Public) && !name.starts_with("anon$")
}

/// The public functions of the pad, for the introspection.
fn script_functions(ast: &AST) -> Vec<ScriptFunction> {
    ast.iter_functions()
        .filter(|function| is_script_public(function.access, function.name))
        .map(|function| {
            ScriptFunction::new(
                function.name.to_owned(),
                function.params.iter().map(|param| (*param).to_owned()).collect(),
                doc_of(&function.comments),
            )
        })
        .collect()
}

/// The documentation of a function: its doc-comment lines without their markers, joined by line breaks.
fn doc_of(comments: &[&str]) -> Option<String> {
    let lines: Vec<String> = comments.iter().filter_map(|comment| doc_line(comment)).collect();
    (!lines.is_empty()).then_some(lines.join("\n"))
}

/// The text of one doc-comment, without its marker (`///` or `/**` ... `*/`); `None` for an ordinary comment.
fn doc_line(comment: &str) -> Option<String> {
    comment
        .strip_prefix("///")
        .map(|text| text.trim().to_owned())
        .or_else(|| {
            comment
                .strip_prefix("/**")
                .map(|text| text.trim_end_matches("*/").trim().to_owned())
        })
}

/// A function signature as the candidates show it: `name(a, b)`.
fn signature_of(name: &str, params: &[&str]) -> String {
    format!("{name}({})", params.join(", "))
}

/// The names the suggestions may propose: the visible functions, the granted modules and the variables of the scope.
/// Nothing else enters the set, so an ungranted element is never suggested.
fn candidates_of(engine: &ScratchpadEngine, state: &PadState) -> CandidateSet {
    let mut functions: Vec<(String, String)> = engine
        .catalog()
        .functions()
        .iter()
        .map(|entry| (entry.path().to_owned(), entry.signature().to_owned()))
        .collect();
    functions.extend(
        state
            .functions
            .iter_functions()
            .filter(|function| is_script_public(function.access, function.name))
            .map(|function| {
                (
                    function.name.to_owned(),
                    signature_of(function.name, &function.params),
                )
            }),
    );
    let modules = engine
        .grants()
        .modules()
        .iter()
        .map(|module| module.name().to_owned())
        .collect();
    let variables = state
        .scope
        .iter()
        .map(|(name, _constant, _value)| name.to_owned())
        .collect();
    CandidateSet::new(functions, modules, variables)
}

/// The text of a warning for a function whose masked form cannot be kept.
fn excluded_message(key: &str) -> String {
    format!(
        "warning: function `{key}` holds a secret that its masked form cannot keep; it was not persisted."
    )
}

/// The persistence warnings of a run. A warning is shown once per variable and per state: it is shown again only when
/// its variable changes state.
fn warnings_of(state: &PadState, warnings: &[PersistWarning], excluded: &[String]) -> Warned {
    let mut warned = Warned {
        fresh: Vec::new(),
        messages: Vec::new(),
        current: BTreeMap::new(),
    };
    for warning in warnings {
        let name = warning.variable().to_owned();
        let message = warning.message();
        if state.warned.get(&name) != Some(&message) {
            warned.messages.push(message.clone());
            warned.fresh.push(warning.clone());
        }
        let _previous = warned.current.insert(name, message);
    }
    for key in excluded {
        let name = format!("function {key}");
        let message = excluded_message(key);
        if state.warned.get(&name) != Some(&message) {
            warned.messages.push(message.clone());
        }
        let _previous = warned.current.insert(name, message);
    }
    warned
}

/// The warnings, one per line, each line break first, masked of the secrets. They are never cut.
fn notices_text(messages: &[String], redactor: &Redactor) -> String {
    let joined = messages.iter().fold(String::new(), |mut text, message| {
        text.push('\n');
        text.push_str(message);
        text
    });
    redactor.redact(&joined)
}

/// The text of a run that succeeded: the captured text, then the value, then the warnings. The whole text is masked; the
/// value alone is cut so that the total stays within the cap.
fn ok_text(
    captured: &str,
    value: &Dynamic,
    cap: usize,
    notices: &str,
    redactor: &Redactor,
) -> (String, bool) {
    let captured = redactor.redact(captured);
    let budget = cap
        .saturating_sub(captured.chars().count())
        .saturating_sub(notices.chars().count());
    let value = truncate_middle(&redactor.redact(&full_text(value)), budget);
    (format!("{captured}{}{notices}", value.text()), value.truncated())
}

/// The text of a run that failed: the rendered error, then the warnings. The error is masked and cut so that the total
/// stays within the cap; the warnings are kept whole.
fn error_text(rendered: &str, cap: usize, notices: &str, redactor: &Redactor) -> (String, bool) {
    let budget = cap.saturating_sub(notices.chars().count());
    let text = truncate_middle(&redactor.redact(rendered), budget);
    (format!("{}{notices}", text.text()), text.truncated())
}

/// The persistable state of the pad, masked of the secrets: the variables and the function sources. It also returns the
/// keys of the functions that cannot be kept in their masked form.
fn build_snapshot(
    engine: &ScratchpadEngine,
    state: &PadState,
    redactor: &Redactor,
) -> (PadSnapshot, Vec<String>) {
    let scope = snapshot_scope(&state.scope);
    let variables = scope
        .variables()
        .iter()
        .map(|(name, value)| (name.clone(), mask_value(value, redactor)))
        .collect();
    let mut functions = BTreeMap::new();
    let mut excluded = Vec::new();
    for (key, text) in &state.sources {
        match masked_source(engine, redactor, key, text) {
            Some((masked_key, masked_text)) => {
                let _previous = functions.insert(masked_key, masked_text);
            }
            None => excluded.push(key.clone()),
        }
    }
    let snapshot = PadSnapshot {
        variables,
        functions,
        warnings: scope.warnings().to_vec(),
    };
    (snapshot, excluded)
}

/// A tagged value with the secrets masked: every string, and every key of a table.
fn mask_value(value: &TaggedValue, redactor: &Redactor) -> TaggedValue {
    match value {
        TaggedValue::String(text) => TaggedValue::String(redactor.redact(text)),
        TaggedValue::Array(items) => {
            TaggedValue::Array(items.iter().map(|item| mask_value(item, redactor)).collect())
        }
        TaggedValue::Map(entries) => TaggedValue::Map(
            entries
                .iter()
                .map(|(key, item)| (redactor.redact(key), mask_value(item, redactor)))
                .collect(),
        ),
        TaggedValue::Unit
        | TaggedValue::Bool(_)
        | TaggedValue::Int(_)
        | TaggedValue::Float(_)
        | TaggedValue::Char(_)
        | TaggedValue::Blob(_) => value.clone(),
    }
}

/// A function source with the secrets masked. The masked source is kept only when it still compiles and still defines
/// exactly one function, under the masked key; otherwise the function is excluded.
fn masked_source(
    engine: &ScratchpadEngine,
    redactor: &Redactor,
    key: &str,
    text: &str,
) -> Option<(String, String)> {
    let masked_key = redactor.redact(key);
    let masked_text = redactor.redact(text);
    if masked_key == key && masked_text == text {
        return Some((masked_key, masked_text));
    }
    let found = extract_function_sources(engine, &masked_text).ok()?;
    match found.as_slice() {
        [only] if only.key() == masked_key => Some((masked_key, masked_text)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use rhai::{EvalAltResult, ImmutableString, Module, Shared};

    use super::{PadSnapshot, RunEnv, RunOutcome, RunStatus, Scratchpad};
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        CoreGroup, GrantedModule, Grants, ScratchpadEngine, ScratchpadLimits, ScriptErrorClass,
    };
    use crate::session::{CancelHandle, Redactor, TaggedValue};

    /// An engine with these grants, these limits and the test natives its registration adds.
    fn engine_with(
        grants: Grants,
        limits: ScratchpadLimits,
        registration: impl FnOnce(&mut rhai::Engine) + 'static,
    ) -> Arc<ScratchpadEngine> {
        let builder = ScratchpadEngine::builder(limits, grants).with_registration(move |engine| {
            registration(engine);
            Ok(())
        });
        match builder.build() {
            Ok(engine) => Arc::new(engine),
            Err(error) => panic!("the engine must build: {error}"),
        }
    }

    /// An engine with the default limits and no test native.
    fn plain() -> Arc<ScratchpadEngine> {
        engine_with(Grants::new(), ScratchpadLimits::new(), |_engine| {})
    }

    /// A module `grafana` with `loki_query(query)`.
    fn grafana() -> GrantedModule {
        let mut module = Module::new();
        let _hash = rhai::FuncRegistration::new("loki_query")
            .set_into_module(&mut module, |query: ImmutableString| format!("loki:{query}"));
        let Some(granted) = GrantedModule::new("grafana", Shared::new(module)) else {
            panic!("a valid module name");
        };
        granted
    }

    /// Runs a script with an empty redactor, a fresh clock and a handle that is not cancelled.
    async fn eval(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    /// The class of the failure of a run, which must have failed.
    fn failed(outcome: &RunOutcome) -> ScriptErrorClass {
        match outcome.status() {
            RunStatus::Failed(class) => class,
            RunStatus::Ok | RunStatus::Cancelled => {
                panic!(
                    "the run must fail, its status is {:?}: {}",
                    outcome.status(),
                    outcome.text()
                )
            }
        }
    }

    #[test]
    fn pad_and_engine_are_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Scratchpad>();
        assert_send_sync::<RunOutcome>();
        assert_send_sync::<PadSnapshot>();
    }

    #[tokio::test]
    async fn variables_and_functions_persist_across_calls() {
        let pad = Scratchpad::new(plain());
        let first = eval(&pad, "let x = 3; fn f(a) { a * 2 }").await;
        assert_eq!(first.status(), RunStatus::Ok, "the first call: {}", first.text());
        let second = eval(&pad, "f(x)").await;
        assert_eq!(second.text(), "6", "the second call sees x and f");
        assert_eq!(second.status(), RunStatus::Ok);
    }

    #[tokio::test]
    async fn persistence_survives_a_restore() {
        let first = Scratchpad::new(plain());
        let _set = eval(&first, "let x = 3; fn f(a) { a * 2 }").await;
        let snapshot = first.snapshot();
        let restored = Scratchpad::restore(plain(), snapshot.variables(), snapshot.functions());
        assert!(restored.broken().is_empty(), "no function is broken");
        let pad = restored.into_pad();
        let outcome = eval(&pad, "f(x)").await;
        assert_eq!(outcome.text(), "6", "f(x) after the restore");
        assert_eq!(outcome.status(), RunStatus::Ok);
    }

    #[tokio::test]
    async fn syntax_error_leaves_the_pad_untouched() {
        let pad = Scratchpad::new(plain());
        let _set = eval(&pad, "let x = 3").await;
        let broken = eval(&pad, "let y = ;").await;
        assert_eq!(failed(&broken), ScriptErrorClass::Syntax, "a syntax error");
        assert!(
            broken.text().contains("line 1"),
            "the position: {}",
            broken.text()
        );
        assert_eq!(eval(&pad, "x").await.text(), "3", "x is still 3");
        assert_eq!(
            failed(&eval(&pad, "y").await),
            ScriptErrorClass::NotFound,
            "y does not exist"
        );
    }

    #[tokio::test]
    async fn runtime_error_keeps_earlier_effects() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "let a = 1; throw \"boom\"; let b = 2").await;
        assert_eq!(failed(&outcome), ScriptErrorClass::Runtime, "a runtime error");
        assert!(outcome.text().contains("boom"), "the message: {}", outcome.text());
        assert_eq!(eval(&pad, "a").await.text(), "1", "a exists");
        assert_eq!(
            failed(&eval(&pad, "b").await),
            ScriptErrorClass::NotFound,
            "b does not exist"
        );
    }

    #[tokio::test]
    async fn a_failing_script_keeps_the_functions_it_defined() {
        let pad = Scratchpad::new(plain());
        let _failing = eval(&pad, "fn g() { 7 } throw \"x\"").await;
        assert_eq!(eval(&pad, "g()").await.text(), "7", "g survives the failure");
    }

    #[tokio::test]
    async fn redefinition_replaces_a_function() {
        let pad = Scratchpad::new(plain());
        let _one = eval(&pad, "fn f() { 1 }").await;
        let _two = eval(&pad, "fn f() { 2 }").await;
        assert_eq!(eval(&pad, "f()").await.text(), "2", "the last definition wins");
        let snapshot = pad.snapshot();
        assert_eq!(
            snapshot.functions().len(),
            1,
            "a single entry: {:?}",
            snapshot.functions()
        );
        assert!(snapshot.functions().contains_key("f/0"), "keyed f/0");
    }

    // Scenario: une redéfinition remplace une fonction (extraction échouée : la source périmée est retirée)
    #[tokio::test]
    async fn failed_extraction_removes_the_stale_source_of_a_redefined_function() {
        let pad = Scratchpad::new(plain());
        let _one = eval(&pad, "fn f() { 1 }").await;
        assert_eq!(
            pad.snapshot().functions().get("f/0").map(String::as_str),
            Some("fn f() { 1 }"),
            "the first definition is kept"
        );
        let outcome = eval(&pad, "fn f() { 2 } fn int.double() { this * 2 } 1").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("the functions of this script were not persisted"),
            "the extraction notice: {}",
            outcome.text()
        );
        assert!(
            !pad.snapshot().functions().contains_key("f/0"),
            "no stale source of f/0 in the snapshot: {:?}",
            pad.snapshot().functions()
        );
        assert_eq!(
            eval(&pad, "f()").await.text(),
            "2",
            "the redefinition stays callable"
        );
    }

    // Scenario: une redéfinition remplace une fonction (extraction échouée, arité un : la source périmée est retirée)
    #[tokio::test]
    async fn failed_extraction_removes_the_stale_source_of_an_arity_one_function() {
        let pad = Scratchpad::new(plain());
        let _one = eval(&pad, "fn f(a) { a + 1 }").await;
        let first = pad.snapshot();
        assert_eq!(
            first.functions().get("f/1").map(String::as_str),
            Some("fn f(a) { a + 1 }"),
            "the first definition is kept under f/1"
        );
        let outcome = eval(&pad, "fn f(a) { a + 2 } fn int.double() { this * 2 } 1").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        let snapshot = pad.snapshot();
        assert!(
            !snapshot.functions().contains_key("f/1"),
            "no stale source of f/1 in the snapshot: {:?}",
            snapshot.functions()
        );
        assert_eq!(
            eval(&pad, "f(1)").await.text(),
            "3",
            "the redefinition stays callable"
        );
    }

    // Scenario: une redéfinition remplace une fonction (extraction échouée : les sources des autres fonctions sont conservées)
    #[tokio::test]
    async fn failed_extraction_keeps_the_source_of_another_function() {
        let pad = Scratchpad::new(plain());
        let _g = eval(&pad, "fn g() { 7 }").await;
        let before = pad.snapshot();
        assert_eq!(
            before.functions().get("g/0").map(String::as_str),
            Some("fn g() { 7 }"),
            "g is kept under g/0 before the failed extraction"
        );
        let outcome = eval(&pad, "fn f(a) { a } fn int.double() { this * 2 } 1").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("the functions of this script were not persisted"),
            "the extraction notice: {}",
            outcome.text()
        );
        let snapshot = pad.snapshot();
        assert_eq!(
            snapshot.functions().get("g/0").map(String::as_str),
            Some("fn g() { 7 }"),
            "the source of g survives the failed extraction: {:?}",
            snapshot.functions()
        );
        assert_eq!(eval(&pad, "g()").await.text(), "7", "g stays callable");
    }

    #[tokio::test]
    async fn call_depth_is_an_error_rendered_to_the_llm() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "fn f(n) { f(n + 1) } f(0)").await;
        assert_eq!(failed(&outcome), ScriptErrorClass::Limit, "a limit error");
        assert!(
            outcome.text().contains("call depth"),
            "names the call depth: {}",
            outcome.text()
        );
    }

    #[tokio::test]
    async fn long_scripts_are_not_interrupted() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "let i = 0; while i < 2000000 { i += 1; } i").await;
        assert_eq!(outcome.status(), RunStatus::Ok, "no operation limit");
        assert_eq!(outcome.text(), "2000000");
    }

    #[tokio::test]
    async fn oversized_result_is_truncated_with_a_notice() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(1000);
        let pad = Scratchpad::new(engine_with(Grants::new(), limits, |_engine| {}));
        let outcome = eval(&pad, "let s = \"\"; for i in 0..5000 { s += \"a\"; } s").await;
        assert!(outcome.text().chars().count() <= 1000, "at most the cap");
        assert!(
            outcome.text().contains("output truncated"),
            "the notice: {}",
            outcome.text()
        );
        assert!(outcome.truncated(), "truncated is true");
        assert_eq!(outcome.status(), RunStatus::Ok);
    }

    #[tokio::test]
    async fn string_beyond_the_memory_limit() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "let s = \"x\"; while true { s += s; } s").await;
        assert_eq!(failed(&outcome), ScriptErrorClass::Limit, "a limit error");
        assert!(
            outcome.text().starts_with("limit exceeded (string size)"),
            "the text is the string size limit error: {}",
            outcome.text()
        );
        let other = Scratchpad::new(plain());
        assert_eq!(
            eval(&other, "1 + 1").await.text(),
            "2",
            "another pad runs normally"
        );
    }

    #[tokio::test]
    async fn closure_variable_warns_without_failing() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "let n = 3; let f = |x| x + n; n").await;
        assert_eq!(outcome.status(), RunStatus::Ok, "the script is not rejected");
        assert!(
            outcome.text().starts_with("3\n"),
            "the value first: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("warning: variable `f` holds a closure"),
            "names f: {}",
            outcome.text()
        );
        let snapshot = outcome.snapshot();
        assert!(!snapshot.variables().contains_key("f"), "f is not persisted");
        assert_eq!(
            snapshot.variables().get("n"),
            Some(&TaggedValue::Int(3)),
            "n is persisted"
        );
    }

    #[tokio::test]
    async fn the_warning_is_not_repeated() {
        let pad = Scratchpad::new(plain());
        let _first = eval(&pad, "let n = 3; let f = |x| x + n; n").await;
        let second = eval(&pad, "n + 1").await;
        assert_eq!(second.text(), "4", "no warning in the text");
        assert!(second.warnings().is_empty(), "no warning raised");
    }

    // Scenario: une fonction dont une API a disparu n'empêche pas la restauration (avertissement du pad, run.sdd #15)
    #[test]
    fn restore_gives_the_unknown_call_of_a_function_as_a_warning() {
        let sources = BTreeMap::from([("h/1".to_owned(), "fn h(n) { ops::disparue(n) }".to_owned())]);
        let restored = Scratchpad::restore(plain(), &BTreeMap::new(), &sources);
        let warnings = restored.warnings();
        assert_eq!(warnings.len(), 1, "one warning for the call to the vanished API");
        assert_eq!(warnings[0].function(), "h/1", "the warning names the function");
        assert_eq!(warnings[0].call(), "ops::disparue", "the warning names the call");
        assert!(
            restored.broken().is_empty(),
            "a call to an unknown name does not make the function broken"
        );
    }

    #[tokio::test]
    async fn restored_pad_lacks_the_excluded_variable() {
        let pad = Scratchpad::new(plain());
        let first = eval(&pad, "let n = 3; let f = |x| x + n; n").await;
        let snapshot = first.snapshot();
        let restored = Scratchpad::restore(plain(), snapshot.variables(), snapshot.functions()).into_pad();
        assert_eq!(eval(&restored, "n").await.text(), "3", "n is 3");
        assert_eq!(
            failed(&eval(&restored, "f").await),
            ScriptErrorClass::NotFound,
            "f is not a variable"
        );
    }

    /// A future that never completes; it records that it was dropped.
    struct Never(Arc<AtomicBool>);

    impl Future for Never {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }

    impl Drop for Never {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A native call that blocks on a future that never completes, through the `vynil-core` bridge. It records that it
    /// was entered, so that a test cancels the run only once the call is really blocked.
    fn stuck(entered: &AtomicBool, dropped: Arc<AtomicBool>) -> Result<i64, Box<EvalAltResult>> {
        entered.store(true, Ordering::SeqCst);
        match vynil_core::rt::block_on(Never(dropped)) {
            Ok(()) => Ok(0),
            Err(error) => Err(vynil_core::rhai_err(error)),
        }
    }

    /// Waits, for a bounded time, until the condition holds, and tells whether it did. The wait is bounded by a deadline,
    /// not a fixed sleep: the test observes a state that another task of the host sets.
    async fn within_ten_seconds(condition: impl Fn() -> bool + Send + Sync) -> bool {
        let wait = async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait).await.is_ok()
    }

    // The host runtime is multi-thread (`agent-sessions#CON-6`): on a `current_thread` runtime, the bridge refuses the call.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_of_a_never_returning_native_call() {
        let entered = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let (entered_flag, dropped_flag) = (Arc::clone(&entered), Arc::clone(&dropped));
        let engine = engine_with(Grants::new(), ScratchpadLimits::new(), move |engine| {
            let (entered_flag, dropped_flag) = (Arc::clone(&entered_flag), Arc::clone(&dropped_flag));
            let _native =
                engine.register_fn("stuck", move || stuck(&entered_flag, Arc::clone(&dropped_flag)));
        });
        let pad = Scratchpad::new(engine);
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let started = tokio::time::Instant::now();
        // The cancellation is sent once the native call is blocked: before that, it would stop the script before the call.
        let _timer = tokio::spawn(async move {
            let _reached = within_ten_seconds(|| entered.load(Ordering::SeqCst)).await;
            trigger.cancel();
        });
        let run = pad.run("stuck()", &cancel, &env);
        let Ok(outcome) = tokio::time::timeout(Duration::from_secs(5), run).await else {
            panic!("the run did not end within five seconds");
        };
        assert_eq!(
            outcome.status(),
            RunStatus::Cancelled,
            "cancelled: {}",
            outcome.text()
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the cancellation is not held back by the call that never returns"
        );
        assert!(
            within_ten_seconds(|| dropped.load(Ordering::SeqCst)).await,
            "the future was dropped"
        );
    }

    #[tokio::test]
    async fn cancellation_of_a_rhai_loop() {
        let pad = Scratchpad::new(plain());
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let started = tokio::time::Instant::now();
        let _timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let outcome = pad.run("loop { }", &cancel, &env).await;
        assert_eq!(outcome.status(), RunStatus::Cancelled, "the loop is cancelled");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the loop is cancelled promptly"
        );
    }

    #[tokio::test]
    async fn cancelled_before_start() {
        let pad = Scratchpad::new(plain());
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        cancel.cancel();
        let outcome = pad.run("let z = 1;", &cancel, &env).await;
        assert_eq!(outcome.status(), RunStatus::Cancelled, "cancelled");
        assert_eq!(outcome.text(), "script cancelled", "the cancellation is the text");
        assert_eq!(
            failed(&eval(&pad, "z").await),
            ScriptErrorClass::NotFound,
            "the script did not execute"
        );
    }

    /// The same blocked call as `stuck`, for a test that does not look at the future it drops.
    fn stuck_untracked(entered: &AtomicBool) -> Result<i64, Box<EvalAltResult>> {
        stuck(entered, Arc::new(AtomicBool::new(false)))
    }

    // The host runtime is multi-thread (`agent-sessions#CON-6`): on a `current_thread` runtime, the bridge refuses the call.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_wins_over_the_error_it_caused() {
        let entered = Arc::new(AtomicBool::new(false));
        let entered_flag = Arc::clone(&entered);
        let engine = engine_with(Grants::new(), ScratchpadLimits::new(), move |engine| {
            let _native = engine.register_fn("stuck", move || stuck_untracked(&entered_flag));
        });
        let pad = Scratchpad::new(engine);
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let _timer = tokio::spawn(async move {
            let _reached = within_ten_seconds(|| entered.load(Ordering::SeqCst)).await;
            trigger.cancel();
        });
        let outcome = pad.run("stuck()", &cancel, &env).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Cancelled,
            "Cancelled, not Failed(Runtime)"
        );
    }

    /// A native call that waits for a ready future through the `vynil-core` bridge.
    fn settle_now() -> Result<i64, Box<EvalAltResult>> {
        match vynil_core::rt::block_on(async { 1i64 }) {
            Ok(value) => Ok(value),
            Err(error) => Err(vynil_core::rhai_err(error)),
        }
    }

    /// Increments the counter every 10 milliseconds, for longer than any test: a task of the host that must keep advancing.
    async fn count_ticks(counter: Arc<AtomicUsize>) {
        for _ in 0..10_000 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _previous = counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_evaluation_does_not_use_a_runtime_worker() {
        let engine = plain();
        let first = Scratchpad::new(Arc::clone(&engine));
        let second = Scratchpad::new(engine);
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let watched = Arc::clone(&ticks);
        let _counting = tokio::spawn(count_ticks(counter));
        let (cancel_a, cancel_b) = (CancelHandle::new(), CancelHandle::new());
        let (trigger_a, trigger_b) = (cancel_a.clone(), cancel_b.clone());
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let (a, b, ()) = tokio::join!(
            first.run("loop { }", &cancel_a, &env),
            second.run("loop { }", &cancel_b, &env),
            async {
                // The runs are cancelled once the counter has advanced while they loop: if they held the workers, the
                // counter would stay at zero and the wait would end at its deadline, failing the check below.
                let _advanced = within_ten_seconds(|| watched.load(Ordering::SeqCst) >= 5).await;
                trigger_a.cancel();
                trigger_b.cancel();
            }
        );
        assert_eq!(a.status(), RunStatus::Cancelled, "the first run");
        assert_eq!(b.status(), RunStatus::Cancelled, "the second run");
        assert!(
            ticks.load(Ordering::SeqCst) >= 5,
            "the counter kept advancing: {}",
            ticks.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn a_panic_is_contained() {
        fn explode() -> i64 {
            panic!("native panic")
        }
        let engine = engine_with(Grants::new(), ScratchpadLimits::new(), |engine| {
            let _native = engine.register_fn("explode", explode);
        });
        let pad = Scratchpad::new(engine);
        assert_eq!(
            failed(&eval(&pad, "explode()").await),
            ScriptErrorClass::Internal,
            "contained"
        );
        assert_eq!(
            eval(&pad, "1 + 1").await.text(),
            "2",
            "the pad is usable after the panic"
        );
    }

    #[tokio::test]
    async fn secrets_are_masked_in_text_and_audit() {
        let pad = Scratchpad::new(plain());
        let mut redactor = Redactor::new();
        let _added = redactor.add_secret("tok-123456");
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let outcome = pad.run("\"Bearer tok-123456\"", &cancel, &env).await;
        assert!(outcome.text().contains("***"), "masked: {}", outcome.text());
        assert!(!outcome.text().contains("tok-123456"), "no secret in the text");
        let record = outcome.record();
        assert!(
            !record.script.contains("tok-123456"),
            "no secret in the script record"
        );
        assert!(
            !record.result.contains("tok-123456"),
            "no secret in the result record"
        );
        let failing = pad.run("throw \"Bearer tok-123456\"", &cancel, &env).await;
        assert_eq!(failed(&failing), ScriptErrorClass::Runtime, "a runtime error");
        let error = failing.record().error.as_deref().unwrap_or_default();
        assert!(error.contains("***"), "the masked error: {error}");
        assert!(
            !error.contains("tok-123456"),
            "no secret in the error record: {error}"
        );
        assert!(
            !failing.text().contains("tok-123456"),
            "no secret in the text: {}",
            failing.text()
        );
    }

    #[tokio::test]
    async fn errors_are_masked_and_suggested() {
        let grants = Grants::new().with_module(grafana());
        let Some(grants) = grants else {
            panic!("the module is accepted");
        };
        let pad = Scratchpad::new(engine_with(grants, ScratchpadLimits::new(), |_engine| {}));
        let mut redactor = Redactor::new();
        let _added = redactor.add_secret("tok-123456");
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let outcome = pad
            .run("grafana::loki_queryy(\"tok-123456\")", &cancel, &env)
            .await;
        assert!(
            outcome.text().contains("grafana::loki_query"),
            "the closest candidate: {}",
            outcome.text()
        );
        assert!(!outcome.text().contains("tok-123456"), "no secret in the error");
        let ungranted = pad.run("writer::loki_query(\"tok-123456\")", &cancel, &env).await;
        assert!(
            !ungranted.text().contains("writer::"),
            "no suggestion names a function of the ungranted module: {}",
            ungranted.text()
        );
        assert!(
            !ungranted.text().contains("tok-123456"),
            "no secret in the ungranted error: {}",
            ungranted.text()
        );
    }

    #[tokio::test]
    async fn print_output_is_rendered_before_the_value() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "print(\"step 1\"); 42").await;
        assert_eq!(outcome.text(), "step 1\n42", "the printed line first");
    }

    #[tokio::test]
    async fn warnings_and_value_share_the_render_cap() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(300);
        let pad = Scratchpad::new(engine_with(Grants::new(), limits, |_engine| {}));
        let outcome = eval(
            &pad,
            "let f = |x| x; let s = \"\"; for i in 0..5000 { s += \"a\"; } s",
        )
        .await;
        assert!(outcome.text().chars().count() <= 300, "the whole text is capped");
        assert!(
            outcome
                .text()
                .ends_with("was not persisted; define a `fn` instead."),
            "the warning survives"
        );
    }

    /// Advances the clock by 250 milliseconds, as a native call that takes time does.
    fn tick(clock: &ManualClock) -> i64 {
        clock.advance(Duration::from_millis(250));
        0
    }

    #[tokio::test]
    async fn audit_record_carries_the_script_and_the_duration() {
        let clock = Arc::new(ManualClock::new(Timestamp::from_millis(0)));
        let ticking = Arc::clone(&clock);
        let engine = engine_with(Grants::new(), ScratchpadLimits::new(), move |engine| {
            let _native = engine.register_fn("tick", move || tick(&ticking));
        });
        let pad = Scratchpad::new(engine);
        let redactor = Redactor::new();
        let env = RunEnv::new("agent", &redactor, clock.as_ref());
        let cancel = CancelHandle::new();
        let outcome = pad.run("tick()", &cancel, &env).await;
        assert_eq!(outcome.record().agent, "agent", "the agent name");
        assert_eq!(outcome.record().script, "tick()", "the full script");
        assert_eq!(outcome.record().duration_ms, 250, "the duration in milliseconds");
    }

    #[tokio::test]
    async fn audit_record_is_built_on_failure() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "throw \"boom\"").await;
        assert_eq!(failed(&outcome), ScriptErrorClass::Runtime, "failed");
        assert!(
            outcome
                .record()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("boom")),
            "the error is recorded"
        );
    }

    #[tokio::test]
    async fn const_is_refused() {
        let pad = Scratchpad::new(plain());
        assert_eq!(
            failed(&eval(&pad, "const C = #{a: 1}; C.a = 2").await),
            ScriptErrorClass::Syntax,
            "a syntax error"
        );
        assert_eq!(
            eval(&pad, "let c = #{a: 1}; c.a = 2; c.a").await.text(),
            "2",
            "let mutates"
        );
    }

    #[tokio::test]
    async fn only_granted_modules_are_reachable_even_through_a_function_the_llm_defined() {
        let mut reader = Module::new();
        let _read = rhai::FuncRegistration::new("read").set_into_module(&mut reader, || "ok".to_owned());
        let Some(reader) = GrantedModule::new("reader", Shared::new(reader)) else {
            panic!("a valid module name");
        };
        let Some(grants) = Grants::new().with_group(CoreGroup::Host).with_module(reader) else {
            panic!("the module is accepted");
        };
        let pad = Scratchpad::new(engine_with(grants, ScratchpadLimits::new(), |_engine| {}));
        let _defined = eval(&pad, "fn w() { writer::delete() }").await;
        let outcome = eval(&pad, "w()").await;
        assert_eq!(failed(&outcome), ScriptErrorClass::NotFound, "not found");
        // The rendered text begins with the error, which names the unknown module once; any further mention of `writer`
        // would come from a suggestion. The module is absent from the engine, so no body of it can run.
        let elsewhere = outcome.text().replacen("writer", "", 1);
        assert!(
            !elsewhere.contains("writer"),
            "no suggestion names writer: {}",
            outcome.text()
        );
    }

    #[tokio::test]
    async fn runs_on_one_pad_are_serialized() {
        let pad = Scratchpad::new(plain());
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let (cancel_a, cancel_b) = (CancelHandle::new(), CancelHandle::new());
        // The first run is polled first and takes the pad lock first; it is long enough for the second run to observe
        // whether it waits for the whole effect of the first one.
        let (first, second) = tokio::join!(
            pad.run(
                "let i = 0; while i < 300000 { i += 1; } let a = i; a",
                &cancel_a,
                &env
            ),
            pad.run("a + 1", &cancel_b, &env)
        );
        assert_eq!(first.status(), RunStatus::Ok, "the first run: {}", first.text());
        assert_eq!(
            second.text(),
            "300001",
            "the second run sees the whole effect of the first"
        );
        assert_eq!(
            second.status(),
            RunStatus::Ok,
            "the second run: {}",
            second.text()
        );
    }

    #[tokio::test]
    async fn secrets_are_masked_before_the_state_keeps_the_variables_and_the_function_sources() {
        let pad = Scratchpad::new(plain());
        let mut redactor = Redactor::new();
        let _added = redactor.add_secret("s3cr3t-value");
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let outcome = pad
            .run(
                "let k = \"s3cr3t-value\"; fn key() { \"s3cr3t-value\" } k",
                &cancel,
                &env,
            )
            .await;
        assert_eq!(outcome.text(), "***", "the rendered result is masked");
        let snapshot = outcome.snapshot();
        assert_eq!(
            snapshot.variables().get("k"),
            Some(&TaggedValue::String("***".to_owned())),
            "k is masked"
        );
        let source = snapshot.functions().get("key/0").map_or("", String::as_str);
        assert!(source.contains("***"), "the function source is masked: {source}");
        assert!(
            !source.contains("s3cr3t-value"),
            "no secret in the function source"
        );
    }

    #[tokio::test]
    async fn the_snapshot_of_a_restored_pad_is_the_one_that_restored_it() {
        let first = Scratchpad::new(plain());
        let saved = eval(&first, "let x = 3; fn f(a) { a * 2 }")
            .await
            .snapshot()
            .clone();
        let restored = Scratchpad::restore(plain(), saved.variables(), saved.functions()).into_pad();
        assert_eq!(
            restored.snapshot().variables(),
            saved.variables(),
            "same variables"
        );
        assert_eq!(
            restored.snapshot().functions(),
            saved.functions(),
            "same functions"
        );
    }

    // Scenario: les avertissements sont calculés sur une exécution échouée
    #[tokio::test]
    async fn warnings_are_computed_on_a_failed_run() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "let n = 3; let f = |x| x + n; throw \"boom\"").await;
        assert_eq!(failed(&outcome), ScriptErrorClass::Runtime, "a runtime error");
        let text = outcome.text();
        let error_at = text.find("boom");
        let warning_at = text.find("warning: variable `f` holds a closure");
        assert!(
            matches!((error_at, warning_at), (Some(error), Some(warning)) if error < warning),
            "the error, then the warning of f: {text}"
        );
        assert!(
            outcome.warnings().iter().any(|warning| warning.variable() == "f"),
            "f is raised by the run"
        );
        assert!(
            !outcome.snapshot().variables().contains_key("f"),
            "f is not persisted"
        );
    }

    // Scenario: l'échec d'extraction conserve la valeur
    #[tokio::test]
    async fn extraction_failure_keeps_the_value() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(&pad, "fn int.double() { this * 2 } 5").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().starts_with("5\n"),
            "the value 5 first: {}",
            outcome.text()
        );
        assert!(
            outcome
                .text()
                .contains("the functions of this script were not persisted"),
            "the notice: {}",
            outcome.text()
        );
        assert!(outcome.snapshot().functions().is_empty(), "no source is kept");
        assert_eq!(
            eval(&pad, "let x = 4; x.double()").await.text(),
            "8",
            "the typed method stays callable in the same pad"
        );
    }

    // Scenario: l'instantané d'introspection est posé avant l'évaluation
    #[tokio::test]
    async fn introspection_snapshot_is_set_before_evaluation() {
        let pad = Scratchpad::new(plain());
        let _set = eval(&pad, "let x = 1").await;
        let outcome = eval(&pad, "vars()").await;
        assert_eq!(outcome.status(), RunStatus::Ok, "vars() runs: {}", outcome.text());
        assert!(
            outcome.text().contains("\"x\""),
            "vars() names x: {}",
            outcome.text()
        );
    }

    // Scenario: un avertissement revient après la disparition de la variable
    #[tokio::test]
    async fn a_warning_returns_after_the_variable_disappears() {
        let pad = Scratchpad::new(plain());
        let first = eval(&pad, "let f = |x| x;").await;
        assert_eq!(first.warnings().len(), 1, "f is warned the first time");
        let shadowed = eval(&pad, "let f = 0;").await;
        assert!(shadowed.warnings().is_empty(), "no closure is left under f");
        let again = eval(&pad, "let f = |x| x;").await;
        assert_eq!(again.warnings().len(), 1, "f is warned again");
        assert!(
            again.text().contains("warning: variable `f` holds a closure"),
            "the warning text: {}",
            again.text()
        );
    }

    // Scenario: une fonction exclue par le masquage est signalée
    #[tokio::test]
    async fn function_excluded_by_masking_is_reported() {
        let pad = Scratchpad::new(plain());
        let mut redactor = Redactor::new();
        let _added = redactor.add_secret("topsecret");
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let env = RunEnv::new("agent", &redactor, &clock);
        let cancel = CancelHandle::new();
        let outcome = pad
            .run("fn f() { let topsecret = 1; topsecret } 1", &cancel, &env)
            .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("function `f/0`") && outcome.text().contains("was not persisted"),
            "the excluded function is reported: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("topsecret"),
            "no secret in the text: {}",
            outcome.text()
        );
        assert!(
            !outcome.snapshot().functions().contains_key("f/0"),
            "f is not in the snapshot"
        );
        assert!(
            outcome.warnings().is_empty(),
            "the masking notice is text only, not a persistence warning"
        );
        let extraction = eval(&pad, "fn int.double() { this * 2 } 1").await;
        assert_eq!(
            extraction.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            extraction.text()
        );
        assert!(
            extraction
                .text()
                .contains("the functions of this script were not persisted"),
            "the extraction notice is in the text: {}",
            extraction.text()
        );
        assert!(
            extraction.warnings().is_empty(),
            "the extraction notice is text only, not a persistence warning"
        );
    }

    // Scenario: runtime current_thread
    #[tokio::test]
    async fn current_thread_runtime_renders_the_bridge_error() {
        let engine = engine_with(Grants::new(), ScratchpadLimits::new(), |engine| {
            let _native = engine.register_fn("settle_now", settle_now);
        });
        let pad = Scratchpad::new(engine);
        let Ok(outcome) = tokio::time::timeout(Duration::from_secs(5), eval(&pad, "settle_now()")).await
        else {
            panic!("the run blocked on a current-thread runtime");
        };
        assert_eq!(failed(&outcome), ScriptErrorClass::Runtime, "a runtime error");
        assert!(
            outcome.text().contains("requires a multi-thread tokio runtime"),
            "the explicit bridge error: {}",
            outcome.text()
        );
    }

    #[tokio::test]
    async fn the_render_cap_comes_from_the_engine_limits() {
        let pad = Scratchpad::new(plain());
        assert_eq!(
            pad.engine.limits().render_cap_chars(),
            8000,
            "the default render cap"
        );
    }

    // T9 (2): the Debug of a snapshot shows the names of its keys and its sizes, never a value nor a source text.
    #[tokio::test]
    async fn snapshot_debug_shows_names_and_sizes_only() {
        let pad = Scratchpad::new(plain());
        let outcome = eval(
            &pad,
            r#"let token = "sk-live-FAKE-SECRET-1234"; fn fetch(x) { "sk-live-FAKE-SECRET-5678" + x }"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        let snapshot = pad.snapshot();
        assert!(
            snapshot.variables().contains_key("token"),
            "control: the snapshot holds the secret variable, so the Debug below is what is checked"
        );
        let rendered = format!("{snapshot:?}");
        assert!(
            !rendered.contains("FAKE-SECRET"),
            "the Debug of a snapshot must not show a value nor a source text: {rendered}"
        );
        assert!(
            rendered.contains("token") && rendered.contains("fetch/1"),
            "the Debug shows the names of the variable and of the function key: {rendered}"
        );
        let outcome_debug = format!("{:?}", outcome.snapshot());
        assert!(
            !outcome_debug.contains("FAKE-SECRET"),
            "the snapshot carried by an outcome follows the same rule: {outcome_debug}"
        );
    }

    // Config task #4: an inspection pad never persists, so it never renders the notice that its functions were not persisted.
    #[tokio::test]
    async fn inspection_pad_does_not_warn_about_unpersisted_functions() {
        let pad = Scratchpad::inspection(plain(), &BTreeMap::new(), &BTreeMap::new());
        let outcome = eval(&pad, "fn f(a) { a } fn int.double() { this * 2 } 1").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("not persisted"),
            "an inspection pad never persists, so it must not warn: {}",
            outcome.text()
        );
        assert_eq!(
            outcome.text(),
            "1",
            "the value alone is rendered: {}",
            outcome.text()
        );
    }
}
