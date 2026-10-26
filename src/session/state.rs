//! Persisted state of a session and its JSON form. The state holds the history, the segments, the scratchpad variables and
//! functions, the todo, the watchers, the run handles, the token counters, the agent reference, the toolset context and
//! versions, the labels, and the quality signals of each turn (`agent-sessions/session#FR-2`, `agent-sessions/session#AC-1`,
//! `agent-sessions/session#AC-5`). It holds names and references only, never a secret (`agent-sessions#CON-3`). Reading and
//! writing work on text: the storage belongs to `SessionStore`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    FormatVersion, History, Labels, RunSignals, Segment, SessionError, SessionId, SessionResult, TaggedValue,
    TurnId,
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

/// Persisted state of one session. Built by [`SessionState::new`], read by [`SessionState::from_json`], rendered by
/// [`SessionState::to_json`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SessionState {
    id: SessionId,
    format: FormatVersion,
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
    /// [`SessionError::Serialization`] when the text is not a valid state.
    pub fn from_json(text: &str) -> SessionResult<Self> {
        let header: FormatHeader = serde_json::from_str(text).map_err(|error| read_error(&error))?;
        header.format.check()?;
        let record: StateRecord = serde_json::from_str(text).map_err(|error| read_error(&error))?;
        Ok(record.into_state())
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

    use super::{AgentRef, SessionState, TokenCounters};
    use crate::clock::{ManualClock, Timestamp};
    use crate::session::{
        BoundaryKind, FormatVersion, LabelAuthor, LabelTarget, Message, ModelSnapshot, Outcome, RunSignals,
        Segment, SegmentContext, SessionError, SessionId, TaggedValue, TurnId,
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
            FormatVersion::new(1, 9),
            "the format read must be 1.9"
        );
        let rendered = state.to_json().expect("the loaded state must serialize");
        assert!(
            !rendered.contains("future_field"),
            "the unknown field must be ignored, not carried into the state"
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
}
