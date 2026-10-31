//! Persisted state of a session and its JSON form. The state holds the history, the segments, the scratchpad variables and
//! functions, the todo, the watchers, the run handles, the token counters, the agent reference, the toolset context and
//! versions, the labels, and the quality signals of each turn (`agent-sessions/session#FR-2`, `agent-sessions/session#AC-1`,
//! `agent-sessions/session#AC-5`). It holds names and references only, never a secret (`agent-sessions#CON-3`). Reading and
//! writing work on text: the storage belongs to `SessionStore`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    FormatVersion, History, Labels, Message, RunSignals, Segment, SessionError, SessionId, SessionResult,
    TaggedValue, TurnId,
};
use crate::clock::Timestamp;

/// Reference to the agent definition a session was built with: its name and the fingerprint of the definition. The
/// fingerprint is an opaque text, supplied by the host or by the `agents` domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AgentRef {
    name: String,
    fingerprint: String,
}

impl AgentRef {
    /// Builds a reference from the agent name and the fingerprint of its definition.
    #[must_use]
    pub fn new<N: Into<String>, F: Into<String>>(name: N, fingerprint: F) -> Self {
        Self {
            name: name.into(),
            fingerprint: fingerprint.into(),
        }
    }

    /// Returns the agent name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the fingerprint of the agent definition.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Token counters of a session: prompt, completion and reasoning tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TokenCounters {
    prompt: u64,
    completion: u64,
    reasoning: u64,
}

impl TokenCounters {
    /// Builds counters from the prompt, completion and reasoning token counts.
    #[must_use]
    pub const fn new(prompt: u64, completion: u64, reasoning: u64) -> Self {
        Self {
            prompt,
            completion,
            reasoning,
        }
    }

    /// Returns the prompt token count.
    #[must_use]
    pub const fn prompt(self) -> u64 {
        self.prompt
    }

    /// Returns the completion token count.
    #[must_use]
    pub const fn completion(self) -> u64 {
        self.completion
    }

    /// Returns the reasoning token count.
    #[must_use]
    pub const fn reasoning(self) -> u64 {
        self.reasoning
    }
}

/// Nature of a session: work, configuration or sub-agent.
///
/// It is written in lowercase, and a document written before the field existed reads as `Work`
/// (`agent-sessions#BR-8`, `agent-sessions/session#FR-2`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// A work session: runs a work agent, which may delegate to sub-agents.
    #[default]
    Work,
    /// A configuration session: creates toolsets and inspects agent definitions and sessions.
    Config,
    /// A sub-agent session, bound to the session that delegated to it.
    Sub,
}

impl SessionKind {
    /// Tells whether the kind is `Work`. The field is omitted from the JSON when it is `Work`, so a state written before the
    /// field existed keeps the same text.
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if hands the field over by reference, and cannot take it by value"
    )]
    pub(crate) const fn is_work(kind: &Self) -> bool {
        matches!(kind, Self::Work)
    }
}

/// Target of the inspection of a configuration session: an agent definition, with the name of the toolset context it runs
/// with, or another session. Reserved to a session of the `Config` nature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "lowercase")]
pub enum InspectionTargetRef {
    /// The definition of an agent, read with a toolset context.
    Definition {
        /// Name of the inspected agent.
        agent: String,
        /// Name of the toolset context the agent is read with.
        context: String,
    },
    /// Another session, by its identifier.
    Session(SessionId),
}

/// Persisted state of one session. Built by [`SessionState::new`], read by [`SessionState::from_json`], rendered by
/// [`SessionState::to_json`].
#[derive(Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SessionState {
    id: SessionId,
    format: FormatVersion,
    #[serde(skip_serializing_if = "SessionKind::is_work")]
    kind: SessionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inspection_target: Option<InspectionTargetRef>,
    created_at: Timestamp,
    agent: AgentRef,
    toolset_context: String,
    toolset_versions: BTreeMap<String, String>,
    history: History,
    segments: Vec<Segment>,
    variables: BTreeMap<String, TaggedValue>,
    functions: BTreeMap<String, String>,
    todo: Vec<serde_json::Value>,
    watchers: Vec<serde_json::Value>,
    run_handles: Vec<serde_json::Value>,
    tokens: TokenCounters,
    labels: Labels,
    signals: BTreeMap<TurnId, RunSignals>,
}

/// Wire form read by [`SessionState::from_json`], after the format version has been checked. Optional fields take their
/// empty value when absent; unknown fields are ignored.
#[derive(Deserialize)]
struct StateRecord {
    id: SessionId,
    format: FormatVersion,
    #[serde(default)]
    kind: SessionKind,
    #[serde(default)]
    parent: Option<SessionId>,
    #[serde(default)]
    inspection_target: Option<InspectionTargetRef>,
    #[serde(default = "default_created_at")]
    created_at: Timestamp,
    agent: AgentRef,
    #[serde(default)]
    toolset_context: String,
    #[serde(default)]
    toolset_versions: BTreeMap<String, String>,
    #[serde(default)]
    history: History,
    #[serde(default)]
    segments: Vec<Segment>,
    #[serde(default)]
    variables: BTreeMap<String, TaggedValue>,
    #[serde(default)]
    functions: BTreeMap<String, String>,
    #[serde(default)]
    todo: Vec<serde_json::Value>,
    #[serde(default)]
    watchers: Vec<serde_json::Value>,
    #[serde(default)]
    run_handles: Vec<serde_json::Value>,
    #[serde(default)]
    tokens: TokenCounters,
    #[serde(default)]
    labels: Labels,
    #[serde(default)]
    signals: BTreeMap<TurnId, RunSignals>,
}

/// Format header read first by [`SessionState::from_json`]: the format version alone, every other field ignored.
#[derive(Deserialize)]
struct FormatHeader {
    format: FormatVersion,
}

impl StateRecord {
    /// Moves the read fields into a state.
    fn into_state(self) -> SessionState {
        SessionState {
            id: self.id,
            format: self.format,
            kind: self.kind,
            parent: self.parent,
            inspection_target: self.inspection_target,
            created_at: self.created_at,
            agent: self.agent,
            toolset_context: self.toolset_context,
            toolset_versions: self.toolset_versions,
            history: self.history,
            segments: self.segments,
            variables: self.variables,
            functions: self.functions,
            todo: self.todo,
            watchers: self.watchers,
            run_handles: self.run_handles,
            tokens: self.tokens,
            labels: self.labels,
            signals: self.signals,
        }
    }
}

/// Creation date given to a state whose document does not carry one: the Unix epoch.
const fn default_created_at() -> Timestamp {
    Timestamp::from_millis(0)
}

/// Converts a JSON reading error into a serialization error. Only the position is kept, never the offending text, so that
/// a secret in a malformed document never reaches an error message.
fn read_error(error: &serde_json::Error) -> SessionError {
    SessionError::Serialization(format!(
        "invalid session document at line {}, column {}",
        error.line(),
        error.column()
    ))
}

impl SessionState {
    /// Builds the state of a new session. The format version is the current one; the history, segments, variables,
    /// functions, todo, watchers, run handles, labels and signals are empty.
    #[must_use]
    pub fn new<C: Into<String>>(
        id: SessionId,
        agent: AgentRef,
        toolset_context: C,
        toolset_versions: BTreeMap<String, String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            format: FormatVersion::CURRENT,
            kind: SessionKind::Work,
            parent: None,
            inspection_target: None,
            created_at,
            agent,
            toolset_context: toolset_context.into(),
            toolset_versions,
            history: History::new(),
            segments: Vec::new(),
            variables: BTreeMap::new(),
            functions: BTreeMap::new(),
            todo: Vec::new(),
            watchers: Vec::new(),
            run_handles: Vec::new(),
            tokens: TokenCounters::default(),
            labels: Labels::new(),
            signals: BTreeMap::new(),
        }
    }

    /// Renders the state as JSON text. Tables are written in key order, so the output is reproducible.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Serialization`] when a value cannot be written, such as a non-finite float.
    pub fn to_json(&self) -> SessionResult<String> {
        serde_json::to_string(self).map_err(|error| {
            SessionError::Serialization(format!("the session state cannot be serialized: {error}"))
        })
    }

    /// Reads a state from JSON text. The format version is read and checked first: a different major version is refused
    /// without interpreting the rest of the document. Unknown fields are ignored and absent optional fields take their
    /// empty value.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::IncompatibleFormat`] when the major version differs from the current one, and
    /// [`SessionError::Serialization`] when the text is not a valid state, or when its nature cannot carry what it holds:
    /// a parent outside the `Sub` nature, or an inspection target outside the `Config` nature. The error names neither
    /// the session nor the parent or target.
    pub fn from_json(text: &str) -> SessionResult<Self> {
        let header: FormatHeader = serde_json::from_str(text).map_err(|error| read_error(&error))?;
        header.format.check()?;
        let record: StateRecord = serde_json::from_str(text).map_err(|error| read_error(&error))?;
        let mut state = record.into_state();
        state.check_placement()?;
        // The read content is interpreted under the current format: unknown fields are dropped, so the label must not
        // keep the older minor version that was read.
        state.format = FormatVersion::CURRENT;
        Ok(state)
    }

    /// Refuses a state whose nature does not carry what it holds. The error gives the rule, never the content.
    fn check_placement(&self) -> SessionResult<()> {
        if self.parent.is_some() && self.kind != SessionKind::Sub {
            return Err(SessionError::Serialization(
                "a parent session is only carried by a sub-agent session".to_owned(),
            ));
        }
        if self.inspection_target.is_some() && self.kind != SessionKind::Config {
            return Err(SessionError::Serialization(
                "an inspection target is only carried by a configuration session".to_owned(),
            ));
        }
        Ok(())
    }

    /// Returns the session identifier.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// Returns the format version of the state.
    #[must_use]
    pub const fn format(&self) -> FormatVersion {
        self.format
    }

    /// Returns the creation date of the session.
    #[must_use]
    pub const fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// Returns the reference to the agent definition.
    #[must_use]
    pub const fn agent(&self) -> &AgentRef {
        &self.agent
    }

    /// Returns the name of the toolset context.
    #[must_use]
    pub const fn toolset_context(&self) -> &str {
        self.toolset_context.as_str()
    }

    /// Returns the toolset versions, by name.
    #[must_use]
    pub const fn toolset_versions(&self) -> &BTreeMap<String, String> {
        &self.toolset_versions
    }

    /// Returns the history.
    #[must_use]
    pub const fn history(&self) -> &History {
        &self.history
    }

    /// Returns the segments, in the order they started.
    #[must_use]
    pub const fn segments(&self) -> &[Segment] {
        self.segments.as_slice()
    }

    /// Returns the scratchpad variables, by name.
    #[must_use]
    pub const fn variables(&self) -> &BTreeMap<String, TaggedValue> {
        &self.variables
    }

    /// Returns the scratchpad functions, by name, as source text.
    #[must_use]
    pub const fn functions(&self) -> &BTreeMap<String, String> {
        &self.functions
    }

    /// Returns the todo entries, as opaque JSON values.
    #[must_use]
    pub const fn todo(&self) -> &[serde_json::Value] {
        self.todo.as_slice()
    }

    /// Returns the watchers, as opaque JSON values.
    #[must_use]
    pub const fn watchers(&self) -> &[serde_json::Value] {
        self.watchers.as_slice()
    }

    /// Returns the handles of the runs in progress, as opaque JSON values.
    #[must_use]
    pub const fn run_handles(&self) -> &[serde_json::Value] {
        self.run_handles.as_slice()
    }

    /// Returns the token counters.
    #[must_use]
    pub const fn tokens(&self) -> TokenCounters {
        self.tokens
    }

    /// Returns the labels of the session.
    #[must_use]
    pub const fn labels(&self) -> &Labels {
        &self.labels
    }

    /// Returns the quality signals of each turn, by turn.
    #[must_use]
    pub const fn signals(&self) -> &BTreeMap<TurnId, RunSignals> {
        &self.signals
    }

    /// Returns the nature of the session.
    #[must_use]
    pub const fn kind(&self) -> SessionKind {
        self.kind
    }

    /// Returns the identifier of the parent session, for a sub-agent session.
    #[must_use]
    pub const fn parent(&self) -> Option<&SessionId> {
        self.parent.as_ref()
    }

    /// Returns the inspection target, when the session has one.
    #[must_use]
    pub const fn inspection_target(&self) -> Option<&InspectionTargetRef> {
        self.inspection_target.as_ref()
    }

    /// Sets the nature of the session. What the new nature does not carry is dropped: the inspection target unless the
    /// nature is `Config`, and the parent session unless the nature is `Sub`.
    #[must_use]
    pub fn with_kind(mut self, kind: SessionKind) -> Self {
        self.kind = kind;
        if !matches!(kind, SessionKind::Config) {
            self.inspection_target = None;
        }
        if !matches!(kind, SessionKind::Sub) {
            self.parent = None;
        }
        self
    }

    /// Sets the parent session of a sub-agent session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotSubSession`], naming the session, when the session is not of the `Sub` nature. The state
    /// is taken by value, so a refused call gives no state back.
    pub fn with_parent(mut self, parent: SessionId) -> SessionResult<Self> {
        if self.kind != SessionKind::Sub {
            return Err(SessionError::NotSubSession(self.id));
        }
        self.parent = Some(parent);
        Ok(self)
    }

    /// Sets the inspection target, or clears it with `None`. A target can only be set on a session of the `Config` nature.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NotConfigSession`], naming the session, when a target is set on a session that is not of
    /// the `Config` nature. The state is then unchanged.
    pub fn set_inspection_target(&mut self, target: Option<InspectionTargetRef>) -> SessionResult<()> {
        if target.is_some() && self.kind != SessionKind::Config {
            return Err(SessionError::NotConfigSession(self.id.clone()));
        }
        self.inspection_target = target;
        Ok(())
    }

    /// Appends a message to the history. Nothing already in the history is changed.
    pub(crate) fn push_message(&mut self, message: Message) {
        self.history.push(message);
    }

    /// Appends a user message received during a turn, refused while a tool call waits for its result.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::PendingToolCall`] when some tool call has no result yet; the history is then unchanged.
    pub(crate) fn push_user_during_turn<C: Into<String>>(&mut self, content: C) -> SessionResult<()> {
        self.history.push_user_during_turn(content)
    }

    /// Appends a segment, which becomes the current one.
    pub(crate) fn push_segment(&mut self, segment: Segment) {
        self.segments.push(segment);
    }

    /// Replaces the scratchpad variables and the function sources with those of a pad snapshot.
    pub(crate) fn set_pad(
        &mut self,
        variables: BTreeMap<String, TaggedValue>,
        functions: BTreeMap<String, String>,
    ) {
        self.variables = variables;
        self.functions = functions;
    }

    /// Adds `usage` to the token counters, saturating at the maximum.
    pub(crate) const fn add_tokens(&mut self, usage: TokenCounters) {
        self.tokens = TokenCounters {
            prompt: self.tokens.prompt.saturating_add(usage.prompt),
            completion: self.tokens.completion.saturating_add(usage.completion),
            reasoning: self.tokens.reasoning.saturating_add(usage.reasoning),
        };
    }

    /// Records the quality signals of `turn`, replacing those of an earlier record of the same turn.
    pub(crate) fn record_signals(&mut self, turn: TurnId, signals: RunSignals) {
        let _replaced = self.signals.insert(turn, signals);
    }
}

/// The Debug of a state shows its identifier, its nature and the number of each of its elements. It never shows the
/// history, the variables, the function sources nor the other content, which may hold secrets.
impl std::fmt::Debug for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionState")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("messages", &self.history.messages().len())
            .field("segments", &self.segments.len())
            .field("variables", &self.variables.len())
            .field("functions", &self.functions.len())
            .field("todo", &self.todo.len())
            .field("watchers", &self.watchers.len())
            .field("run_handles", &self.run_handles.len())
            .field("labels", &self.labels.iter().count())
            .field("signals", &self.signals.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
impl SessionState {
    /// Test-only access to the variables, so that a store test can change the original object after saving it.
    pub(crate) const fn variables_mut(&mut self) -> &mut BTreeMap<String, TaggedValue> {
        &mut self.variables
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{AgentRef, InspectionTargetRef, SessionKind, SessionState, TokenCounters};
    use crate::clock::{ManualClock, Timestamp};
    use crate::session::{
        BoundaryKind, FormatVersion, LabelAuthor, LabelTarget, Message, ModelSnapshot, Outcome, RunSignals,
        Segment, SegmentContext, SessionError, SessionId, TaggedValue, ToolCall, TurnId,
    };

    fn session_id(text: &str) -> SessionId {
        SessionId::new(text).expect("a non-empty text makes a session identifier")
    }

    fn toolsets(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, version)| ((*name).to_owned(), (*version).to_owned()))
            .collect()
    }

    fn minimal() -> SessionState {
        SessionState::new(
            session_id("s-1"),
            AgentRef::new("main-agent", "sha-1"),
            "k8s-context",
            toolsets(&[("k8s", "1.4.0")]),
            Timestamp::from_millis(1000),
        )
    }

    /// A state holding every kind of field, built inside the module.
    fn populated() -> SessionState {
        let clock = ManualClock::new(Timestamp::from_millis(1500));
        let mut state = minimal();
        state.history.push(Message::user("list the pods"));
        state.history.push(Message::assistant("done", None, Vec::new()));
        state.segments.push(Segment::new(
            BoundaryKind::Start,
            SegmentContext::new("system prompt", Vec::new(), ModelSnapshot::new("model-x", "none")),
            Timestamp::from_millis(1000),
        ));
        state.variables.extend([
            ("count".to_owned(), TaggedValue::Int(7)),
            ("letter".to_owned(), TaggedValue::Char('x')),
        ]);
        state
            .functions
            .extend([("scale".to_owned(), "fn scale(n) { n }".to_owned())]);
        state.todo.push(json!({"text": "check the pods", "done": false}));
        state.tokens = TokenCounters::new(10, 20, 5);
        assert!(
            state
                .labels
                .add("good", LabelTarget::Session, LabelAuthor::Human, &clock)
                .is_some(),
            "a non-empty label must be added to the populated state"
        );
        let mut signals = RunSignals::new("main-agent", "profile-a", toolsets(&[("k8s", "1.4.0")]));
        signals.finish(Outcome::Valid);
        state.signals.extend([(TurnId::new(2), signals)]);
        state
    }

    fn minimal_document(extra: &serde_json::Value) -> String {
        let mut document = json!({
            "id": "s-1",
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "format": {"major": 1, "minor": 0},
        });
        if let (Some(target), Some(source)) = (document.as_object_mut(), extra.as_object()) {
            target.extend(source.clone());
        }
        document.to_string()
    }

    #[test]
    fn full_round_trip_keeps_every_field() {
        let original = populated();
        let text = original.to_json().expect("a populated state must serialize");
        let restored = SessionState::from_json(&text).expect("the text written by to_json must load");
        assert_eq!(
            restored, original,
            "every field of the state must equal the original after a serialization and a deserialization"
        );
    }

    #[test]
    fn variable_types_survive() {
        let mut state = minimal();
        state.variables.extend([
            ("c".to_owned(), TaggedValue::Char('x')),
            ("b".to_owned(), TaggedValue::Blob(vec![1, 2, 255])),
            ("n".to_owned(), TaggedValue::Int(7)),
        ]);
        let text = state.to_json().expect("the state must serialize");
        let restored = SessionState::from_json(&text).expect("the state must load");
        let variables = restored.variables();
        assert!(
            matches!(variables.get("c"), Some(TaggedValue::Char('x'))),
            "the variable c must come back as the char x, got {variables:?}"
        );
        assert!(
            matches!(variables.get("b"), Some(TaggedValue::Blob(bytes)) if bytes == &[1, 2, 255]),
            "the variable b must come back as the same blob"
        );
        assert!(
            matches!(variables.get("n"), Some(TaggedValue::Int(7))),
            "the variable n must come back as the integer 7"
        );
    }

    #[test]
    fn newer_minor_with_unknown_field_loads() {
        let text = json!({
            "id": "s-1",
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "format": {"major": 1, "minor": 9},
            "future_field": {"x": 1},
        })
        .to_string();
        let state = SessionState::from_json(&text).expect("a format 1.9 document must load");
        assert_eq!(
            state.format(),
            FormatVersion::CURRENT,
            "a state read from format 1.9 must hold the current format version"
        );
        let rendered = state.to_json().expect("the loaded state must serialize");
        assert!(
            !rendered.contains("future_field"),
            "the unknown field must be ignored, not carried into the state"
        );
    }

    // Scenario: l'état réécrit porte le format courant
    #[test]
    fn rewritten_state_holds_the_current_format() {
        let text = json!({
            "id": "s-1",
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "format": {"major": 1, "minor": 9},
            "future_field": {"x": 1},
        })
        .to_string();
        let state = SessionState::from_json(&text).expect("a format 1.9 document must load");
        let saved = state.to_json().expect("the loaded state must serialize");
        let document: serde_json::Value =
            serde_json::from_str(&saved).expect("the saved text must be valid JSON");
        assert_eq!(
            document["format"],
            json!({"major": 1, "minor": 0}),
            "the saved document must carry the current format version"
        );
        assert!(
            document.get("future_field").is_none(),
            "the saved document must not carry the unknown field"
        );
    }

    // Scenario: la date de création absente vaut l'époque Unix
    #[test]
    fn absent_creation_date_is_the_epoch() {
        let state = SessionState::from_json(&minimal_document(&json!({})))
            .expect("a minimal format 1.0 document must load");
        assert_eq!(
            state.created_at(),
            Timestamp::from_millis(0),
            "a document without creation date must read as the Unix epoch"
        );
    }

    #[test]
    fn different_major_refused_before_reading() {
        let text = json!({
            "id": "s-1",
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "format": {"major": 2, "minor": 0},
            "history": "not a list",
            "variables": 42,
        })
        .to_string();
        let result = SessionState::from_json(&text);
        let Err(SessionError::IncompatibleFormat { saved, current }) = result else {
            panic!(
                "a format 2.0 document must be refused as IncompatibleFormat, before its invalid body is read"
            );
        };
        assert_eq!(
            saved,
            FormatVersion::new(2, 0),
            "the refusal must name the saved version 2.0"
        );
        assert_eq!(
            current,
            FormatVersion::CURRENT,
            "the refusal must name the current version"
        );
    }

    #[test]
    fn no_secret_in_the_state() {
        let text = populated().to_json().expect("the populated state must serialize");
        assert!(
            text.contains("k8s-context"),
            "the state must hold the toolset context by its name"
        );
        for forbidden in [
            "credential",
            "secret",
            "password",
            "api_key",
            "endpoint",
            "bearer",
        ] {
            assert!(
                !text.contains(forbidden),
                "the state must not carry a credential field, but the text contains {forbidden}"
            );
        }
    }

    #[test]
    fn absent_optional_fields_default() {
        let state = SessionState::from_json(&minimal_document(&json!({})))
            .expect("a minimal format 1.0 document must load");
        assert!(state.history().messages().is_empty(), "the history must be empty");
        assert!(state.segments().is_empty(), "the segments must be empty");
        assert!(state.variables().is_empty(), "the variables must be empty");
        assert!(state.functions().is_empty(), "the functions must be empty");
        assert!(state.todo().is_empty(), "the todo must be empty");
        assert!(state.watchers().is_empty(), "the watchers must be empty");
        assert!(state.run_handles().is_empty(), "the run handles must be empty");
        assert_eq!(
            state.tokens(),
            TokenCounters::default(),
            "the token counters must be zero"
        );
        assert_eq!(state.labels().iter().count(), 0, "the labels must be empty");
        assert!(state.signals().is_empty(), "the signals must be empty");
        assert!(
            state.toolset_context().is_empty(),
            "the toolset context must be empty"
        );
        assert!(
            state.toolset_versions().is_empty(),
            "the toolset versions must be empty"
        );
    }

    #[test]
    fn turn_signals_keyed_by_turn_id_survive() {
        let mut state = populated();
        state.signals.extend([(
            TurnId::new(10),
            RunSignals::new("main-agent", "profile-a", BTreeMap::new()),
        )]);
        let text = state.to_json().expect("the state must serialize");
        assert!(
            text.contains(r#""signals":{"2":"#),
            "a turn identifier must be written as a JSON string key, got {text}"
        );
        let restored = SessionState::from_json(&text).expect("the state must load");
        assert!(
            restored.signals().contains_key(&TurnId::new(2))
                && restored.signals().contains_key(&TurnId::new(10)),
            "both turn keys must come back as turn identifiers"
        );
        assert_eq!(
            restored, state,
            "the turn signals must equal the original after a round trip"
        );
    }

    #[test]
    fn created_state_holds_the_current_format() {
        let state = minimal();
        assert_eq!(
            state.format(),
            FormatVersion::CURRENT,
            "a state created by new must hold the current format version"
        );
        assert_eq!(
            state.created_at(),
            Timestamp::from_millis(1000),
            "the creation date given to new must be kept"
        );
        assert_eq!(
            state.id().as_str(),
            "s-1",
            "the identifier given to new must be kept"
        );
    }

    #[test]
    fn empty_session_id_refused_on_read() {
        let text = json!({
            "id": "",
            "agent": {"name": "main-agent", "fingerprint": "sha-1"},
            "format": {"major": 1, "minor": 0},
        })
        .to_string();
        assert!(
            matches!(
                SessionState::from_json(&text),
                Err(SessionError::Serialization(_))
            ),
            "an empty session identifier must be refused when the state is read"
        );
        assert!(
            SessionState::from_json(&minimal_document(&json!({}))).is_ok(),
            "control: the same document with a non-empty identifier must be read, so the refusal above is about the identifier"
        );
    }

    #[test]
    fn malformed_text_refused() {
        assert!(
            matches!(
                SessionState::from_json("not json"),
                Err(SessionError::Serialization(_))
            ),
            "text that is not JSON must be refused as a serialization error"
        );
        assert!(
            SessionState::from_json(&minimal_document(&json!({}))).is_ok(),
            "control: a valid minimal document must be read, so the refusal above is about the text"
        );
    }

    #[test]
    fn unserializable_variable_refused() {
        let mut state = minimal();
        state
            .variables
            .extend([("nan".to_owned(), TaggedValue::Float(f64::NAN))]);
        assert!(
            matches!(state.to_json(), Err(SessionError::Serialization(_))),
            "a state holding a non-finite float must fail to serialize"
        );
        assert!(
            minimal().to_json().is_ok(),
            "control: the same state without the non-finite float must serialize, so the refusal above is about the float"
        );
    }

    #[test]
    fn tables_are_written_in_key_order() {
        let mut state = minimal();
        state.variables.extend([
            ("zeta".to_owned(), TaggedValue::Int(2)),
            ("alpha".to_owned(), TaggedValue::Int(1)),
        ]);
        let first = state.to_json().expect("the state must serialize");
        let second = state.to_json().expect("the state must serialize a second time");
        assert_eq!(
            first, second,
            "two serializations of the same state must be identical"
        );
        let alpha = first.find("alpha").expect("the variable alpha must be written");
        let zeta = first.find("zeta").expect("the variable zeta must be written");
        assert!(alpha < zeta, "the variables must be written in key order");
    }

    /// Writes the state and reads it back, through the text form.
    fn round_trip(state: &SessionState) -> SessionState {
        let text = state.to_json().expect("the state must serialize");
        SessionState::from_json(&text).expect("the text written by to_json must load")
    }

    // Scenario: la nature vaut Work par défaut
    #[test]
    fn kind_defaults_to_work() {
        let state = SessionState::from_json(&minimal_document(&json!({})))
            .expect("a document without a kind must load");
        assert_eq!(
            state.kind(),
            SessionKind::Work,
            "a document written without a nature field must read as a work session"
        );
    }

    // Scenario: un sous-agent garde son parent
    #[test]
    fn sub_agent_keeps_its_parent() {
        let state = minimal()
            .with_kind(SessionKind::Sub)
            .with_parent(SessionId::new("s-parent").expect("a non-empty identifier"))
            .expect("a sub-agent session accepts a parent session");
        let restored = round_trip(&state);
        assert_eq!(
            restored.kind(),
            SessionKind::Sub,
            "the sub-agent nature must survive a round trip"
        );
        assert_eq!(
            restored.parent().map(SessionId::as_str),
            Some("s-parent"),
            "the parent session must survive a round trip"
        );
        assert_eq!(
            restored, state,
            "the sub-agent state must equal the original after a round trip"
        );
    }

    // Scenario: la cible d'inspection fait l'aller-retour
    #[test]
    fn inspection_target_round_trips() {
        let mut state = minimal().with_kind(SessionKind::Config);
        let definition = InspectionTargetRef::Definition {
            agent: "main-agent".to_owned(),
            context: "k8s-context".to_owned(),
        };
        state
            .set_inspection_target(Some(definition.clone()))
            .expect("a configuration session accepts a definition target");
        let restored = round_trip(&state);
        assert_eq!(
            restored.inspection_target(),
            Some(&definition),
            "the definition target must come back equal to the original"
        );

        let session = InspectionTargetRef::Session(SessionId::new("s-2").expect("a non-empty identifier"));
        state
            .set_inspection_target(Some(session.clone()))
            .expect("a configuration session accepts a session target");
        let restored_session = round_trip(&state);
        assert_eq!(
            restored_session.inspection_target(),
            Some(&session),
            "the session target must come back equal to the original"
        );
        assert_eq!(
            restored_session, state,
            "the configuration state must equal the original after a round trip"
        );
    }

    // Scenario: cible d'inspection refusée hors Config
    #[test]
    fn inspection_target_refused_outside_config() {
        let mut state = minimal();
        let before = state.clone();
        let refused = state.set_inspection_target(Some(InspectionTargetRef::Session(
            SessionId::new("s-2").expect("a non-empty identifier"),
        )));
        assert!(
            matches!(&refused, Err(SessionError::NotConfigSession(id)) if id.as_str() == "s-1"),
            "a target on a work session must fail with NotConfigSession naming the session, got {refused:?}"
        );
        assert_eq!(
            state, before,
            "the refused operation must leave the state unchanged"
        );
    }

    // Control of the scenario above: the same target is accepted on a configuration session.
    #[test]
    fn inspection_target_accepted_on_config() {
        let mut state = minimal().with_kind(SessionKind::Config);
        assert!(
            state
                .set_inspection_target(Some(InspectionTargetRef::Session(
                    SessionId::new("s-2").expect("a non-empty identifier"),
                )))
                .is_ok(),
            "control: the same target must be accepted on a configuration session"
        );
    }

    // Scenario: parent refusé hors sous-agent
    #[test]
    fn parent_refused_outside_sub_agent() {
        let parent = SessionId::new("s-parent").expect("a non-empty identifier");
        let refused = minimal().with_parent(parent.clone());
        assert!(
            matches!(&refused, Err(SessionError::NotSubSession(id)) if id.as_str() == "s-1"),
            "a parent on a work session must fail with NotSubSession naming the session, got {refused:?}"
        );
        assert!(
            minimal().with_kind(SessionKind::Sub).with_parent(parent).is_ok(),
            "control: the same parent must be accepted on a sub-agent session"
        );
    }

    // Scenario: changer de nature retire la cible
    #[test]
    fn changing_kind_drops_the_target() {
        let mut state = minimal().with_kind(SessionKind::Config);
        state
            .set_inspection_target(Some(InspectionTargetRef::Session(
                SessionId::new("s-2").expect("a non-empty identifier"),
            )))
            .expect("a configuration session accepts a session target");
        let kept = state.clone().with_kind(SessionKind::Config);
        assert!(
            kept.inspection_target().is_some(),
            "control: keeping the configuration nature must keep the target"
        );
        let work = state.with_kind(SessionKind::Work);
        assert!(
            work.inspection_target().is_none(),
            "a session that leaves the configuration nature must have no target"
        );
    }

    // Must (#9): changing the kind drops the parent when the new nature is not sub-agent.
    #[test]
    fn changing_kind_drops_the_parent() {
        let sub = minimal()
            .with_kind(SessionKind::Sub)
            .with_parent(SessionId::new("s-parent").expect("a non-empty identifier"))
            .expect("a sub-agent session accepts a parent session");
        let kept = sub.clone().with_kind(SessionKind::Sub);
        assert!(
            kept.parent().is_some(),
            "control: keeping the sub-agent nature must keep the parent"
        );
        let work = sub.with_kind(SessionKind::Work);
        assert!(
            work.parent().is_none(),
            "a session that leaves the sub-agent nature must have no parent"
        );
    }

    // Must (#9): an inspection target is cleared with None.
    #[test]
    fn inspection_target_cleared_with_none() {
        let mut state = minimal().with_kind(SessionKind::Config);
        state
            .set_inspection_target(Some(InspectionTargetRef::Session(
                SessionId::new("s-2").expect("a non-empty identifier"),
            )))
            .expect("a configuration session accepts a session target");
        assert!(
            state.set_inspection_target(None).is_ok(),
            "clearing the target must be accepted on a configuration session"
        );
        assert!(
            state.inspection_target().is_none(),
            "the target must be gone after set_inspection_target(None)"
        );
    }

    #[test]
    fn kind_is_written_in_lowercase() {
        let config = minimal()
            .with_kind(SessionKind::Config)
            .to_json()
            .expect("the state must serialize");
        let sub = minimal()
            .with_kind(SessionKind::Sub)
            .to_json()
            .expect("the state must serialize");
        assert!(
            config.contains(r#""kind":"config""#),
            "the configuration nature must be written as config, got {config}"
        );
        assert!(
            sub.contains(r#""kind":"sub""#),
            "the sub-agent nature must be written as sub, got {sub}"
        );
    }

    #[test]
    fn work_state_without_parent_or_target_keeps_its_json() {
        let text = minimal().to_json().expect("the state must serialize");
        for key in ["\"kind\"", "\"parent\"", "\"inspection_target\""] {
            assert!(
                !text.contains(key),
                "an absent optional field must be omitted from the JSON, but {key} is written: {text}"
            );
        }
    }

    /// A document of the given nature carrying a parent session, with an identifier that must not leak into the error.
    fn document_with_parent(kind: &str) -> String {
        minimal_document(&json!({"kind": kind, "parent": "parent-session-leak"}))
    }

    /// A document of the given nature carrying an inspection target, with names that must not leak into the error.
    fn document_with_target(kind: &str) -> String {
        minimal_document(&json!({
            "kind": kind,
            "inspection_target": {"definition": {"agent": "agent-name-leak", "context": "context-name-leak"}},
        }))
    }

    // T9 (4): a document whose nature cannot carry its parent is refused, without the session content in the error.
    #[test]
    fn from_json_refuses_a_parent_outside_sub_agent() {
        for kind in ["work", "config"] {
            let result = SessionState::from_json(&document_with_parent(kind));
            let Err(SessionError::Serialization(message)) = &result else {
                panic!("a parent on a `{kind}` document must be refused as Serialization, got {result:?}");
            };
            assert!(
                !message.contains("parent-session-leak") && !message.contains("s-1"),
                "the refusal must not carry the session content, got {message}"
            );
        }
        assert!(
            SessionState::from_json(&document_with_parent("sub")).is_ok(),
            "control: the same parent is read on a sub-agent document"
        );
    }

    // T9 (4): a document whose nature cannot carry its inspection target is refused, without the names in the error.
    #[test]
    fn from_json_refuses_a_target_outside_config() {
        for kind in ["work", "sub"] {
            let result = SessionState::from_json(&document_with_target(kind));
            let Err(SessionError::Serialization(message)) = &result else {
                panic!("a target on a `{kind}` document must be refused as Serialization, got {result:?}");
            };
            assert!(
                !message.contains("agent-name-leak") && !message.contains("context-name-leak"),
                "the refusal must not carry the names of the target, got {message}"
            );
        }
        assert!(
            SessionState::from_json(&document_with_target("config")).is_ok(),
            "control: the same target is read on a configuration document"
        );
    }

    // T9 (4): clearing the target is accepted on every nature, and a state without a target is left as it was.
    #[test]
    fn inspection_target_cleared_with_none_on_any_nature() {
        for state in [
            minimal(),
            minimal().with_kind(SessionKind::Config),
            minimal()
                .with_kind(SessionKind::Sub)
                .with_parent(session_id("s-parent"))
                .expect("a sub-agent session accepts a parent session"),
        ] {
            let mut cleared = state.clone();
            assert!(
                cleared.set_inspection_target(None).is_ok(),
                "clearing the target must be accepted on the {:?} nature",
                state.kind()
            );
            assert_eq!(
                cleared,
                state,
                "clearing an absent target leaves the {:?} state unchanged",
                state.kind()
            );
        }
    }

    // T9 (4): the Debug of a state shows its identifier, its nature and the counts of its elements, never the content.
    #[test]
    fn debug_of_a_state_shows_no_content() {
        let mut state = minimal().with_kind(SessionKind::Config);
        state.history.push(Message::user("FAKE-SECRET in the history"));
        state.variables.extend([(
            "token".to_owned(),
            TaggedValue::String("FAKE-SECRET in a variable".to_owned()),
        )]);
        state.functions.extend([(
            "fetch/1".to_owned(),
            "fn fetch(x) { \"FAKE-SECRET in a source\" }".to_owned(),
        )]);
        let rendered = format!("{state:?}");
        for forbidden in ["FAKE-SECRET", "history", "token", "fetch"] {
            assert!(
                !rendered.contains(forbidden),
                "the Debug of a state must not show its content, but it shows {forbidden}: {rendered}"
            );
        }
        for expected in ["s-1", "Config", "variables", "functions"] {
            assert!(
                rendered.contains(expected),
                "the Debug of a state shows {expected}: {rendered}"
            );
        }
    }

    #[test]
    fn push_message_appends_in_order() {
        let mut state = minimal();
        state.push_message(Message::user("first"));
        state.push_message(Message::assistant("second", None, Vec::new()));
        assert_eq!(
            state.history().messages(),
            [
                Message::user("first"),
                Message::assistant("second", None, Vec::new())
            ],
            "messages must be appended in the order they were pushed"
        );
    }

    #[test]
    fn push_user_during_turn_waits_for_tool_results() {
        let mut state = minimal();
        state.push_message(Message::assistant(
            "",
            None,
            vec![ToolCall::new("c1", "rhai_pad", json!({"script": "1"}))],
        ));
        let refused = state.push_user_during_turn("late");
        assert!(
            matches!(refused, Err(SessionError::PendingToolCall)),
            "a user message must be refused while a call waits, got {refused:?}"
        );
        assert_eq!(
            state.history().messages().len(),
            1,
            "a refused message must not be appended"
        );
        state.push_message(Message::tool_result("c1", "rhai_pad", "1", None));
        state
            .push_user_during_turn("late")
            .expect("the message is accepted once the result is there");
        assert_eq!(
            state.history().messages().last(),
            Some(&Message::user("late")),
            "the accepted message must come after the result"
        );
    }

    #[test]
    fn push_segment_makes_the_new_segment_current() {
        let mut state = minimal();
        let context = SegmentContext::new("prompt", Vec::new(), ModelSnapshot::new("m", "none"));
        state.push_segment(Segment::new(
            BoundaryKind::Start,
            context.clone(),
            Timestamp::from_millis(1),
        ));
        state.push_segment(Segment::new(
            BoundaryKind::Compaction,
            context,
            Timestamp::from_millis(2),
        ));
        let kinds: Vec<BoundaryKind> = state.segments().iter().map(Segment::boundary).collect();
        assert_eq!(
            kinds,
            [BoundaryKind::Start, BoundaryKind::Compaction],
            "segments must keep the order they started in"
        );
    }

    #[test]
    fn set_pad_replaces_variables_and_functions() {
        let mut state = populated();
        state.set_pad(
            BTreeMap::from([("x".to_owned(), TaggedValue::Int(5))]),
            BTreeMap::from([("f/1".to_owned(), "fn f(a) { a }".to_owned())]),
        );
        assert_eq!(
            state.variables().keys().map(String::as_str).collect::<Vec<_>>(),
            ["x"],
            "the variables must be replaced, not merged"
        );
        assert_eq!(
            state.functions().keys().map(String::as_str).collect::<Vec<_>>(),
            ["f/1"],
            "the functions must be replaced, not merged"
        );
    }

    #[test]
    fn add_tokens_accumulates_and_saturates() {
        let mut state = minimal();
        state.add_tokens(TokenCounters::new(10, 20, 5));
        state.add_tokens(TokenCounters::new(1, 2, 3));
        assert_eq!(
            state.tokens(),
            TokenCounters::new(11, 22, 8),
            "counters must add up"
        );
        state.add_tokens(TokenCounters::new(u64::MAX, u64::MAX, u64::MAX));
        assert_eq!(
            state.tokens(),
            TokenCounters::new(u64::MAX, u64::MAX, u64::MAX),
            "counters must saturate instead of overflowing"
        );
    }

    #[test]
    fn record_signals_stores_by_turn_and_replaces() {
        let mut state = minimal();
        let mut first = RunSignals::new("a", "p", BTreeMap::new());
        first.finish(Outcome::Failed);
        state.record_signals(TurnId::new(1), first);
        let mut second = RunSignals::new("a", "p", BTreeMap::new());
        second.finish(Outcome::Valid);
        state.record_signals(TurnId::new(1), second);
        state.record_signals(TurnId::new(2), RunSignals::new("a", "p", BTreeMap::new()));
        assert_eq!(state.signals().len(), 2, "one record per turn");
        assert_eq!(
            state.signals().get(&TurnId::new(1)).and_then(RunSignals::outcome),
            Some(Outcome::Valid),
            "a second record of the same turn replaces the first"
        );
    }
}
