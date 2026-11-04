//! Turn loop of a session: a message comes in, the model is called until a final answer without tool call, each call of the
//! `rhai_pad` tool runs in the scratchpad of the session, transient errors are retried with a backoff, the context is
//! compacted before a request, the turn can be cancelled, and the state is saved at the end of the turn
//! (`agent-sessions/session#FR-4` to `FR-7`, `AC-3`, `AC-9`). See `src/runtime/turn.sdd`.

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU64;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::task::JoinHandle;

use crate::agents::{
    ErrorClass, FinishReason, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse,
    ReportedUsage, ResolvedProfile,
};
use crate::clock::{Clock, Timestamp};
use crate::context::{Budget, CompactionStrategy, UsageTracker, estimate_tokens, replayed_reasoning};
use crate::scratchpad::{RunEnv, RunStatus, Scratchpad, truncate_middle};
use crate::session::{
    Backoff, BoundaryKind, CancelHandle, Event, EventKind, EventSink, History, Message, NativeTool, Outcome,
    Reasoning, Redactor, RunSignals, Segment, SegmentContext, SessionEdits, SessionError, SessionResult,
    SessionState, SessionStore, SleepOutcome, TokenCounters, ToolCall, TurnGate, TurnId,
};

/// Name of the native tool that runs a script in the scratchpad of the session.
pub const RHAI_PAD_TOOL: &str = "rhai_pad";

/// Default maximum number of model iterations in one turn.
const DEFAULT_MAX_STEPS: u32 = 200;

/// Safety margin, in percent, of the local estimates of the context size.
const ESTIMATE_MARGIN_PERCENT: u64 = 10;

/// Response of a turn that reached its maximum number of model iterations.
const MAX_STEPS_MESSAGE: &str = "The turn was stopped: the maximum number of model calls was reached.";

/// Final message of a turn whose response the output limit cut before any text or tool call.
const CUT_MESSAGE: &str = "the response was cut by the output limit before any text or tool call";

/// Result given to the model for a tool call that did not run because the turn was cancelled.
const CANCELLED_RESULT: &str = "tool call cancelled";

/// Result given to the model for a call of `rhai_pad` without a `script` argument.
const MISSING_SCRIPT: &str = "the rhai_pad tool requires a string argument `script`";

/// The sessions that have an active turn in this process. A session has at most one active turn per process, whatever the
/// runner that drives it (`agent-sessions/session#FR-4`).
static GATE: LazyLock<TurnGate> = LazyLock::new(TurnGate::new);

/// Messages the host sends to a session while a turn is active.
///
/// Clones share the same queue. A message sent during a turn enters the history right after the tool results of the
/// running iteration, never between a tool call and its result.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct UserInbox {
    queue: Arc<Mutex<VecDeque<String>>>,
}

impl UserInbox {
    /// Queues a message for the active turn. It does not block.
    pub fn send<T: Into<String>>(&self, text: T) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(text.into());
    }

    /// Takes every queued message, in the order they were sent.
    fn drain(&self) -> Vec<String> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

/// Configuration of a turn runner: the collaborators shared with the host and the limits of a turn.
#[non_exhaustive]
pub struct TurnConfig {
    clock: Arc<dyn Clock>,
    redactor: Redactor,
    sink: Arc<dyn EventSink>,
    cancel: CancelHandle,
    budget: Budget,
    overrun: Option<Duration>,
    max_steps: u32,
    agent: Option<String>,
}

impl std::fmt::Debug for TurnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnConfig")
            .field("overrun", &self.overrun)
            .field("max_steps", &self.max_steps)
            .finish_non_exhaustive()
    }
}

impl TurnConfig {
    /// Builds a configuration from the shared clock, the redactor of the session, the shared event sink, the root
    /// cancellation handle and the context budget. There is no overrun delay, the limit is 200 model calls per turn, and
    /// the agent name for the audit is the one of the session state.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        redactor: Redactor,
        sink: Arc<dyn EventSink>,
        cancel: CancelHandle,
        budget: Budget,
    ) -> Self {
        Self {
            clock,
            redactor,
            sink,
            cancel,
            budget,
            overrun: None,
            max_steps: DEFAULT_MAX_STEPS,
            agent: None,
        }
    }

    /// Sets the delay after which `Overrun` is emitted. The turn is never interrupted.
    #[must_use]
    pub const fn with_overrun(mut self, overrun: Duration) -> Self {
        self.overrun = Some(overrun);
        self
    }

    /// Sets the maximum number of model calls in one turn.
    #[must_use]
    pub const fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Sets the agent name recorded in the audit and in the quality signals.
    #[must_use]
    pub fn with_agent<A: Into<String>>(mut self, agent: A) -> Self {
        self.agent = Some(agent.into());
        self
    }
}

/// What a turn returns: its outcome, its masked final response and the number of model calls.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TurnOutput {
    outcome: Outcome,
    response: String,
    calls: u32,
}

impl TurnOutput {
    /// Returns the outcome of the turn.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// Returns the final response, masked of the secrets. It is empty after a cancellation or a failure of the model.
    #[must_use]
    pub fn response(&self) -> &str {
        &self.response
    }

    /// Returns the number of model calls made during the turn.
    #[must_use]
    pub const fn calls(&self) -> u32 {
        self.calls
    }
}

/// How a turn ends: the outcome and the response given to the host.
struct Finish {
    outcome: Outcome,
    response: String,
}

impl Finish {
    const fn cancelled() -> Self {
        Self {
            outcome: Outcome::Cancelled,
            response: String::new(),
        }
    }

    const fn failed() -> Self {
        Self {
            outcome: Outcome::Failed,
            response: String::new(),
        }
    }

    const fn valid(response: String) -> Self {
        Self {
            outcome: Outcome::Valid,
            response,
        }
    }
}

/// Cancellation of a turn: the handle of the turn and the root handle, and a handle that merges them for the scripts.
struct TurnCancel {
    turn: CancelHandle,
    root: CancelHandle,
    merged: CancelHandle,
    bridge: JoinHandle<()>,
}

impl TurnCancel {
    /// Merges the handle of the turn into a child of the root handle, so that a script sees either of them.
    fn new(turn: &CancelHandle, root: &CancelHandle) -> Self {
        let merged = root.child();
        let (source, target) = (turn.clone(), merged.clone());
        let bridge = tokio::spawn(async move {
            source.cancelled().await;
            target.cancel();
        });
        Self {
            turn: turn.clone(),
            root: root.clone(),
            merged,
            bridge,
        }
    }

    fn is_cancelled(&self) -> bool {
        self.turn.is_cancelled() || self.root.is_cancelled()
    }

    async fn cancelled(&self) {
        tokio::select! {
            () = self.turn.cancelled() => {}
            () = self.root.cancelled() => {}
        }
    }

    /// Runs `future` unless the turn is cancelled first, in which case the future is dropped.
    async fn guard<F: Future>(&self, future: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            () = self.cancelled() => None,
            output = future => Some(output),
        }
    }

    /// Waits for `delay` through the cancellable sleep, unless the turn is cancelled first.
    async fn sleep(&self, delay: Duration) -> SleepOutcome {
        tokio::select! {
            biased;
            () = self.cancelled() => SleepOutcome::Cancelled,
            outcome = self.merged.sleep(delay) => outcome,
        }
    }
}

impl Drop for TurnCancel {
    fn drop(&mut self) {
        self.bridge.abort();
    }
}

/// What changes during one turn.
struct TurnRun {
    turn: TurnId,
    cancel: TurnCancel,
    started: Timestamp,
    overrun_sent: bool,
    steps: u32,
    calls: u32,
    rhai_errors: u32,
    backoff: Backoff,
}

/// Emits the events of a session, dated by the shared clock.
struct Emitter {
    session: crate::session::SessionId,
    clock: Arc<dyn Clock>,
    sink: Arc<dyn EventSink>,
}

impl Emitter {
    fn emit(&self, kind: EventKind) {
        self.sink
            .emit(Event::new(self.session.clone(), self.clock.now(), kind));
    }
}

/// Holds back the end of a streamed text while it could still be the beginning of a secret cut between two fragments.
struct Masker<'red> {
    redactor: &'red Redactor,
    pending: String,
}

impl<'red> Masker<'red> {
    const fn new(redactor: &'red Redactor) -> Self {
        Self {
            redactor,
            pending: String::new(),
        }
    }

    /// Takes a fragment and returns the masked text that can be emitted now. The last characters that could start a secret
    /// stay held back for the next fragment.
    fn push(&mut self, fragment: &str) -> Option<String> {
        self.pending.push_str(fragment);
        let cut = self.cut();
        self.release(cut)
    }

    /// Returns the masked text still held back, at the end of the response.
    fn finish(&mut self) -> Option<String> {
        self.release(self.pending.len())
    }

    /// Emits the masked text before byte offset `cut` of the pending text, and keeps the rest.
    fn release(&mut self, cut: usize) -> Option<String> {
        let (head, tail) = self.pending.split_at_checked(cut)?;
        let text = self.redactor.redact(head);
        self.pending = tail.to_owned();
        (!text.is_empty()).then_some(text)
    }

    /// Byte offset up to which the pending text can be emitted: all of it but the last `longest secret - 1` characters,
    /// moved forward when a secret straddles that offset.
    fn cut(&self) -> usize {
        let hold = self.redactor.longest_secret_len().saturating_sub(1);
        let kept_chars = self.pending.chars().count().saturating_sub(hold);
        self.pending
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(self.pending.len()))
            .skip(kept_chars)
            .find(|&offset| self.splits_cleanly(offset))
            .unwrap_or(self.pending.len())
    }

    /// Tells whether masking the text on each side of `offset` gives the same as masking it whole, that is whether no
    /// secret straddles the offset.
    fn splits_cleanly(&self, offset: usize) -> bool {
        self.pending.split_at_checked(offset).is_none_or(|(head, tail)| {
            format!("{}{}", self.redactor.redact(head), self.redactor.redact(tail))
                == self.redactor.redact(&self.pending)
        })
    }
}

/// Size of the session in tokens, from the usage the provider reports and from local estimates.
struct Tokens {
    tracker: Option<UsageTracker>,
    hint: NonZeroU64,
}

impl Tokens {
    fn new(hint: NonZeroU64) -> Self {
        Self {
            tracker: UsageTracker::new(ESTIMATE_MARGIN_PERCENT, hint.get()),
            hint,
        }
    }

    /// Estimate of `chars` characters, without margin.
    fn estimate(&self, chars: u64) -> u64 {
        estimate_tokens(chars, self.hint, 0)
    }

    fn session(&self) -> u64 {
        self.tracker.as_ref().map_or(0, UsageTracker::session_tokens)
    }

    const fn added(&mut self, tokens: u64) {
        if let Some(tracker) = self.tracker.as_mut() {
            tracker.note_added(tokens);
        }
    }

    const fn set_session(&mut self, tokens: u64) {
        if let Some(tracker) = self.tracker.as_mut() {
            tracker.set_session_estimate(tokens);
        }
    }

    fn compacted(&mut self, tokens: u64) {
        if let Some(tracker) = self.tracker.as_mut() {
            tracker.note_compaction();
        }
        self.set_session(tokens);
    }

    fn record(&mut self, agent: &str, usage: Option<ReportedUsage>, estimated_prompt: u64) {
        if let Some(tracker) = self.tracker.as_mut() {
            tracker.record_response(
                agent,
                usage.map(|reported| reported.prompt()),
                usage.map(|reported| reported.completion()),
                estimated_prompt,
            );
        }
    }
}

/// Number of characters, saturating.
fn chars_of(text: &str) -> u64 {
    u64::try_from(text.chars().count()).unwrap_or(u64::MAX)
}

/// Number of characters a message adds to a request: its text, and the name and arguments of its tool calls.
fn message_chars(message: &Message) -> u64 {
    match message {
        Message::System { content } | Message::User { content } | Message::ToolResult { content, .. } => {
            chars_of(content)
        }
        Message::Assistant {
            content, tool_calls, ..
        } => tool_calls.iter().fold(chars_of(content), |total, call| {
            total
                .saturating_add(chars_of(call.name()))
                .saturating_add(chars_of(&call.arguments().to_string()))
        }),
        Message::Boundary { .. } | Message::Discarded { .. } => 0,
    }
}

/// Number of characters of the messages.
fn messages_chars(messages: &[Message]) -> u64 {
    messages
        .iter()
        .fold(0, |total, message| total.saturating_add(message_chars(message)))
}

/// Number of characters of the system prompt and of the tool definitions of a segment.
fn context_chars(context: &SegmentContext) -> u64 {
    context
        .tool_definitions()
        .iter()
        .fold(chars_of(context.system_prompt()), |total, tool| {
            total.saturating_add(chars_of(&tool.to_string()))
        })
}

/// The messages of the current segment: those after the last boundary.
fn segment_messages(history: &History) -> &[Message] {
    let messages = history.messages();
    let start = messages
        .iter()
        .rposition(|message| matches!(message, Message::Boundary { .. }))
        .map_or(0, |boundary| boundary.saturating_add(1));
    messages.get(start..).unwrap_or_default()
}

/// Whole milliseconds of `duration`, saturating.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Name of an error class in the `InferenceRetry` event.
const fn class_name(class: ErrorClass) -> &'static str {
    match class {
        ErrorClass::Transient => "transient",
        ErrorClass::Permanent => "permanent",
        ErrorClass::ContextOverflow => "context_overflow",
    }
}

/// The result of a tool call, as the model receives it.
struct ToolOutput {
    text: String,
    original_chars: Option<u64>,
    cancelled: bool,
}

impl ToolOutput {
    const fn error(text: String) -> Self {
        Self {
            text,
            original_chars: None,
            cancelled: false,
        }
    }
}

/// How one model call ended.
enum Attempt {
    Response(InferenceResponse),
    Error(InferenceError),
    Cancelled,
}

/// What the model returned after the retries and the compaction of an overflow, or why the turn stops.
enum Inference {
    Response(InferenceResponse),
    End(Finish),
}

/// Runs the turns of one session.
#[non_exhaustive]
pub struct TurnRunner<P, C, S> {
    provider: P,
    strategy: C,
    profile: ResolvedProfile,
    context: SegmentContext,
    pad: Scratchpad,
    store: S,
    state: SessionState,
    config: TurnConfig,
    inbox: UserInbox,
    emitter: Emitter,
    tokens: Tokens,
    agent: String,
    natives: BTreeMap<String, Arc<dyn NativeTool>>,
    edits: Option<Arc<SessionEdits>>,
}

impl<P, C, S> std::fmt::Debug for TurnRunner<P, C, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnRunner")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<P: InferenceProvider, C: CompactionStrategy, S: SessionStore> TurnRunner<P, C, S> {
    /// Builds the runner of a session. A state without segment gets its `Start` segment, with `context`.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature is the contract of src/runtime/turn.sdd: eight distinct collaborators of one session"
    )]
    pub fn new(
        provider: P,
        strategy: C,
        profile: ResolvedProfile,
        context: SegmentContext,
        pad: Scratchpad,
        store: S,
        mut state: SessionState,
        config: TurnConfig,
    ) -> Self {
        if state.segments().is_empty() {
            state.push_segment(Segment::new(
                BoundaryKind::Start,
                context.clone(),
                config.clock.now(),
            ));
        }
        let mut tokens = Tokens::new(profile.tokenization_hint());
        let chars = context_chars(&context).saturating_add(messages_chars(segment_messages(state.history())));
        tokens.set_session(tokens.estimate(chars));
        let emitter = Emitter {
            session: state.id().clone(),
            clock: Arc::clone(&config.clock),
            sink: Arc::clone(&config.sink),
        };
        let agent = config
            .agent
            .clone()
            .unwrap_or_else(|| state.agent().name().to_owned());
        Self {
            provider,
            strategy,
            profile,
            context,
            pad,
            store,
            state,
            config,
            inbox: UserInbox::default(),
            emitter,
            tokens,
            agent,
            natives: BTreeMap::new(),
            edits: None,
        }
    }

    /// Registers a native tool: the turn loop calls it by its name, with the arguments of the model, without the scratchpad.
    /// Chainable with `?`, and the runner stays usable after a refusal.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidNativeTool`] when the name is `rhai_pad` or a native tool of this name is already
    /// registered; the runner is then unchanged.
    pub fn with_native_tool(&mut self, tool: Arc<dyn NativeTool>) -> SessionResult<&mut Self> {
        let name = tool.name().to_owned();
        if name == RHAI_PAD_TOOL || self.natives.contains_key(&name) {
            return Err(SessionError::InvalidNativeTool { name });
        }
        let _previous = self.natives.insert(name, tool);
        Ok(self)
    }

    /// Connects the cell of session edits that the `session` module fills: after each tool call the loop empties it and
    /// applies the title and the review flags it holds. Chainable.
    pub fn with_session_edits(&mut self, edits: Arc<SessionEdits>) -> &mut Self {
        self.edits = Some(edits);
        self
    }

    /// Returns the inbox of the session.
    #[must_use]
    pub fn inbox(&self) -> UserInbox {
        self.inbox.clone()
    }

    /// Returns the current state of the session.
    #[must_use]
    pub const fn state(&self) -> &SessionState {
        &self.state
    }

    /// Runs one turn for the message `text`, under the cancellation handle `cancel` of the turn (the root handle of the
    /// configuration also cancels it). The model is called until a response without tool call; a failure of the model is
    /// an outcome, not an error.
    ///
    /// # Errors
    ///
    /// Returns [`crate::SessionError::TurnActive`] when a turn is already active for the session, in which case nothing is
    /// modified, and [`crate::SessionError::Store`] when the save at the end of the turn fails: the turn is then over.
    pub async fn run_turn(&mut self, text: &str, cancel: &CancelHandle) -> SessionResult<TurnOutput> {
        let permit = GATE.try_begin(self.state.id().clone())?;
        let turn = self
            .state
            .signals()
            .keys()
            .next_back()
            .map_or(TurnId::new(1), |last| last.next());
        let mut run = TurnRun {
            turn,
            cancel: TurnCancel::new(cancel, &self.config.cancel),
            started: self.config.clock.now(),
            overrun_sent: false,
            steps: 0,
            calls: 0,
            rhai_errors: 0,
            backoff: Backoff::new(),
        };
        self.emitter.emit(EventKind::TurnStarted { turn });
        let finish = self.drive(&mut run, text).await?;
        self.record_signals(&run, finish.outcome);
        let saved = self.store.save(&self.state).await;
        drop(permit);
        self.emitter.emit(EventKind::TurnEnded {
            turn,
            outcome: finish.outcome,
            response: finish.response.clone(),
        });
        saved?;
        Ok(TurnOutput {
            outcome: finish.outcome,
            response: finish.response,
            calls: run.calls,
        })
    }

    /// Records the quality signals of the turn.
    fn record_signals(&mut self, run: &TurnRun, outcome: Outcome) {
        let mut signals = RunSignals::new(
            self.agent.as_str(),
            self.profile.name(),
            self.state.toolset_versions().clone(),
        );
        for _ in 0..run.rhai_errors {
            signals.record_rhai_error();
        }
        signals.finish(outcome);
        self.state.record_signals(run.turn, signals);
    }

    /// The iterations of the turn, until a final response or a stop.
    async fn drive(&mut self, run: &mut TurnRun, text: &str) -> SessionResult<Finish> {
        if let Some(finish) = self.admit(run, text).await? {
            return Ok(finish);
        }
        loop {
            if let Some(finish) = self.begin_iteration(run).await {
                return Ok(finish);
            }
            let response = match self.infer(run).await {
                Inference::Response(response) => response,
                Inference::End(finish) => return Ok(finish),
            };
            if let Some(finish) = self.handle_response(run, &response).await? {
                return Ok(finish);
            }
        }
    }

    /// Checks the incoming message against the budget, compacts first when it would not fit, and appends it.
    async fn admit(&mut self, run: &TurnRun, text: &str) -> SessionResult<Option<Finish>> {
        let message = self.tokens.estimate(chars_of(text));
        let system = self.tokens.estimate(context_chars(&self.context));
        if self.config.budget.check_user_message(system, message).is_err() {
            return Ok(Some(Finish::failed()));
        }
        let due = self
            .config
            .budget
            .needs_compaction(self.tokens.session().saturating_add(message));
        if due && let Some(finish) = self.compact(run).await {
            return Ok(Some(finish));
        }
        self.state.push_message(Message::user(text));
        self.tokens.added(message);
        self.drain_inbox()?;
        Ok(None)
    }

    /// Appends the messages the host sent during the turn. They follow the results of the iteration.
    fn drain_inbox(&mut self) -> SessionResult<()> {
        for text in self.inbox.drain() {
            let tokens = self.tokens.estimate(chars_of(&text));
            self.state.push_user_during_turn(text)?;
            self.tokens.added(tokens);
        }
        Ok(())
    }

    /// Before each request: cancellation, overrun, limit of iterations, and compaction.
    async fn begin_iteration(&mut self, run: &mut TurnRun) -> Option<Finish> {
        if run.cancel.is_cancelled() {
            return Some(Finish::cancelled());
        }
        self.check_overrun(run);
        if run.steps >= self.config.max_steps {
            return Some(Finish {
                outcome: Outcome::Failed,
                response: MAX_STEPS_MESSAGE.to_owned(),
            });
        }
        run.steps = run.steps.saturating_add(1);
        if self.config.budget.needs_compaction(self.tokens.session()) {
            return self.compact(run).await;
        }
        None
    }

    /// Emits `Overrun` once, when the configured delay has elapsed.
    fn check_overrun(&self, run: &mut TurnRun) {
        let Some(limit) = self.config.overrun else {
            return;
        };
        let elapsed = self
            .config
            .clock
            .now()
            .as_millis()
            .saturating_sub(run.started.as_millis());
        if !run.overrun_sent && elapsed > millis(limit) {
            run.overrun_sent = true;
            self.emitter.emit(EventKind::Overrun { elapsed_ms: elapsed });
        }
    }

    /// Compacts the history and opens a new segment. `Some` when the turn must stop.
    async fn compact(&mut self, run: &TurnRun) -> Option<Finish> {
        let compacted = run
            .cancel
            .guard(self.strategy.compact(self.state.history(), None))
            .await;
        match compacted {
            None => Some(Finish::cancelled()),
            Some(Err(_error)) => Some(Finish::failed()),
            Some(Ok(messages)) => {
                let chars = context_chars(&self.context).saturating_add(messages_chars(&messages));
                for message in messages {
                    self.state.push_message(message);
                }
                self.state.push_segment(Segment::new(
                    BoundaryKind::Compaction,
                    self.context.clone(),
                    self.config.clock.now(),
                ));
                let tokens = self.tokens.estimate(chars);
                self.tokens.compacted(tokens);
                None
            }
        }
    }

    /// The request for the model: the segment context, the messages of the segment and the replayed reasoning, with the
    /// estimate of its size in tokens.
    fn build_request(&self) -> (InferenceRequest, u64) {
        let context = self
            .state
            .segments()
            .last()
            .map_or(&self.context, Segment::context);
        let messages = segment_messages(self.state.history());
        let reasoning = replayed_reasoning(messages, self.profile.replay())
            .into_iter()
            .map(Option::<&Reasoning>::cloned)
            .collect();
        let chars = context_chars(context).saturating_add(messages_chars(messages));
        let request = InferenceRequest::new(
            self.profile.clone(),
            context.system_prompt().to_owned(),
            messages.to_vec(),
            context.tool_definitions().to_vec(),
            reasoning,
        );
        (request, self.tokens.estimate(chars))
    }

    /// Calls the model, retrying a transient error after a backoff and compacting once on a context overflow.
    async fn infer(&mut self, run: &mut TurnRun) -> Inference {
        let mut compacted_for_overflow = false;
        loop {
            if run.cancel.is_cancelled() {
                return Inference::End(Finish::cancelled());
            }
            let (request, estimated) = self.build_request();
            run.calls = run.calls.saturating_add(1);
            let error = match self.attempt(run, request).await {
                Attempt::Cancelled => return Inference::End(Finish::cancelled()),
                Attempt::Response(response) => {
                    run.backoff.reset();
                    self.record_usage(&response, estimated);
                    return Inference::Response(response);
                }
                Attempt::Error(error) => error,
            };
            let class = error.class();
            if let Some(finish) = self.recover(run, class, &mut compacted_for_overflow).await {
                return Inference::End(finish);
            }
        }
    }

    /// Reacts to the failure of a model call: wait before a retry, compact once on an overflow. `Some` when the turn
    /// stops, `None` when the call is made again.
    async fn recover(
        &mut self,
        run: &mut TurnRun,
        class: ErrorClass,
        compacted: &mut bool,
    ) -> Option<Finish> {
        match class {
            ErrorClass::Transient => self.wait_to_retry(run, class).await,
            ErrorClass::ContextOverflow if !*compacted => {
                *compacted = true;
                self.compact(run).await
            }
            ErrorClass::ContextOverflow | ErrorClass::Permanent => Some(Finish::failed()),
        }
    }

    /// One call to the model, streaming its text as masked fragments.
    async fn attempt(&self, run: &TurnRun, request: InferenceRequest) -> Attempt {
        let mut masker = Masker::new(&self.config.redactor);
        let (emitter, turn) = (&self.emitter, run.turn);
        let call = self.provider.stream(request, |fragment: &str| {
            if let Some(text) = masker.push(fragment) {
                emitter.emit(EventKind::TextDelta { turn, text });
            }
        });
        let Some(result) = run.cancel.guard(call).await else {
            return Attempt::Cancelled;
        };
        match result {
            Ok(response) => {
                if let Some(text) = masker.finish() {
                    emitter.emit(EventKind::TextDelta { turn, text });
                }
                Attempt::Response(response)
            }
            Err(error) => Attempt::Error(error),
        }
    }

    /// Emits `InferenceRetry` and waits for the next delay of the schedule. `Some` when the wait was cancelled.
    async fn wait_to_retry(&self, run: &mut TurnRun, class: ErrorClass) -> Option<Finish> {
        let delay = run.backoff.next_delay();
        self.emitter.emit(EventKind::InferenceRetry {
            attempt: run.backoff.attempts(),
            delay_ms: millis(delay),
            error_class: class_name(class).to_owned(),
        });
        match run.cancel.sleep(delay).await {
            SleepOutcome::Elapsed => None,
            SleepOutcome::Cancelled => Some(Finish::cancelled()),
        }
    }

    /// Updates the token accounting with the usage of a response, reported or estimated.
    fn record_usage(&mut self, response: &InferenceResponse, estimated_prompt: u64) {
        self.tokens
            .record(&self.agent, response.usage(), estimated_prompt);
        let (answer, reasoning) = match response.message() {
            Message::Assistant {
                content, reasoning, ..
            } => (
                chars_of(content),
                reasoning.as_ref().map_or(0, |thought| chars_of(thought.text())),
            ),
            Message::System { .. }
            | Message::User { .. }
            | Message::ToolResult { .. }
            | Message::Boundary { .. }
            | Message::Discarded { .. } => (0, 0),
        };
        let (prompt, completion) = response.usage().map_or_else(
            || (estimated_prompt, self.tokens.estimate(answer)),
            |reported| (reported.prompt(), reported.completion()),
        );
        self.emitter.emit(EventKind::TokenUsage { prompt, completion });
        let reasoning_tokens = response
            .usage()
            .and_then(|reported| reported.reasoning())
            .unwrap_or_else(|| self.tokens.estimate(reasoning));
        self.state
            .add_tokens(TokenCounters::new(prompt, completion, reasoning_tokens));
    }

    /// Masks a response of the model before it enters the history.
    fn mask_message(&self, content: &str, reasoning: Option<&Reasoning>, calls: &[ToolCall]) -> Message {
        let redactor = &self.config.redactor;
        Message::assistant(
            redactor.redact(content),
            reasoning.map(|thought| Reasoning::new(thought.emitter(), redactor.redact(thought.text()))),
            calls.iter().map(|call| self.mask_call(call)).collect(),
        )
    }

    /// Masks the identifier, the name and the arguments of a tool call.
    fn mask_call(&self, call: &ToolCall) -> ToolCall {
        let redactor = &self.config.redactor;
        ToolCall::new(
            redactor.redact(call.id()),
            redactor.redact(call.name()),
            redactor.redact_json(call.arguments()),
        )
    }

    /// Appends the response to the history, then runs its tool calls. `Some` when the turn ends.
    async fn handle_response(
        &mut self,
        run: &mut TurnRun,
        response: &InferenceResponse,
    ) -> SessionResult<Option<Finish>> {
        let Message::Assistant {
            content,
            reasoning,
            tool_calls,
        } = response.message()
        else {
            return Ok(Some(Finish::failed()));
        };
        if response.finish_reason() == Some(FinishReason::Length) {
            let (has_text, has_tool_calls) = (!content.is_empty(), !tool_calls.is_empty());
            self.emitter.emit(EventKind::ResponseTruncated {
                has_text,
                has_tool_calls,
            });
            if !has_text && !has_tool_calls {
                return Ok(Some(Finish {
                    outcome: Outcome::Failed,
                    response: CUT_MESSAGE.to_owned(),
                }));
            }
        }
        let message = self.mask_message(content, reasoning.as_ref(), tool_calls);
        self.tokens.added(self.tokens.estimate(message_chars(&message)));
        let masked_content = self.config.redactor.redact(content);
        self.state.push_message(message);
        if tool_calls.is_empty() {
            return Ok(Some(Finish::valid(masked_content)));
        }
        if self.run_tools(run, tool_calls).await {
            return Ok(Some(Finish::cancelled()));
        }
        self.drain_inbox()?;
        Ok(None)
    }

    /// Runs the tool calls in order and appends their results. Returns whether the turn was cancelled: the calls not run
    /// then get a result that says so, so that no call stays without result.
    async fn run_tools(&mut self, run: &mut TurnRun, calls: &[ToolCall]) -> bool {
        let mut cancelled = false;
        for call in calls {
            let masked = self.mask_call(call);
            let output = if cancelled || run.cancel.is_cancelled() {
                ToolOutput {
                    text: CANCELLED_RESULT.to_owned(),
                    original_chars: None,
                    cancelled: true,
                }
            } else {
                self.execute(run, masked.name(), call.arguments()).await
            };
            cancelled = cancelled || output.cancelled;
            self.apply_session_edits();
            let result = Message::tool_result(masked.id(), masked.name(), output.text, output.original_chars);
            self.tokens.added(self.tokens.estimate(message_chars(&result)));
            self.state.push_message(result);
        }
        cancelled
    }

    /// Empties the cell of session edits and applies what it held: the title, then the review flags in order. The texts
    /// are masked and bounded here; each change emits its event, dated by the shared clock.
    fn apply_session_edits(&mut self) {
        let Some(edits) = &self.edits else {
            return;
        };
        let pending = edits.take();
        let redactor = &self.config.redactor;
        if let Some(raw) = pending.title()
            && let Some(stored) = self.state.set_title_masked(raw, redactor)
        {
            self.emitter.emit(EventKind::TitleChanged {
                title: stored.to_owned(),
            });
        }
        for raw in pending.reviews() {
            let reason = self
                .state
                .add_review_requested(raw, redactor, &*self.config.clock);
            self.emitter.emit(EventKind::ReviewRequested { reason });
        }
    }

    /// Runs one tool call: `rhai_pad` in the scratchpad, a registered native tool without it, any other name is an error.
    async fn execute(&mut self, run: &mut TurnRun, name: &str, arguments: &Value) -> ToolOutput {
        if name == RHAI_PAD_TOOL {
            return match arguments.get("script").and_then(Value::as_str) {
                Some(script) => self.run_script(run, script).await,
                None => ToolOutput::error(MISSING_SCRIPT.to_owned()),
            };
        }
        match self.natives.get(name) {
            Some(tool) => self.run_native(tool.as_ref(), arguments, run).await,
            None => ToolOutput::error(format!("unknown tool `{name}`")),
        }
    }

    /// Runs a native tool under the cancellation of the turn: its text, or its error text, is masked by the redactor, then
    /// capped by the render cap of the budget, as the output of a script is. A turn cancelled during the call ends it.
    async fn run_native(&self, tool: &dyn NativeTool, arguments: &Value, run: &TurnRun) -> ToolOutput {
        let Some(result) = run.cancel.guard(tool.call(arguments, &run.cancel.merged)).await else {
            return ToolOutput {
                text: CANCELLED_RESULT.to_owned(),
                original_chars: None,
                cancelled: true,
            };
        };
        let (Ok(text) | Err(text)) = result;
        let masked = self.config.redactor.redact(&text);
        let cap = usize::try_from(self.config.budget.render_cap_chars()).unwrap_or(usize::MAX);
        let rendered = truncate_middle(&masked, cap);
        ToolOutput {
            text: rendered.text().to_owned(),
            original_chars: rendered
                .truncated()
                .then(|| u64::try_from(rendered.original_chars()).unwrap_or(u64::MAX)),
            cancelled: false,
        }
    }

    /// Runs a script in the scratchpad, reports its audit record and keeps the persistable pad in the state.
    async fn run_script(&mut self, run: &mut TurnRun, script: &str) -> ToolOutput {
        let env = RunEnv::new(&self.agent, &self.config.redactor, &*self.config.clock);
        let outcome = self.pad.run(script, &run.cancel.merged, &env).await;
        self.emitter.emit(EventKind::Script(outcome.record().clone()));
        let pad = outcome.snapshot();
        self.state
            .set_pad(pad.variables().clone(), pad.functions().clone());
        if matches!(outcome.status(), RunStatus::Failed(_)) {
            run.rhai_errors = run.rhai_errors.saturating_add(1);
        }
        ToolOutput {
            text: outcome.text().to_owned(),
            original_chars: outcome
                .truncated()
                .then(|| u64::try_from(outcome.record().original_len).unwrap_or(u64::MAX)),
            cancelled: matches!(outcome.status(), RunStatus::Cancelled),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::{Masker, RHAI_PAD_TOOL, TurnConfig, TurnOutput, TurnRunner, UserInbox};
    use super::{message_chars, messages_chars};
    use crate::agents::{
        ErrorClass, FinishReason, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse,
        ModelProfile, ReportedUsage, ScriptedProvider,
    };
    use crate::clock::{ManualClock, Timestamp};
    use crate::context::{
        Budget, ContextError, ContextResult, DefaultStrategy, Digest, ReplayMode, Summarizer,
    };
    use crate::scratchpad::{Grants, Scratchpad, ScratchpadEngine, ScratchpadLimits};
    use crate::session::{
        AgentRef, BoundaryKind, CancelHandle, DiscardCause, EventKind, InMemoryStore, LabelAuthor, Message,
        ModelSnapshot, NativeTool, Outcome, Reasoning, RecordingSink, Redactor, ReviewState, Segment,
        SegmentContext, SessionEdits, SessionError, SessionId, SessionResult, SessionState, SessionStore,
        TaggedValue, ToolCall, TurnId, review_state,
    };

    // --- doubles -------------------------------------------------------------------------------------------------

    impl InferenceProvider for Arc<ScriptedProvider> {
        fn complete(
            &self,
            request: InferenceRequest,
        ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send {
            self.as_ref().complete(request)
        }

        fn stream<F>(
            &self,
            request: InferenceRequest,
            fragments: F,
        ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send
        where
            F: FnMut(&str) + Send,
        {
            self.as_ref().stream(request, fragments)
        }
    }

    impl SessionStore for Arc<InMemoryStore> {
        fn load(&self, id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send {
            self.as_ref().load(id)
        }

        fn save(&self, state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send {
            self.as_ref().save(state)
        }
    }

    /// A provider that never answers, and tells when it was called.
    struct Hanging {
        called: Arc<Notify>,
    }

    impl InferenceProvider for Hanging {
        fn complete(
            &self,
            _request: InferenceRequest,
        ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send {
            self.called.notify_one();
            std::future::pending()
        }
    }

    /// A provider that answers `done` once released, and tells when it was called.
    struct Gated {
        called: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl Gated {
        async fn complete_gated(&self) -> Reply {
            self.called.notify_one();
            self.release.notified().await;
            say("done")
        }
    }

    impl InferenceProvider for Gated {
        fn complete(
            &self,
            _request: InferenceRequest,
        ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send {
            self.complete_gated()
        }
    }

    /// A store whose save fails.
    struct BrokenStore;

    impl SessionStore for BrokenStore {
        fn load(&self, _id: &SessionId) -> impl Future<Output = SessionResult<Option<SessionState>>> + Send {
            std::future::ready(Ok(None))
        }

        fn save(&self, _state: &SessionState) -> impl Future<Output = SessionResult<()>> + Send {
            std::future::ready(Err(SessionError::Store("disk full".to_owned())))
        }
    }

    /// A summarizer that answers a fixed summary, or fails.
    struct FixedSummarizer {
        fails: bool,
        calls: Arc<AtomicUsize>,
    }

    impl Summarizer for FixedSummarizer {
        fn summarize(
            &self,
            _transcript: &str,
            _digest: Option<&Digest>,
        ) -> impl Future<Output = ContextResult<String>> + Send {
            let _before = self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(if self.fails {
                Err(ContextError::Summarizer("down".to_owned()))
            } else {
                Ok("the summary".to_owned())
            })
        }
    }

    // --- builders ------------------------------------------------------------------------------------------------

    type Reply = Result<InferenceResponse, InferenceError>;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "test helper: scripted replies are results, most of them failures"
    )]
    fn say(text: &str) -> Reply {
        Ok(InferenceResponse::new(
            Message::assistant(text, None, Vec::new()),
            None,
        ))
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "test helper: scripted replies are results, most of them failures"
    )]
    fn call_tool(id: &str, name: &str, arguments: Value) -> Reply {
        Ok(InferenceResponse::new(
            Message::assistant("", None, vec![ToolCall::new(id, name, arguments)]),
            None,
        ))
    }

    fn run_script(id: &str, script: &str) -> Reply {
        call_tool(id, RHAI_PAD_TOOL, json!({"script": script}))
    }

    fn fail(class: ErrorClass, text: &str) -> Reply {
        Err(InferenceError::new(class, text.to_owned()))
    }

    fn engine_with(registration: impl FnOnce(&mut rhai::Engine) + 'static) -> Arc<ScratchpadEngine> {
        let builder = ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new()).with_registration(
            move |engine| {
                registration(engine);
                Ok(())
            },
        );
        Arc::new(builder.build().expect("the engine must build"))
    }

    /// What a test changes in the default assembly.
    struct Options {
        session: &'static str,
        redactor: Redactor,
        budget: Budget,
        overrun: Option<Duration>,
        max_steps: Option<u32>,
        engine: Arc<ScratchpadEngine>,
        clock: Arc<ManualClock>,
        state: Option<SessionState>,
        replay: ReplayMode,
        summarizer_fails: bool,
        summarizer_calls: Arc<AtomicUsize>,
    }

    impl Options {
        fn named(session: &'static str) -> Self {
            Self {
                session,
                redactor: Redactor::new(),
                budget: Budget::new(100_000, 100, 1000, 1000),
                overrun: None,
                max_steps: None,
                engine: engine_with(|_engine| {}),
                clock: Arc::new(ManualClock::new(Timestamp::from_millis(0))),
                state: None,
                replay: ReplayMode::None,
                summarizer_fails: false,
                summarizer_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    /// The handles a test keeps on what the runner owns.
    struct Parts {
        sink: Arc<RecordingSink>,
        store: Arc<InMemoryStore>,
        root: CancelHandle,
    }

    type Strategy = DefaultStrategy<FixedSummarizer>;

    /// A runner and the handles a test keeps on what it owns.
    type Rig<P> = (TurnRunner<P, Strategy, Arc<InMemoryStore>>, Parts);

    fn new_state(session: &str) -> SessionState {
        SessionState::new(
            SessionId::new(session).expect("a session identifier"),
            AgentRef::new("main-agent", "fp-1"),
            "ctx",
            BTreeMap::from([("k8s".to_owned(), "1.0.0".to_owned())]),
            Timestamp::from_millis(0),
        )
    }

    fn segment_context() -> SegmentContext {
        SegmentContext::new(
            "system prompt",
            vec![json!({"name": "rhai_pad", "parameters": {"type": "object"}})],
            ModelSnapshot::new("model", ReplayMode::None),
        )
    }

    fn assemble<P: InferenceProvider>(provider: P, options: Options) -> Rig<P> {
        let sink = Arc::new(RecordingSink::new());
        let store = Arc::new(InMemoryStore::new());
        let root = CancelHandle::new();
        let profile = ModelProfile::new("profile-a".to_owned(), "prov".to_owned(), "model".to_owned())
            .with_context_size(100_000)
            .with_replay(options.replay)
            .resolve(None)
            .expect("the profile resolves");
        let clock = Arc::clone(&options.clock);
        let events = Arc::clone(&sink);
        let mut config = TurnConfig::new(clock, options.redactor, events, root.clone(), options.budget);
        if let Some(overrun) = options.overrun {
            config = config.with_overrun(overrun);
        }
        if let Some(max_steps) = options.max_steps {
            config = config.with_max_steps(max_steps);
        }
        let strategy = DefaultStrategy::new(
            FixedSummarizer {
                fails: options.summarizer_fails,
                calls: options.summarizer_calls,
            },
            ReplayMode::None,
        );
        let state = options.state.unwrap_or_else(|| new_state(options.session));
        let runner = TurnRunner::new(
            provider,
            strategy,
            profile,
            segment_context(),
            Scratchpad::new(options.engine),
            Arc::clone(&store),
            state,
            config,
        );
        (runner, Parts { sink, store, root })
    }

    fn delta_text(kind: EventKind) -> Option<String> {
        let EventKind::TextDelta { text, .. } = kind else {
            return None;
        };
        Some(text)
    }

    fn token_usage(kind: &EventKind) -> Option<(u64, u64)> {
        let EventKind::TokenUsage { prompt, completion } = kind else {
            return None;
        };
        Some((*prompt, *completion))
    }

    fn retry_delay(kind: &EventKind) -> Option<u64> {
        let EventKind::InferenceRetry { delay_ms, .. } = kind else {
            return None;
        };
        Some(*delay_ms)
    }

    fn overrun_elapsed(kind: &EventKind) -> Option<u64> {
        let EventKind::Overrun { elapsed_ms } = kind else {
            return None;
        };
        Some(*elapsed_ms)
    }

    fn tool_result_parts(message: Message) -> Option<(String, String, String)> {
        let Message::ToolResult {
            call_id,
            tool,
            content,
            ..
        } = message
        else {
            return None;
        };
        Some((call_id, tool, content))
    }

    fn kinds(parts: &Parts) -> Vec<EventKind> {
        parts.sink.events().into_iter().map(|event| event.kind).collect()
    }

    fn deltas(parts: &Parts) -> Vec<String> {
        kinds(parts).into_iter().filter_map(delta_text).collect()
    }

    async fn run(
        runner: &mut TurnRunner<impl InferenceProvider, Strategy, Arc<InMemoryStore>>,
        text: &str,
    ) -> TurnOutput {
        runner
            .run_turn(text, &CancelHandle::new())
            .await
            .expect("the turn must run")
    }

    fn roles(runner: &TurnRunner<impl InferenceProvider, Strategy, Arc<InMemoryStore>>) -> Vec<Message> {
        runner.state().history().messages().to_vec()
    }

    // --- scenarios -----------------------------------------------------------------------------------------------

    // Scenario: réponse sans outil
    #[tokio::test]
    async fn reply_without_tool() {
        let provider = Arc::new(ScriptedProvider::new(vec![say("bonjour")]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-reply"));
        let output = run(&mut runner, "salut").await;
        assert_eq!(output.outcome(), Outcome::Valid, "a plain answer is a valid turn");
        assert_eq!(
            output.response(),
            "bonjour",
            "the response is the answer of the model"
        );
        assert_eq!(output.calls(), 1, "one model call");
        assert_eq!(
            roles(&runner),
            [
                Message::user("salut"),
                Message::assistant("bonjour", None, Vec::new())
            ],
            "the history keeps the message and the answer"
        );
        let events = kinds(&parts);
        assert_eq!(
            events.first(),
            Some(&EventKind::TurnStarted { turn: TurnId::new(1) }),
            "the turn starts with TurnStarted"
        );
        assert_eq!(
            events.last(),
            Some(&EventKind::TurnEnded {
                turn: TurnId::new(1),
                outcome: Outcome::Valid,
                response: "bonjour".to_owned()
            }),
            "the turn ends with TurnEnded carrying the response"
        );
        assert_eq!(deltas(&parts), ["bonjour"], "the answer is streamed as a delta");
    }

    // Scenario: réponse sans outil (usage rapporté, ou estimé en repli)
    #[tokio::test]
    async fn token_usage_is_reported_then_estimated() {
        let reported = Ok(InferenceResponse::new(
            Message::assistant("one", None, Vec::new()),
            Some(ReportedUsage::new(120, 7)),
        ));
        let provider = Arc::new(ScriptedProvider::new(vec![reported, say("two")]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-usage"));
        let _first = run(&mut runner, "a").await;
        let _second = run(&mut runner, "b").await;
        let usages: Vec<(u64, u64)> = kinds(&parts)
            .into_iter()
            .filter_map(|kind| token_usage(&kind))
            .collect();
        assert_eq!(usages.len(), 2, "one TokenUsage per model call");
        assert_eq!(
            usages.first(),
            Some(&(120, 7)),
            "the reported usage is used as is"
        );
        assert!(
            usages
                .get(1)
                .is_some_and(|&(prompt, completion)| prompt > 0 && completion > 0),
            "an estimate replaces the missing report, got {usages:?}"
        );
        let tokens = runner.state().tokens();
        assert_eq!(
            tokens.prompt(),
            120u64.saturating_add(usages.get(1).map_or(0, |u| u.0))
        );
        assert!(tokens.completion() >= 7, "the counters add the completion tokens");
    }

    // Scenario: appel de rhai_pad puis réponse
    #[tokio::test]
    async fn rhai_pad_call_then_answer() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "let x = 2 + 3; x"),
            say("x is five"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-pad"));
        let output = run(&mut runner, "compute").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        assert_eq!(output.calls(), 2, "two model calls");
        let messages = roles(&runner);
        assert_eq!(
            messages.get(2),
            Some(&Message::tool_result("c1", RHAI_PAD_TOOL, "5", None)),
            "the result 5 follows the call in the history"
        );
        let requests = provider.requests();
        assert_eq!(
            requests.get(1).map(|request| request.messages().len()),
            Some(3),
            "the second request holds the message, the call and its result"
        );
        assert!(
            requests
                .get(1)
                .is_some_and(|request| request.messages().contains(&Message::tool_result(
                    "c1",
                    RHAI_PAD_TOOL,
                    "5",
                    None
                ))),
            "the result is in the next request"
        );
        let scripts = kinds(&parts)
            .into_iter()
            .filter(|kind| matches!(kind, EventKind::Script(record) if record.script == "let x = 2 + 3; x" && record.result == "5"))
            .count();
        assert_eq!(scripts, 1, "one Script event for the script");
        assert_eq!(
            runner.state().variables().get("x"),
            Some(&TaggedValue::Int(5)),
            "the variable x is in the state"
        );
    }

    // Scenario: erreur transitoire réessayée
    #[tokio::test(start_paused = true)]
    async fn transient_error_is_retried() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            fail(ErrorClass::Transient, "upstream exploded"),
            say("recovered"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-retry"));
        let started = tokio::time::Instant::now();
        let output = run(&mut runner, "hello").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the retry succeeds");
        assert_eq!(
            output.response(),
            "recovered",
            "the response comes after the retry"
        );
        assert_eq!(output.calls(), 2, "two model calls");
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "the loop waited for the backoff delay"
        );
        assert!(
            kinds(&parts).contains(&EventKind::InferenceRetry {
                attempt: 1,
                delay_ms: 1000,
                error_class: "transient".to_owned()
            }),
            "one InferenceRetry of 1000 ms"
        );
        assert_eq!(
            roles(&runner),
            [
                Message::user("hello"),
                Message::assistant("recovered", None, Vec::new())
            ],
            "no error message in the history"
        );
    }

    // Scenario: erreur transitoire réessayée (le calendrier repart de zéro après un succès)
    #[tokio::test(start_paused = true)]
    async fn backoff_resets_after_a_success() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            fail(ErrorClass::Transient, "x"),
            fail(ErrorClass::Transient, "x"),
            run_script("c1", "1"),
            fail(ErrorClass::Transient, "x"),
            say("end"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-reset"));
        let output = run(&mut runner, "hello").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        let delays: Vec<u64> = kinds(&parts)
            .into_iter()
            .filter_map(|kind| retry_delay(&kind))
            .collect();
        assert_eq!(
            delays,
            [1000, 2000, 1000],
            "the delays double, then restart after a success"
        );
    }

    // Scenario: annulation pendant le backoff
    #[tokio::test(start_paused = true)]
    async fn cancel_during_backoff() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            fail(ErrorClass::Transient, "x"),
            fail(ErrorClass::Transient, "x"),
            fail(ErrorClass::Transient, "x"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-cancel-backoff"));
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let started = tokio::time::Instant::now();
        let (output, ()) = tokio::join!(runner.run_turn("hello", &cancel), async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            trigger.cancel();
        });
        let output = output.expect("a cancelled turn is not an error");
        assert_eq!(output.outcome(), Outcome::Cancelled, "the turn is cancelled");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the wait ended with the cancellation, not with the delay"
        );
        assert_eq!(
            roles(&runner),
            [Message::user("hello")],
            "only the incoming message stays"
        );
        assert_eq!(provider.requests().len(), 1, "no call after the cancellation");
        assert!(
            kinds(&parts).iter().any(|kind| matches!(
                kind,
                EventKind::TurnEnded {
                    outcome: Outcome::Cancelled,
                    ..
                }
            )),
            "TurnEnded reports the cancellation"
        );
    }

    // Scenario: erreur permanente
    #[tokio::test]
    async fn permanent_error() {
        let provider = Arc::new(ScriptedProvider::new(vec![fail(
            ErrorClass::Permanent,
            "bad request details",
        )]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-permanent"));
        let output = run(&mut runner, "hello").await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "a permanent error fails the turn"
        );
        assert_eq!(output.response(), "", "no response after a failure");
        assert_eq!(
            roles(&runner),
            [Message::user("hello")],
            "the history does not carry the error"
        );
        assert!(
            kinds(&parts).iter().any(|kind| matches!(
                kind,
                EventKind::TurnEnded {
                    outcome: Outcome::Failed,
                    ..
                }
            )),
            "TurnEnded is emitted"
        );
        let saved = parts
            .store
            .load(runner.state().id())
            .await
            .expect("load")
            .expect("saved");
        assert!(
            !saved.to_json().expect("json").contains("bad request"),
            "the error text is nowhere in the saved state"
        );
    }

    // Scenario: débordement de contexte
    #[tokio::test]
    async fn context_overflow_compacts_and_retries_once() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            fail(ErrorClass::ContextOverflow, "too long"),
            say("fits now"),
        ]));
        let options = Options::named("t-overflow");
        let summaries = Arc::clone(&options.summarizer_calls);
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "hello").await;
        assert_eq!(
            output.outcome(),
            Outcome::Valid,
            "the retry after the compaction succeeds"
        );
        assert_eq!(output.response(), "fits now", "the answer of the second call");
        assert_eq!(provider.requests().len(), 2, "one new call only");
        assert_eq!(summaries.load(Ordering::SeqCst), 1, "one compaction");
        let boundaries = runner
            .state()
            .segments()
            .iter()
            .filter(|segment| segment.boundary() == BoundaryKind::Compaction)
            .count();
        assert_eq!(boundaries, 1, "a Compaction segment was opened");
        assert!(
            provider
                .requests()
                .get(1)
                .is_some_and(|request| request.messages().first().is_some_and(
                    |first| matches!(first, Message::User { content } if content.contains("the summary"))
                )),
            "the second request starts with the summary"
        );
    }

    // Scenario: débordement de contexte (le second débordement échoue)
    #[tokio::test]
    async fn second_overflow_fails() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            fail(ErrorClass::ContextOverflow, "too long"),
            fail(ErrorClass::ContextOverflow, "still too long"),
            say("never"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-overflow-twice"));
        let output = run(&mut runner, "hello").await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "a second overflow fails the turn"
        );
        assert_eq!(provider.requests().len(), 2, "no third call");
        assert_eq!(output.calls(), 2, "two model calls are reported");
    }

    // Scenario: compaction avant requête
    #[tokio::test]
    async fn compaction_before_request() {
        let mut state = new_state("t-compaction");
        for index in 0..4 {
            state.push_message(Message::user(format!("question {index} {}", "x".repeat(400))));
            state.push_message(Message::assistant("a".repeat(400), None, Vec::new()));
        }
        let provider = Arc::new(ScriptedProvider::new(vec![say("ok")]));
        let options = Options {
            state: Some(state),
            budget: Budget::new(1000, 50, 100, 100),
            ..Options::named("t-compaction")
        };
        let summaries = Arc::clone(&options.summarizer_calls);
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "last question").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        assert_eq!(summaries.load(Ordering::SeqCst), 1, "one compaction");
        let kinds: Vec<BoundaryKind> = runner.state().segments().iter().map(Segment::boundary).collect();
        assert_eq!(
            kinds,
            [BoundaryKind::Start, BoundaryKind::Compaction],
            "a Compaction segment opens after the Start one"
        );
        let requests = provider.requests();
        let messages = requests
            .first()
            .map(InferenceRequest::messages)
            .unwrap_or_default();
        assert_eq!(
            messages.len(),
            2,
            "the request holds the summary and the new message only"
        );
        assert!(
            matches!(messages.first(), Some(Message::User { content }) if content.contains("the summary")),
            "the summary comes first"
        );
        assert_eq!(
            messages.get(1),
            Some(&Message::user("last question")),
            "then the new message"
        );
    }

    // Scenario: compaction avant requête (échec du résumé)
    #[tokio::test]
    async fn failed_summary_fails_the_turn() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            fail(ErrorClass::ContextOverflow, "too long"),
            say("never"),
        ]));
        let options = Options {
            summarizer_fails: true,
            ..Options::named("t-summary-fails")
        };
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "hello").await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "a summary that fails fails the turn"
        );
        assert_eq!(
            provider.requests().len(),
            1,
            "no call after the failed compaction"
        );
    }

    // Scenario: compaction avant requête (message d'entrée trop gros)
    #[tokio::test]
    async fn oversized_input_is_refused_without_a_call() {
        let provider = Arc::new(ScriptedProvider::new(vec![say("never")]));
        let options = Options {
            budget: Budget::new(1000, 50, 100, 100),
            ..Options::named("t-oversized")
        };
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, &"y".repeat(10_000)).await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "an oversized message is refused"
        );
        assert_eq!(output.calls(), 0, "no model call");
        assert!(provider.requests().is_empty(), "the provider was not called");
        assert!(
            roles(&runner).is_empty(),
            "the refused message is not in the history"
        );
        assert!(
            kinds(&parts).iter().any(|kind| matches!(
                kind,
                EventKind::TurnEnded {
                    outcome: Outcome::Failed,
                    ..
                }
            )),
            "TurnEnded is emitted"
        );
    }

    /// Sends a message to the inbox once it is known, as a script of the turn would.
    fn poke(cell: &OnceLock<UserInbox>) {
        if let Some(inbox) = cell.get() {
            inbox.send("hello from the user");
        }
    }

    // Scenario: message utilisateur pendant un script
    #[tokio::test]
    async fn user_message_during_a_script_waits_for_the_result() {
        let cell: Arc<OnceLock<UserInbox>> = Arc::new(OnceLock::new());
        let native = Arc::clone(&cell);
        let options = Options {
            engine: engine_with(move |engine| {
                let _poke = engine.register_fn("poke", move || poke(&native));
            }),
            ..Options::named("t-inbox")
        };
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "poke(); 7"),
            say("seen"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        assert!(
            cell.set(runner.inbox()).is_ok(),
            "the inbox is handed to the native function"
        );
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        assert_eq!(
            roles(&runner),
            [
                Message::user("go"),
                Message::assistant(
                    "",
                    None,
                    vec![ToolCall::new("c1", RHAI_PAD_TOOL, json!({"script": "poke(); 7"}))]
                ),
                Message::tool_result("c1", RHAI_PAD_TOOL, "7", None),
                Message::user("hello from the user"),
                Message::assistant("seen", None, Vec::new()),
            ],
            "the user message follows the result, never the call"
        );
        let requests = provider.requests();
        assert_eq!(
            requests.get(1).and_then(|request| request.messages().last()),
            Some(&Message::user("hello from the user")),
            "the next request carries the message after the result"
        );
    }

    // Scenario: un seul tour actif
    #[tokio::test]
    async fn only_one_active_turn() {
        let called = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let gated = Gated {
            called: Arc::clone(&called),
            release: Arc::clone(&release),
        };
        let (mut first, _first_parts) = assemble(gated, Options::named("t-single"));
        let task = tokio::spawn(async move {
            let output = first.run_turn("first", &CancelHandle::new()).await;
            (first, output)
        });
        called.notified().await;
        let provider = Arc::new(ScriptedProvider::new(vec![say("second")]));
        let (mut second, _second_parts) = assemble(Arc::clone(&provider), Options::named("t-single"));
        let refused = second.run_turn("second", &CancelHandle::new()).await;
        assert!(
            matches!(refused, Err(SessionError::TurnActive(_))),
            "a second turn on the same session is refused, got {refused:?}"
        );
        assert!(
            second.state().history().messages().is_empty(),
            "the refused turn changed nothing"
        );
        assert!(provider.requests().is_empty(), "the refused turn made no call");
        release.notify_one();
        let (finished, output) = task.await.expect("the first turn task");
        assert_eq!(
            output.expect("first turn").outcome(),
            Outcome::Valid,
            "the first turn completes"
        );
        assert_eq!(
            finished.state().history().messages().len(),
            2,
            "the state of the running turn is intact"
        );
        let again = second.run_turn("second", &CancelHandle::new()).await;
        assert!(again.is_ok(), "the permit is released at the end of the turn");
    }

    // Scenario: un seul tour actif (le permis est relâché après annulation et échec du stockage)
    #[tokio::test]
    async fn permit_is_released_after_a_store_failure() {
        let provider = Arc::new(ScriptedProvider::new(vec![say("one"), say("two")]));
        let sink = Arc::new(RecordingSink::new());
        let options = Options::named("t-store-failure");
        let profile = ModelProfile::new("p".to_owned(), "prov".to_owned(), "m".to_owned())
            .with_context_size(100_000)
            .resolve(None)
            .expect("profile");
        let clock = Arc::clone(&options.clock);
        let events = Arc::clone(&sink);
        let config = TurnConfig::new(
            clock,
            Redactor::new(),
            events,
            CancelHandle::new(),
            options.budget,
        );
        let strategy = DefaultStrategy::new(
            FixedSummarizer {
                fails: false,
                calls: Arc::new(AtomicUsize::new(0)),
            },
            ReplayMode::None,
        );
        let mut runner = TurnRunner::new(
            Arc::clone(&provider),
            strategy,
            profile,
            segment_context(),
            Scratchpad::new(options.engine),
            BrokenStore,
            new_state("t-store-failure"),
            config,
        );
        let first = runner.run_turn("a", &CancelHandle::new()).await;
        assert!(
            matches!(first, Err(SessionError::Store(_))),
            "a failed save is rendered, got {first:?}"
        );
        assert!(
            sink.events().iter().any(|event| matches!(
                event.kind,
                EventKind::TurnEnded {
                    outcome: Outcome::Valid,
                    ..
                }
            )),
            "the turn still ended"
        );
        let second = runner.run_turn("b", &CancelHandle::new()).await;
        assert!(
            !matches!(second, Err(SessionError::TurnActive(_))),
            "the permit was released, got {second:?}"
        );
    }

    // Scenario: outil inconnu
    #[tokio::test]
    async fn unknown_tool_returns_an_error_result() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "inconnu", json!({})),
            say("ok"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-unknown"));
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn continues");
        let result = roles(&runner).into_iter().find_map(tool_result_parts);
        let (call_id, tool, content) = result.expect("a result for the unknown call");
        assert_eq!(call_id, "c1", "the result answers the call");
        assert_eq!(tool, "inconnu", "the result names the tool");
        assert!(
            content.contains("inconnu"),
            "the error names the tool, got {content}"
        );
    }

    // Scenario: outil inconnu (rhai_pad sans argument script)
    #[tokio::test]
    async fn rhai_pad_without_script_returns_an_error_result() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", RHAI_PAD_TOOL, json!({"code": "1"})),
            say("ok"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-no-script"));
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn continues");
        assert!(
            roles(&runner).iter().any(
                |message| matches!(message, Message::ToolResult { content, .. } if content.contains("script"))
            ),
            "the result tells that the script argument is missing"
        );
    }

    // Scenario: secret absent des sorties
    #[tokio::test]
    async fn secret_is_absent_from_every_output() {
        let mut redactor = Redactor::new();
        redactor.add_secret("s3cr3t-token").expect("secret");
        let reply = Ok(InferenceResponse::new(
            Message::assistant(
                "The key is s3cr3t-token, ok",
                Some(Reasoning::new("model", "thinking about s3cr3t-token")),
                Vec::new(),
            ),
            None,
        ));
        let provider = Arc::new(ScriptedProvider::new(vec![reply]).with_fragments(vec![vec![
            "The key is s3cr".to_owned(),
            "3t-tok".to_owned(),
            "en, ok".to_owned(),
        ]]));
        let options = Options {
            redactor,
            ..Options::named("t-secret")
        };
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "tell me").await;
        assert_eq!(
            output.response(),
            "The key is *redacted secret*, ok",
            "the response is masked"
        );
        let streamed = deltas(&parts).concat();
        assert_eq!(
            streamed, "The key is *redacted secret*, ok",
            "the deltas, joined, are masked"
        );
        assert!(
            deltas(&parts).iter().all(|delta| !delta.contains("s3cr")),
            "no delta holds a piece of the secret, got {:?}",
            deltas(&parts)
        );
        let state_text = runner.state().to_json().expect("json");
        assert!(
            !state_text.contains("s3cr3t-token"),
            "the history holds no secret"
        );
        assert!(
            kinds(&parts).iter().any(|kind| matches!(kind, EventKind::TurnEnded { response, .. } if response == "The key is *redacted secret*, ok")),
            "TurnEnded is masked"
        );
    }

    // Scenario: secret absent des sorties (l'écho d'un script et les appels d'outil)
    #[tokio::test]
    async fn secret_is_masked_in_tool_calls_and_results() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123456").expect("secret");
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "\"Bearer tok-123456\""),
            say("done"),
        ]));
        let options = Options {
            redactor,
            ..Options::named("t-secret-tools")
        };
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        let state_text = runner.state().to_json().expect("json");
        assert!(!state_text.contains("tok-123456"), "no secret in the history");
        let requests = provider.requests();
        let second = requests.get(1).expect("second request");
        assert!(
            !format!("{:?}", second.messages()).contains("tok-123456"),
            "no secret in what the model receives"
        );
        assert!(
            !format!("{:?}", kinds(&parts)).contains("tok-123456"),
            "no secret in the events"
        );
    }

    /// A native tool of the tests: it answers its `text` argument, or `fixed` when one is given.
    struct Native {
        name: &'static str,
        fixed: Option<String>,
        failure: Option<String>,
        wait_for_cancel: bool,
    }

    impl Native {
        /// The result of the call: the cancellation, a failure, the fixed text or the `text` argument.
        async fn answer(&self, arguments: &Value, cancel: &CancelHandle) -> Result<String, String> {
            if self.wait_for_cancel {
                cancel.cancelled().await;
                return Err("stopped by the cancellation".to_owned());
            }
            if let Some(failure) = &self.failure {
                return Err(failure.clone());
            }
            let text = arguments.get("text").and_then(Value::as_str).unwrap_or_default();
            Ok(self.fixed.clone().unwrap_or_else(|| text.to_owned()))
        }

        fn echo(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                fixed: None,
                failure: None,
                wait_for_cancel: false,
            })
        }

        /// A tool that fails with `text`.
        fn failing(name: &'static str, text: &str) -> Arc<Self> {
            Arc::new(Self {
                name,
                fixed: None,
                failure: Some(text.to_owned()),
                wait_for_cancel: false,
            })
        }

        /// A tool that waits for the cancellation of the turn handed to it.
        fn waiting(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                fixed: None,
                failure: None,
                wait_for_cancel: true,
            })
        }

        fn fixed(name: &'static str, text: &str) -> Arc<Self> {
            Arc::new(Self {
                name,
                fixed: Some(text.to_owned()),
                failure: None,
                wait_for_cancel: false,
            })
        }
    }

    impl NativeTool for Native {
        fn name(&self) -> &str {
            self.name
        }

        fn definition(&self) -> Value {
            json!({"name": self.name, "description": "a test tool", "parameters": {"type": "object"}})
        }

        fn call<'call>(
            &'call self,
            arguments: &'call Value,
            cancel: &'call CancelHandle,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'call>> {
            Box::pin(self.answer(arguments, cancel))
        }
    }

    /// The contents of the tool results of the history, in order.
    fn tool_results(
        runner: &TurnRunner<impl InferenceProvider, Strategy, Arc<InMemoryStore>>,
    ) -> Vec<String> {
        roles(runner)
            .into_iter()
            .filter_map(tool_result_parts)
            .map(|(_, _, content)| content)
            .collect()
    }

    // Scenario: un tool natif enregistré est appelé sans scratchpad
    #[tokio::test]
    async fn native_tool_is_called_without_scratchpad() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "echo", json!({"text": "a"})),
            say("done"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-native-echo"));
        assert!(
            runner.with_native_tool(Native::echo("echo")).is_ok(),
            "the native tool is registered"
        );
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        assert_eq!(
            tool_results(&runner),
            vec!["a".to_owned()],
            "the result is in the history"
        );
        assert!(
            !kinds(&parts)
                .iter()
                .any(|kind| matches!(kind, EventKind::Script(_))),
            "no script ran"
        );
        assert_eq!(output.response(), "done", "the model receives the final answer");
    }

    // Must (native_tool.sdd #2) : le texte d'erreur d'un tool natif est rendu au modèle comme résultat d'outil
    #[tokio::test]
    async fn native_tool_error_is_given_to_the_model_as_a_result() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "broken", json!({})),
            say("done"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-native-error"));
        assert!(
            runner
                .with_native_tool(Native::failing("broken", "it broke"))
                .is_ok(),
            "the native tool is registered"
        );
        let output = run(&mut runner, "go").await;
        assert_eq!(
            output.outcome(),
            Outcome::Valid,
            "the turn goes on after the error"
        );
        assert_eq!(
            tool_results(&runner),
            vec!["it broke".to_owned()],
            "the error text is the result"
        );
    }

    // Must (native_tool.sdd #2) : l'annulation du tour est honorée pendant l'appel d'un tool natif
    #[tokio::test]
    async fn cancel_during_a_native_tool_keeps_the_history_valid() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "slow", json!({})),
            say("never"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-cancel-native"));
        assert!(
            runner.with_native_tool(Native::waiting("slow")).is_ok(),
            "the native tool is registered"
        );
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let (output, ()) = tokio::join!(runner.run_turn("go", &cancel), async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });
        let output = output.expect("a cancelled turn is not an error");
        assert_eq!(output.outcome(), Outcome::Cancelled, "the turn is cancelled");
        assert_eq!(
            tool_results(&runner),
            vec!["tool call cancelled".to_owned()],
            "the call has a result that says it was cancelled"
        );
        assert_eq!(provider.requests().len(), 1, "no request after the cancellation");
    }

    // Scenario: un tool natif ne rend jamais un secret déclaré
    #[tokio::test]
    async fn native_tool_never_returns_a_declared_secret() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123456").expect("secret");
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "leak", json!({})),
            say("done"),
        ]));
        let options = Options {
            redactor,
            ..Options::named("t-native-secret")
        };
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        assert!(
            runner
                .with_native_tool(Native::fixed("leak", "Bearer tok-123456"))
                .is_ok(),
            "the native tool is registered"
        );
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        let results = tool_results(&runner);
        assert!(
            results.first().is_some_and(|text| text.starts_with("Bearer ")),
            "the masked result is in the history: {results:?}"
        );
        let state_text = runner.state().to_json().expect("json");
        assert!(!state_text.contains("tok-123456"), "no secret in the history");
        let requests = provider.requests();
        let second = requests.get(1).expect("second request");
        assert!(
            !format!("{:?}", second.messages()).contains("tok-123456"),
            "no secret in what the model receives"
        );
        assert!(
            !format!("{:?}", kinds(&parts)).contains("tok-123456"),
            "no secret in the events"
        );
    }

    // Scenario: l'enregistrement d'un nom réservé ou en double est refusé
    #[tokio::test]
    async fn refused_registrations_keep_the_first_valid_tool() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "echo", json!({})),
            say("done"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-native-refused"));
        let reserved = runner.with_native_tool(Native::fixed(RHAI_PAD_TOOL, "x")).err();
        assert!(
            matches!(&reserved, Some(SessionError::InvalidNativeTool { name }) if name == RHAI_PAD_TOOL),
            "the reserved name is refused: {reserved:?}"
        );
        assert!(
            runner.with_native_tool(Native::fixed("echo", "first")).is_ok(),
            "the first tool is registered"
        );
        let duplicate = runner.with_native_tool(Native::fixed("echo", "second")).err();
        assert!(
            matches!(&duplicate, Some(SessionError::InvalidNativeTool { name }) if name == "echo"),
            "the duplicate name is refused: {duplicate:?}"
        );
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        assert_eq!(
            tool_results(&runner),
            vec!["first".to_owned()],
            "the first valid tool is still callable"
        );
    }

    // Must (turn.sdd) : le texte d'un tool natif est borné par le plafond de rendu de la Budget
    #[tokio::test]
    async fn native_output_is_capped_by_the_budget_render_cap() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "long", json!({})),
            say("done"),
        ]));
        let options = Options {
            budget: Budget::new(100_000, 100, 1000, 200),
            ..Options::named("t-native-cap")
        };
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let long = "abcdefghij".repeat(60);
        assert!(
            runner.with_native_tool(Native::fixed("long", &long)).is_ok(),
            "the native tool is registered"
        );
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        let results = tool_results(&runner);
        let result = results.first().expect("the result is in the history");
        assert!(result.chars().count() <= 200, "the result is capped: {result}");
        assert!(
            result.starts_with("abcdefghij"),
            "the start of the output is kept: {result}"
        );
        assert!(
            result.contains("output truncated"),
            "a notice marks the cut: {result}"
        );
    }

    // Scenario: dépassement de délai signalé sans interruption
    #[tokio::test]
    async fn overrun_is_signalled_once_without_interruption() {
        let options = Options::named("t-overrun");
        let clock = Arc::clone(&options.clock);
        let options = Options {
            overrun: Some(Duration::from_secs(1)),
            engine: engine_with(move |engine| {
                let _tick = engine.register_fn("tick", move || clock.advance(Duration::from_secs(5)));
            }),
            ..options
        };
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "tick(); 1"),
            run_script("c2", "tick(); 2"),
            say("finished"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn ends normally");
        assert_eq!(output.response(), "finished", "the turn was not interrupted");
        let overruns: Vec<u64> = kinds(&parts)
            .into_iter()
            .filter_map(|kind| overrun_elapsed(&kind))
            .collect();
        assert_eq!(
            overruns,
            [5000],
            "a single Overrun, at the iteration that follows the delay"
        );
    }

    // Scenario: dépassement de délai signalé sans interruption (aucun délai configuré)
    #[tokio::test]
    async fn no_overrun_without_a_configured_delay() {
        let options = Options::named("t-no-overrun");
        let clock = Arc::clone(&options.clock);
        let options = Options {
            engine: engine_with(move |engine| {
                let _tick = engine.register_fn("tick", move || clock.advance(Duration::from_secs(500)));
            }),
            ..options
        };
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "tick(); 1"),
            say("fin"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let _output = run(&mut runner, "go").await;
        assert!(
            !kinds(&parts)
                .iter()
                .any(|kind| matches!(kind, EventKind::Overrun { .. })),
            "no delay, no Overrun"
        );
    }

    // Scenario: nombre maximal d'itérations
    #[tokio::test]
    async fn maximum_number_of_iterations() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "1"),
            run_script("c2", "2"),
            run_script("c3", "3"),
            run_script("c4", "4"),
        ]));
        let options = Options {
            max_steps: Some(3),
            ..Options::named("t-max-steps")
        };
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Failed, "the limit fails the turn");
        assert_eq!(output.calls(), 3, "three model calls");
        assert_eq!(provider.requests().len(), 3, "no call beyond the limit");
        assert!(!output.response().is_empty(), "a fixed message tells why");
        assert!(
            runner.state().history().pending_tool_calls().is_empty(),
            "every tool call has its result"
        );
    }

    // Scenario: persistance en fin de tour
    #[tokio::test]
    async fn state_is_saved_at_the_end_of_the_turn() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "let x = 2; fn double(a) { a * 2 } double(x)"),
            say("four"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-persist"));
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        let loaded = parts
            .store
            .load(runner.state().id())
            .await
            .expect("load")
            .expect("the state was saved");
        assert_eq!(
            &loaded,
            runner.state(),
            "the saved state is the state of the runner"
        );
        assert_eq!(loaded.history().messages().len(), 4, "the history is saved");
        assert_eq!(
            loaded.variables().get("x"),
            Some(&TaggedValue::Int(2)),
            "the variable is saved"
        );
        assert!(
            loaded.functions().keys().any(|key| key.starts_with("double/")),
            "the function is saved, got {:?}",
            loaded.functions()
        );
    }

    // Scenario: persistance en fin de tour (signaux)
    #[tokio::test]
    async fn signals_carry_the_outcome_and_the_rhai_errors() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "throw \"oops\""),
            run_script("c2", "1 + 1"),
            say("recovered"),
            say("second turn"),
        ]));
        let options = Options {
            redactor: Redactor::new(),
            ..Options::named("t-signals")
        };
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let _first = run(&mut runner, "go").await;
        let _second = run(&mut runner, "again").await;
        let first = runner
            .state()
            .signals()
            .get(&TurnId::new(1))
            .expect("signals of turn 1");
        assert_eq!(first.outcome(), Some(Outcome::Valid), "the outcome of the turn");
        assert_eq!(first.rhai_errors(), 1, "one Rhai error");
        assert_eq!(first.agent(), "main-agent", "the agent of the session");
        assert_eq!(first.model_profile(), "profile-a", "the model profile");
        assert_eq!(
            first.toolsets().get("k8s").map(String::as_str),
            Some("1.0.0"),
            "the toolset versions"
        );
        let second = runner
            .state()
            .signals()
            .get(&TurnId::new(2))
            .expect("signals of turn 2");
        assert_eq!(second.rhai_errors(), 0, "no error in the second turn");
    }

    // Scenario: annulation (pendant l'inférence, avant la requête, par le handle racine)
    #[tokio::test]
    async fn cancel_during_inference_abandons_the_call() {
        let called = Arc::new(Notify::new());
        let hanging = Hanging {
            called: Arc::clone(&called),
        };
        let (mut runner, _parts) = assemble(hanging, Options::named("t-cancel-inference"));
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let (output, ()) = tokio::join!(runner.run_turn("hello", &cancel), async move {
            called.notified().await;
            trigger.cancel();
        });
        assert_eq!(
            output.expect("turn").outcome(),
            Outcome::Cancelled,
            "the call is abandoned"
        );
        assert_eq!(
            roles(&runner),
            [Message::user("hello")],
            "the history keeps the message only"
        );
    }

    // Scenario: annulation (par le handle racine, avant la requête)
    #[tokio::test]
    async fn root_cancel_before_the_request() {
        let provider = Arc::new(ScriptedProvider::new(vec![say("never")]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-root-cancel"));
        parts.root.cancel();
        let output = run(&mut runner, "hello").await;
        assert_eq!(
            output.outcome(),
            Outcome::Cancelled,
            "the root handle cancels the turn"
        );
        assert!(
            provider.requests().is_empty(),
            "no request after the cancellation"
        );
        assert_eq!(output.response(), "", "no response");
    }

    // Scenario: annulation (pendant l'inférence, par le handle racine)
    #[tokio::test]
    async fn root_cancel_during_inference() {
        let called = Arc::new(Notify::new());
        let hanging = Hanging {
            called: Arc::clone(&called),
        };
        let (mut runner, parts) = assemble(hanging, Options::named("t-root-inference"));
        let root = parts.root.clone();
        let turn_cancel = CancelHandle::new();
        let (output, ()) = tokio::join!(runner.run_turn("hello", &turn_cancel), async move {
            called.notified().await;
            root.cancel();
        });
        assert_eq!(
            output.expect("turn").outcome(),
            Outcome::Cancelled,
            "the root handle abandons the call"
        );
    }

    // Scenario: annulation (pendant un script, l'historique reste valide)
    #[tokio::test]
    async fn cancel_during_a_script_keeps_the_history_valid() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(InferenceResponse::new(
                Message::assistant(
                    "",
                    None,
                    vec![
                        ToolCall::new("c1", RHAI_PAD_TOOL, json!({"script": "loop { }"})),
                        ToolCall::new("c2", RHAI_PAD_TOOL, json!({"script": "1"})),
                    ],
                ),
                None,
            )),
            say("never"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-cancel-script"));
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let (output, ()) = tokio::join!(runner.run_turn("go", &cancel), async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        assert_eq!(
            output.expect("turn").outcome(),
            Outcome::Cancelled,
            "the script is cancelled"
        );
        assert!(
            runner.state().history().pending_tool_calls().is_empty(),
            "every tool call has a result, the second one included"
        );
        assert_eq!(provider.requests().len(), 1, "no request after the cancellation");
    }

    // Scenario: réponse sans outil (la requête porte le prompt système, les tools et le raisonnement rejoué)
    #[tokio::test]
    async fn request_carries_the_segment_context_and_the_replayed_reasoning() {
        let thinking = Ok(InferenceResponse::new(
            Message::assistant(
                "",
                Some(Reasoning::new("model", "let me compute")),
                vec![ToolCall::new("c1", RHAI_PAD_TOOL, json!({"script": "1"}))],
            ),
            None,
        ));
        let provider = Arc::new(ScriptedProvider::new(vec![thinking, say("one")]));
        let options = Options {
            replay: ReplayMode::AllTurns,
            ..Options::named("t-request")
        };
        let (mut runner, _parts) = assemble(Arc::clone(&provider), options);
        let _output = run(&mut runner, "go").await;
        let requests = provider.requests();
        let first = requests.first().expect("first request");
        assert_eq!(
            first.system_prompt(),
            "system prompt",
            "the system prompt of the segment"
        );
        assert_eq!(
            first.tools().len(),
            1,
            "the native tool definitions of the segment"
        );
        assert_eq!(first.profile().name(), "profile-a", "the resolved profile");
        let second = requests.get(1).expect("second request");
        assert_eq!(
            second.reasoning(),
            [None, Some(Reasoning::new("model", "let me compute")), None],
            "the reasoning is replayed according to the profile mode"
        );
    }

    // Scenario: réponse sans outil (une réponse qui n'est pas de l'assistant)
    #[tokio::test]
    async fn a_response_that_is_not_an_assistant_message_fails_the_turn() {
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(InferenceResponse::new(
            Message::user("not me"),
            None,
        ))]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-not-assistant"));
        let output = run(&mut runner, "go").await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "an invalid response fails the turn"
        );
        assert_eq!(
            roles(&runner),
            [Message::user("go")],
            "the invalid response is not kept"
        );
    }

    // Test de non-régression (../session/history.sdd #4) : la branche `Discarded` des match de caractères compte zéro.
    #[test]
    fn a_discarded_message_counts_zero_characters() {
        let discarded = Message::discarded(
            DiscardCause::Truncated,
            Some(Reasoning::new("model", "a thought of forty thousand characters")),
        );
        assert_eq!(
            message_chars(&discarded),
            0,
            "a discarded message adds no character to the fallback estimate"
        );
        assert_eq!(
            messages_chars(&[discarded, Message::user("x")]),
            1,
            "only the neighbour message must count in the estimate"
        );
    }

    // Test de non-régression (../session/history.sdd #4) : le décompte d'usage compte zéro pour `Discarded`, raisonnement et complétion.
    #[tokio::test]
    async fn a_discarded_response_counts_zero_in_the_usage() {
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(InferenceResponse::new(
            Message::discarded(
                DiscardCause::Empty,
                Some(Reasoning::new("model", "a thought never sent to the model")),
            ),
            None,
        ))]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-discarded-zero"));
        let output = run(&mut runner, "go").await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "a discarded message is not an assistant answer: as for any non-assistant response, the turn fails"
        );
        assert_eq!(
            roles(&runner),
            [Message::user("go")],
            "the discarded response must not be kept in the history"
        );
        assert_eq!(
            runner.state().tokens().reasoning(),
            0,
            "the reasoning carried by a discarded message must count zero in the usage"
        );
        assert_eq!(
            runner.state().tokens().completion(),
            0,
            "a discarded message must feed the completion estimate with zero characters when the usage is not reported"
        );
    }

    const CUT_MESSAGE: &str = "the response was cut by the output limit before any text or tool call";

    // Scenario: une réponse vide tronquée par la limite de sortie échoue explicitement
    #[tokio::test]
    async fn an_empty_response_cut_by_the_output_limit_fails_explicitly() {
        let cut = InferenceResponse::new(
            Message::assistant("", Some(Reasoning::new("model", "thinking")), Vec::new()),
            Some(ReportedUsage::new(10, 20).with_reasoning(12)),
        )
        .with_finish_reason(FinishReason::Length);
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(cut)]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-cut-empty"));
        let output = run(&mut runner, "go").await;
        assert_eq!(
            output.outcome(),
            Outcome::Failed,
            "an empty cut response fails the turn"
        );
        assert_eq!(
            output.response(),
            CUT_MESSAGE,
            "the fixed message is the response"
        );
        assert_eq!(output.calls(), 1, "no further call to the model");
        assert_eq!(
            roles(&runner),
            [Message::user("go")],
            "no empty assistant message and no error text in the history"
        );
        let events = kinds(&parts);
        assert!(
            events.contains(&EventKind::ResponseTruncated {
                has_text: false,
                has_tool_calls: false
            }),
            "ResponseTruncated is emitted, got {events:?}"
        );
        assert!(
            matches!(
                events.last(),
                Some(EventKind::TurnEnded {
                    outcome: Outcome::Failed,
                    ..
                })
            ),
            "the turn ends with TurnEnded"
        );
        assert_eq!(
            runner.state().tokens().reasoning(),
            12,
            "the reported reasoning tokens are counted"
        );
    }

    // Scenario: une réponse tronquée qui porte du texte suit son cours
    #[tokio::test]
    async fn a_truncated_response_that_holds_text_goes_on() {
        let cut = InferenceResponse::new(Message::assistant("Bonjour", None, Vec::new()), None)
            .with_finish_reason(FinishReason::Length);
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(cut)]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-cut-text"));
        let output = run(&mut runner, "go").await;
        assert_eq!(
            output.outcome(),
            Outcome::Valid,
            "a cut response with text is valid"
        );
        assert_eq!(output.response(), "Bonjour", "the text is the response");
        assert!(
            kinds(&parts).contains(&EventKind::ResponseTruncated {
                has_text: true,
                has_tool_calls: false
            }),
            "ResponseTruncated carries has_text"
        );
        assert_eq!(
            roles(&runner),
            [
                Message::user("go"),
                Message::assistant("Bonjour", None, Vec::new())
            ],
            "the cut answer is kept"
        );
    }

    #[tokio::test]
    async fn a_truncated_response_that_holds_tool_calls_goes_on() {
        let cut = InferenceResponse::new(
            Message::assistant(
                "",
                None,
                vec![ToolCall::new("c1", RHAI_PAD_TOOL, json!({"script": "1 + 1"}))],
            ),
            None,
        )
        .with_finish_reason(FinishReason::Length);
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(cut), say("done")]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-cut-calls"));
        let output = run(&mut runner, "go").await;
        assert_eq!(
            output.outcome(),
            Outcome::Valid,
            "a cut response with a call follows its course"
        );
        assert_eq!(
            output.calls(),
            2,
            "the loop asks the model again after the tool result"
        );
        assert!(
            kinds(&parts).contains(&EventKind::ResponseTruncated {
                has_text: false,
                has_tool_calls: true
            }),
            "ResponseTruncated carries has_tool_calls"
        );
    }

    #[tokio::test]
    async fn a_response_that_is_not_cut_emits_no_truncation() {
        let done = InferenceResponse::new(Message::assistant("ok", None, Vec::new()), None)
            .with_finish_reason(FinishReason::Stop);
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(done)]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-stop"));
        drop(run(&mut runner, "go").await);
        assert!(
            !kinds(&parts)
                .iter()
                .any(|kind| matches!(kind, EventKind::ResponseTruncated { .. })),
            "a stopped response is not truncated"
        );
    }

    #[tokio::test]
    async fn reported_reasoning_tokens_replace_the_estimate() {
        let reply = InferenceResponse::new(
            Message::assistant(
                "ok",
                Some(Reasoning::new("model", "a long thought indeed")),
                Vec::new(),
            ),
            Some(ReportedUsage::new(10, 20).with_reasoning(7)),
        );
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(reply)]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-reasoning"));
        drop(run(&mut runner, "go").await);
        assert_eq!(runner.state().tokens().reasoning(), 7, "the reported count wins");
    }

    // Scenario: les jetons de raisonnement rapportés s'ajoutent au compteur (repli sur l'estimation)
    #[tokio::test]
    async fn missing_reported_reasoning_falls_back_on_the_estimate() {
        let thought = "a long thought indeed ".repeat(40);
        let reply = |usage: Option<ReportedUsage>| {
            InferenceResponse::new(
                Message::assistant("ok", Some(Reasoning::new("model", thought.clone())), Vec::new()),
                usage,
            )
        };
        let mut counted = Vec::new();
        for (session, usage) in [
            ("t-fallback-none", None),
            ("t-fallback-usage", Some(ReportedUsage::new(10, 20))),
        ] {
            let provider = Arc::new(ScriptedProvider::new(vec![Ok(reply(usage))]));
            let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named(session));
            drop(run(&mut runner, "go").await);
            counted.push(runner.state().tokens().reasoning());
        }
        assert!(
            counted.first().is_some_and(|tokens| *tokens > 0),
            "the estimate of a long reasoning is not zero, got {counted:?}"
        );
        assert_eq!(
            counted.first(),
            counted.get(1),
            "a usage without reasoning tokens falls back on the estimate"
        );
    }

    #[tokio::test]
    async fn run_turn_future_is_send() {
        fn assert_send<T: Send>(_value: &T) {}
        let provider = Arc::new(ScriptedProvider::new(vec![say("ok")]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-send"));
        let cancel = CancelHandle::new();
        let future = runner.run_turn("go", &cancel);
        assert_send(&future);
        assert!(future.await.is_ok(), "the turn runs");
    }

    // --- masking of the streamed fragments -----------------------------------------------------------------------

    fn stream(redactor: &Redactor, fragments: &[&str]) -> Vec<String> {
        let mut masker = Masker::new(redactor);
        let mut out: Vec<String> = fragments
            .iter()
            .filter_map(|fragment| masker.push(fragment))
            .collect();
        out.extend(masker.finish());
        out
    }

    #[test]
    fn masker_without_secret_passes_fragments_through() {
        let redactor = Redactor::new();
        assert_eq!(
            stream(&redactor, &["ab", "cd"]),
            ["ab", "cd"],
            "nothing is held back"
        );
    }

    #[test]
    fn masker_masks_a_secret_cut_between_fragments() {
        let mut redactor = Redactor::new();
        redactor.add_secret("abcdef").expect("secret");
        for cut in 1..6 {
            let text = "xx abcdef yy";
            let out = stream(
                &redactor,
                &[
                    text.get(..3 + cut).unwrap_or_default(),
                    text.get(3 + cut..).unwrap_or_default(),
                ],
            );
            assert_eq!(
                out.concat(),
                "xx *redacted secret* yy",
                "the secret cut at {cut} is masked"
            );
            assert!(
                out.iter()
                    .all(|piece| !piece.contains('a') || piece.contains("*redacted secret*")),
                "no piece shows a part of the secret cut at {cut}, got {out:?}"
            );
        }
    }

    #[test]
    fn masker_flushes_a_partial_prefix_at_the_end() {
        let mut redactor = Redactor::new();
        redactor.add_secret("abcdef").expect("secret");
        let out = stream(&redactor, &["hello abc"]);
        assert_eq!(
            out.concat(),
            "hello abc",
            "an incomplete prefix is emitted once the response ends"
        );
        assert_eq!(
            out.first().map(String::as_str),
            Some("hell"),
            "but only after holding back the last five characters"
        );
    }

    #[test]
    fn masker_handles_multibyte_secrets() {
        let mut redactor = Redactor::new();
        redactor.add_secret("\u{e9}".repeat(6)).expect("secret");
        let out = stream(&redactor, &["a \u{e9}\u{e9}", "\u{e9}\u{e9}\u{e9}\u{e9} b"]);
        assert_eq!(
            out.concat(),
            "a *redacted secret* b",
            "a multi-byte secret cut between fragments is masked"
        );
    }

    #[test]
    fn masker_masks_a_secret_inside_one_fragment_that_ends_in_the_hold_zone() {
        let mut redactor = Redactor::new();
        redactor.add_secret("abcdef").expect("secret");
        assert_eq!(
            stream(&redactor, &["see abcdef"]).concat(),
            "see *redacted secret*",
            "a complete secret at the end"
        );
        assert_eq!(
            stream(&redactor, &["abcdefabcdef!"]).concat(),
            "*redacted secret**redacted secret*!",
            "two secrets in a row"
        );
    }

    #[test]
    fn inbox_keeps_the_order_and_is_shared_by_clones() {
        let inbox = UserInbox::default();
        let clone = inbox.clone();
        inbox.send("one");
        clone.send(String::from("two"));
        assert_eq!(
            inbox.drain(),
            ["one", "two"],
            "messages come out in the order they were sent"
        );
        assert!(
            clone.drain().is_empty(),
            "draining empties the queue for every clone"
        );
    }

    #[test]
    fn config_debug_hides_the_collaborators() {
        let config = TurnConfig::new(
            Arc::new(ManualClock::new(Timestamp::from_millis(0))),
            Redactor::new(),
            Arc::new(RecordingSink::new()),
            CancelHandle::new(),
            Budget::new(1000, 10, 10, 10),
        )
        .with_agent("a")
        .with_max_steps(5)
        .with_overrun(Duration::from_secs(2));
        let text = format!("{config:?}");
        assert!(text.contains("max_steps: 5"), "the limits are shown, got {text}");
    }

    // --- session edits (title and flag for analysis) -------------------------------------------------------------

    /// Options whose engine has the functions `ask_title(text)` and `ask_flag(reason)` over `edits`, standing for the
    /// `session` module that the domain `modules` provides.
    fn options_with_edits(session: &'static str, edits: &Arc<SessionEdits>) -> Options {
        let (for_title, for_flag) = (Arc::clone(edits), Arc::clone(edits));
        let mut options = Options::named(session);
        options.engine = engine_with(move |engine| {
            let _title = engine.register_fn("ask_title", move |text: &str| for_title.set_title(text));
            let _flag = engine.register_fn("ask_flag", move |reason: &str| for_flag.flag_for_review(reason));
        });
        options
    }

    fn title_events(parts: &Parts) -> Vec<String> {
        kinds(parts)
            .into_iter()
            .filter_map(|kind| {
                if let EventKind::TitleChanged { title } = kind {
                    Some(title)
                } else {
                    None
                }
            })
            .collect()
    }

    fn review_events(parts: &Parts) -> Vec<String> {
        kinds(parts)
            .into_iter()
            .filter_map(|kind| {
                if let EventKind::ReviewRequested { reason } = kind {
                    Some(reason)
                } else {
                    None
                }
            })
            .collect()
    }

    async fn saved_state(parts: &Parts, id: &str) -> SessionState {
        parts
            .store
            .load(&SessionId::new(id).expect("a session identifier"))
            .await
            .expect("the store loads")
            .expect("the turn saved the state")
    }

    // Scenario: un titre posé par un script apparaît dans l'état et l'événement
    #[tokio::test]
    async fn a_title_set_by_a_script_reaches_the_state_and_the_event() {
        let edits = Arc::new(SessionEdits::new());
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "ask_title(\"  Migration\\nloki \")"),
            run_script("c2", "1 + 1"),
            say("done"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), options_with_edits("t-title", &edits));
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let output = run(&mut runner, "go").await;
        assert_eq!(output.outcome(), Outcome::Valid, "the turn is valid");
        assert_eq!(
            runner.state().title(),
            Some("Migration loki"),
            "the title is normalized and set in the state"
        );
        assert_eq!(
            title_events(&parts),
            vec!["Migration loki".to_owned()],
            "one TitleChanged event carries the normalized title"
        );
        assert!(edits.take().is_empty(), "the cell was emptied by the loop");
        assert_eq!(
            saved_state(&parts, "t-title").await.title(),
            Some("Migration loki"),
            "the end-of-turn save holds the title"
        );
    }

    // Scenario: un signalement ajoute l'étiquette
    #[tokio::test]
    async fn a_flag_adds_a_review_label() {
        let edits = Arc::new(SessionEdits::new());
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "ask_flag(\"wrong answer on loki\"); ask_flag(\"\")"),
            say("done"),
        ]));
        let mut options = options_with_edits("t-flag", &edits);
        options.clock = Arc::new(ManualClock::new(Timestamp::from_millis(1234)));
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let _output = run(&mut runner, "go").await;
        let labels: Vec<_> = runner
            .state()
            .labels()
            .iter()
            .map(|label| (label.text().to_owned(), label.author(), label.date()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (
                    "review-requested: wrong answer on loki".to_owned(),
                    LabelAuthor::Automatic,
                    Timestamp::from_millis(1234)
                ),
                (
                    "review-requested".to_owned(),
                    LabelAuthor::Automatic,
                    Timestamp::from_millis(1234)
                ),
            ],
            "each flag adds an automatic, dated label on the session"
        );
        assert_eq!(
            review_state(runner.state().labels()),
            Some(ReviewState::Requested(String::new())),
            "the last flag wins in the review state"
        );
        assert_eq!(
            review_events(&parts),
            vec!["wrong answer on loki".to_owned(), String::new()],
            "one ReviewRequested event per flag"
        );
        assert_eq!(
            saved_state(&parts, "t-flag").await.labels().iter().count(),
            2,
            "the end-of-turn save holds the labels"
        );
    }

    #[tokio::test]
    async fn a_long_reason_is_bounded_to_200_characters() {
        let edits = Arc::new(SessionEdits::new());
        let script = format!("ask_flag(\"{}\")", "x".repeat(300));
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", &script),
            say("done"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), options_with_edits("t-long", &edits));
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let _output = run(&mut runner, "go").await;
        let reasons = review_events(&parts);
        assert_eq!(reasons.len(), 1, "one flag, one event");
        assert!(
            reasons
                .iter()
                .all(|reason| reason.chars().count() == 200 && reason.ends_with('\u{2026}')),
            "the reason is bounded to 200 characters, ellipsis included: {reasons:?}"
        );
        let texts: Vec<_> = runner
            .state()
            .labels()
            .iter()
            .map(|label| label.text().chars().count())
            .collect();
        assert_eq!(
            texts,
            vec!["review-requested: ".chars().count() + 200],
            "the label carries the bounded reason"
        );
    }

    // Scenario: un secret n'apparaît nulle part
    #[tokio::test]
    async fn a_secret_never_reaches_a_title_a_reason_or_an_event() {
        let edits = Arc::new(SessionEdits::new());
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script(
                "c1",
                "ask_title(\"deploy with s3cr3t-token\"); ask_flag(\"leak of s3cr3t-token\")",
            ),
            say("done"),
        ]));
        let mut options = options_with_edits("t-secret", &edits);
        options.redactor = {
            let mut redactor = Redactor::new();
            redactor.add_secret("s3cr3t-token").expect("a long enough secret");
            redactor
        };
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let _output = run(&mut runner, "go").await;
        assert_eq!(
            runner.state().title(),
            Some("deploy with *redacted secret*"),
            "the secret is masked in the title"
        );
        let everything = format!(
            "{:?}{}{}",
            parts.sink.events(),
            runner.state().to_json().expect("the state serializes"),
            saved_state(&parts, "t-secret")
                .await
                .to_json()
                .expect("the saved state serializes")
        );
        assert!(
            !everything.contains("s3cr3t-token"),
            "the secret appears in no event, state or label: {everything}"
        );
        assert!(
            everything.contains("review-requested: leak of *redacted secret*"),
            "the label keeps the masked reason: {everything}"
        );
    }

    // Regression guard for the absence of a cell only: it cannot detect a regression of the application of the edits, the
    // other tests of this section do.
    #[tokio::test]
    async fn a_runner_without_a_cell_ignores_the_edits() {
        let edits = Arc::new(SessionEdits::new());
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", "ask_title(\"unwired\"); ask_flag(\"unwired\")"),
            say("done"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), options_with_edits("t-unwired", &edits));
        let _output = run(&mut runner, "go").await;
        assert_eq!(
            runner.state().title(),
            None,
            "a runner without a cell sets no title"
        );
        assert!(
            runner.state().labels().iter().next().is_none(),
            "and adds no label"
        );
        assert!(
            title_events(&parts).is_empty() && review_events(&parts).is_empty(),
            "and emits nothing"
        );
    }

    /// A native tool that fills the cell of session edits, then answers or waits for the cancellation of the turn.
    struct Editing {
        name: &'static str,
        edits: Arc<SessionEdits>,
        wait_for_cancel: bool,
    }

    impl Editing {
        /// The answer, after the cancellation of the turn when the tool was built to wait for it.
        async fn answer(&self, cancel: &CancelHandle) -> Result<String, String> {
            if self.wait_for_cancel {
                cancel.cancelled().await;
            }
            Ok("ok".to_owned())
        }
    }

    impl NativeTool for Editing {
        fn name(&self) -> &str {
            self.name
        }

        fn definition(&self) -> Value {
            json!({"name": "editing", "description": "a test tool", "parameters": {"type": "object"}})
        }

        fn call<'call>(
            &'call self,
            _arguments: &'call Value,
            cancel: &'call CancelHandle,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'call>> {
            self.edits.set_title("Title from a native tool");
            self.edits.flag_for_review("flag from a native tool");
            Box::pin(self.answer(cancel))
        }
    }

    // Mutant: the edits applied only after a `rhai_pad` call.
    #[tokio::test]
    async fn a_native_tool_can_fill_the_cell() {
        let edits = Arc::new(SessionEdits::new());
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "editing", json!({})),
            say("done"),
        ]));
        let (mut runner, parts) = assemble(Arc::clone(&provider), Options::named("t-native-edits"));
        assert!(
            runner
                .with_native_tool(Arc::new(Editing {
                    name: "editing",
                    edits: Arc::clone(&edits),
                    wait_for_cancel: false,
                }))
                .is_ok(),
            "the native tool is registered"
        );
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let _output = run(&mut runner, "go").await;
        assert_eq!(
            runner.state().title(),
            Some("Title from a native tool"),
            "the title is applied"
        );
        assert_eq!(
            title_events(&parts),
            vec!["Title from a native tool".to_owned()],
            "and announced"
        );
        assert_eq!(
            review_events(&parts),
            vec!["flag from a native tool".to_owned()],
            "the flag is applied too"
        );
    }

    // Mutant: `if !output.cancelled { apply }`. A call cancelled during the tool leaves the cell empty for the next turn.
    #[tokio::test]
    async fn a_cancelled_call_leaves_the_cell_empty() {
        let edits = Arc::new(SessionEdits::new());
        let provider = Arc::new(ScriptedProvider::new(vec![
            call_tool("c1", "editing", json!({})),
            say("never"),
        ]));
        let (mut runner, _parts) = assemble(Arc::clone(&provider), Options::named("t-cancel-edits"));
        assert!(
            runner
                .with_native_tool(Arc::new(Editing {
                    name: "editing",
                    edits: Arc::clone(&edits),
                    wait_for_cancel: true,
                }))
                .is_ok(),
            "the native tool is registered"
        );
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let (output, ()) = tokio::join!(runner.run_turn("go", &cancel), async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });
        assert_eq!(
            output.expect("a cancelled turn is not an error").outcome(),
            Outcome::Cancelled,
            "the turn is cancelled"
        );
        assert!(
            edits.take().is_empty(),
            "nothing is left in the cell for the next turn"
        );
    }

    // Mutants: the first or the second masking removed in the loop. Title and reason straddle their bounds, through the
    // events, the state and the saved state.
    #[tokio::test]
    async fn a_secret_straddling_a_bound_reaches_no_event_nor_state() {
        let secret = "s3cr3t-token";
        let edits = Arc::new(SessionEdits::new());
        let script = format!(
            "ask_title(\"{}{secret} tail\"); ask_flag(\"{}{secret} tail\"); ask_flag(\"ab\\ncd ef\")",
            "t".repeat(110),
            "r".repeat(192)
        );
        let provider = Arc::new(ScriptedProvider::new(vec![
            run_script("c1", &script),
            say("done"),
        ]));
        let mut options = options_with_edits("t-bound-secret", &edits);
        options.redactor = {
            let mut redactor = Redactor::new();
            redactor.add_secret(secret).expect("a long enough secret");
            redactor.add_secret("ab cd ef").expect("a long enough secret");
            redactor
        };
        let (mut runner, parts) = assemble(Arc::clone(&provider), options);
        let _wired = runner.with_session_edits(Arc::clone(&edits));
        let _output = run(&mut runner, "go").await;
        let everything = format!(
            "{:?}{}{}",
            parts.sink.events(),
            runner.state().to_json().expect("the state serializes"),
            saved_state(&parts, "t-bound-secret")
                .await
                .to_json()
                .expect("the saved state serializes")
        );
        let chars: Vec<char> = secret.chars().collect();
        for window in chars.windows(6) {
            let piece: String = window.iter().collect();
            assert!(
                !everything.contains(&piece),
                "the piece `{piece}` leaks: {everything}"
            );
        }
        assert!(
            !everything.contains("ab cd ef"),
            "the rebuilt secret leaks: {everything}"
        );
        assert!(runner.state().title().is_some(), "the title was set");
    }
}
