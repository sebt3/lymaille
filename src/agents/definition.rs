//! Agent definition, work, configuration or sub-agent: identifier, kind, templated system prompt, model profile name and parameters,
//! reachable modules and toolsets, sandbox limits, context strategy, delegable sub-agents or output schema, and its
//! fingerprint (`agent-sessions/agents#FR-1`, `FR-2`, `FR-8`, `agent-sessions/session#FR-2`). The definition carries no
//! Memory right: access to a Memory level is decided by the toolsets (`agent-sessions/modules#FR-4`).

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AgentsError, AgentsResult, SamplingParams};

/// Nature of an agent: a work agent does the work of a session, a configuration agent configures the host, and a
/// sub-agent is delegated to by a work agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentKind {
    /// A work agent, chosen at the start of a session. It may delegate to sub-agents and returns no output schema.
    #[serde(alias = "main")]
    Work,
    /// A configuration agent. It does not delegate, has no MCP module and receives the modules reserved to configuration.
    Config,
    /// A sub-agent, delegated to by a work agent. It returns an output schema and does not delegate.
    Sub,
}

/// Limits of the sandbox of an agent: the delay of a script and the render cap of an output, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SandboxLimits {
    script_timeout: Duration,
    render_cap_chars: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_string_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_array_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_map_len: Option<u64>,
}

impl SandboxLimits {
    /// Builds the limits from the script delay and the render cap in characters. The memory limits start absent.
    #[must_use]
    pub const fn new(script_timeout: Duration, render_cap_chars: u64) -> Self {
        Self {
            script_timeout,
            render_cap_chars,
            max_string_bytes: None,
            max_array_len: None,
            max_map_len: None,
        }
    }

    /// Sets the string limit, in bytes. Zero is accepted here and refused by [`AgentDefinition::validate`].
    #[must_use]
    pub const fn with_max_string_bytes(mut self, bytes: u64) -> Self {
        self.max_string_bytes = Some(bytes);
        self
    }

    /// Sets the array limit, in elements. Zero is accepted here and refused by [`AgentDefinition::validate`].
    #[must_use]
    pub const fn with_max_array_len(mut self, len: u64) -> Self {
        self.max_array_len = Some(len);
        self
    }

    /// Sets the map limit, in entries. Zero is accepted here and refused by [`AgentDefinition::validate`].
    #[must_use]
    pub const fn with_max_map_len(mut self, len: u64) -> Self {
        self.max_map_len = Some(len);
        self
    }

    /// Tells whether one of the three memory limits is set to zero, which the definition refuses.
    fn has_zero_memory_limit(&self) -> bool {
        [self.max_string_bytes, self.max_array_len, self.max_map_len].contains(&Some(0))
    }

    /// Returns the delay granted to a script.
    #[must_use]
    pub const fn script_timeout(&self) -> Duration {
        self.script_timeout
    }

    /// Returns the render cap of an output, in characters.
    #[must_use]
    pub const fn render_cap_chars(&self) -> u64 {
        self.render_cap_chars
    }

    /// Returns the string limit in bytes, if set.
    #[must_use]
    pub const fn max_string_bytes(&self) -> Option<u64> {
        self.max_string_bytes
    }

    /// Returns the array limit in elements, if set.
    #[must_use]
    pub const fn max_array_len(&self) -> Option<u64> {
        self.max_array_len
    }

    /// Returns the map limit in entries, if set.
    #[must_use]
    pub const fn max_map_len(&self) -> Option<u64> {
        self.max_map_len
    }
}

/// Definition of an agent. It designates its model by the name of a profile and never names a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AgentDefinition {
    id: String,
    kind: AgentKind,
    prompt: String,
    profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sampling: Option<SamplingParams>,
    #[serde(default)]
    modules: Vec<String>,
    #[serde(default)]
    toolsets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sandbox: Option<SandboxLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_strategy: Option<String>,
    #[serde(default)]
    delegable: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_schema: Option<Value>,
}

impl AgentDefinition {
    /// Builds a definition from its identifier, its nature, its system prompt template and the name of its profile.
    /// Every capability starts empty.
    #[must_use]
    pub const fn new(id: String, kind: AgentKind, prompt: String, profile: String) -> Self {
        Self {
            id,
            kind,
            prompt,
            profile,
            sampling: None,
            modules: Vec::new(),
            toolsets: Vec::new(),
            sandbox: None,
            context_strategy: None,
            delegable: Vec::new(),
            output_schema: None,
        }
    }

    /// Sets the sampling parameters.
    #[must_use]
    pub fn with_sampling(mut self, sampling: SamplingParams) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// Sets the modules reachable by the agent.
    #[must_use]
    pub fn with_modules(mut self, modules: Vec<String>) -> Self {
        self.modules = modules;
        self
    }

    /// Sets the toolsets reachable by the agent.
    #[must_use]
    pub fn with_toolsets(mut self, toolsets: Vec<String>) -> Self {
        self.toolsets = toolsets;
        self
    }

    /// Sets the sandbox limits.
    #[must_use]
    pub const fn with_sandbox(mut self, sandbox: SandboxLimits) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Sets the name of the context strategy.
    #[must_use]
    pub fn with_context_strategy(mut self, strategy: String) -> Self {
        self.context_strategy = Some(strategy);
        self
    }

    /// Sets the delegable sub-agents. Only a work agent (`Work`) may delegate: a sub-agent or a configuration agent that
    /// declares any is refused by [`AgentDefinition::validate`].
    #[must_use]
    pub fn with_delegable(mut self, delegable: Vec<String>) -> Self {
        self.delegable = delegable;
        self
    }

    /// Sets the JSON schema of the output of a sub-agent.
    #[must_use]
    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Returns the identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the nature of the agent.
    #[must_use]
    pub const fn kind(&self) -> AgentKind {
        self.kind
    }

    /// Returns the system prompt template.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the name of the model profile.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Returns the sampling parameters, if any.
    #[must_use]
    pub const fn sampling(&self) -> Option<&SamplingParams> {
        self.sampling.as_ref()
    }

    /// Returns the modules reachable by the agent.
    #[must_use]
    pub fn modules(&self) -> &[String] {
        &self.modules
    }

    /// Returns the toolsets reachable by the agent.
    #[must_use]
    pub fn toolsets(&self) -> &[String] {
        &self.toolsets
    }

    /// Returns the sandbox limits, if any.
    #[must_use]
    pub const fn sandbox(&self) -> Option<SandboxLimits> {
        self.sandbox
    }

    /// Returns the name of the context strategy, if any.
    #[must_use]
    pub fn context_strategy(&self) -> Option<&str> {
        self.context_strategy.as_deref()
    }

    /// Returns the delegable sub-agents, in the order of the definition. Empty for any nature but `Work`.
    #[must_use]
    pub fn delegable(&self) -> &[String] {
        &self.delegable
    }

    /// Returns the JSON schema of the output of a sub-agent, if any.
    #[must_use]
    pub const fn output_schema(&self) -> Option<&Value> {
        self.output_schema.as_ref()
    }

    /// Checks that the definition is consistent.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::InvalidDefinition`] when the identifier, the prompt or the profile name is empty, when a
    /// sub-agent or a configuration agent declares delegable sub-agents, when a work or configuration agent declares an
    /// output schema, when a configuration module is granted to a work agent or a sub-agent, when a delegable sub-agent
    /// is the agent itself or is listed twice, when the toolsets or the modules contain a duplicate, or when a memory
    /// limit of the sandbox is zero.
    pub fn validate(&self) -> AgentsResult<()> {
        if self.id.trim().is_empty() {
            return self.refuse("identifier is empty");
        }
        if self.prompt.trim().is_empty() {
            return self.refuse("prompt is empty");
        }
        if self.profile.trim().is_empty() {
            return self.refuse("profile name is empty");
        }
        if matches!(self.kind, AgentKind::Sub | AgentKind::Config) && !self.delegable.is_empty() {
            return self.refuse("a sub-agent or a configuration agent cannot declare delegable sub-agents");
        }
        if matches!(self.kind, AgentKind::Work | AgentKind::Config) && self.output_schema.is_some() {
            return self.refuse("a work or configuration agent cannot declare an output schema");
        }
        if self.delegable.contains(&self.id) {
            return self.refuse("a delegable sub-agent is the agent itself");
        }
        if has_duplicate(&self.delegable) {
            return self.refuse("duplicate delegable sub-agent");
        }
        if has_duplicate(&self.toolsets) {
            return self.refuse("duplicate toolset");
        }
        if has_duplicate(&self.modules) {
            return self.refuse("duplicate module");
        }
        if matches!(self.kind, AgentKind::Work | AgentKind::Sub)
            && self
                .modules
                .iter()
                .any(|module| CONFIGURATION_MODULES.contains(&module.as_str()))
        {
            return self.refuse("a configuration module is granted to a work agent or a sub-agent");
        }
        if self
            .sandbox
            .is_some_and(|sandbox| sandbox.has_zero_memory_limit())
        {
            return self.refuse("a sandbox memory limit is zero");
        }
        Ok(())
    }

    /// Writes the definition as text: a YAML frontmatter of every field but the prompt, then the prompt as the body, exactly.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::InvalidDefinition`] when the definition does not serialize, which no definition built by this
    /// module does.
    pub fn to_text(&self) -> AgentsResult<String> {
        let mut head = serde_json::to_value(self)
            .ok()
            .ok_or_else(|| text_refusal("the definition does not serialize"))?;
        let object = head
            .as_object_mut()
            .ok_or_else(|| text_refusal("the definition is not a table of fields"))?;
        let _prompt = object.remove("prompt");
        let yaml = yaml_serde::to_string(&*object)
            .ok()
            .ok_or_else(|| text_refusal("the frontmatter does not serialize"))?;
        let line_break = if yaml.ends_with('\n') { "" } else { "\n" };
        Ok(format!("{FENCE}\n{yaml}{line_break}{FENCE}\n{}", self.prompt))
    }

    /// Reads a text of the format of [`AgentDefinition::to_text`]: the frontmatter is read as YAML, the body is the prompt.
    /// Unknown frontmatter fields are ignored. The result is not validated: call [`AgentDefinition::validate`] for that.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::InvalidDefinition`] when the frontmatter is missing or not closed, when it is not readable
    /// YAML of a definition, or when it holds a `prompt` field, which the body is. The refusal carries no value of the text.
    pub fn from_text(text: &str) -> AgentsResult<Self> {
        let (frontmatter, prompt) = split_text(text)
            .ok_or_else(|| text_refusal("the frontmatter is missing or is not closed by a `---` line"))?;
        let mut head: Value = yaml_serde::from_str(&frontmatter)
            .ok()
            .ok_or_else(|| text_refusal("the frontmatter is not readable YAML of a definition"))?;
        let object = head
            .as_object_mut()
            .ok_or_else(|| text_refusal("the frontmatter is not a table of fields"))?;
        if object.contains_key("prompt") {
            return Err(text_refusal(
                "the frontmatter holds a prompt field, the prompt is the body",
            ));
        }
        let _previous = object.insert("prompt".to_owned(), Value::String(prompt));
        serde_json::from_value::<Self>(head)
            .ok()
            .ok_or_else(|| text_refusal("the frontmatter is not readable YAML of a definition"))
    }

    /// Returns the fingerprint of the definition: 16 hexadecimal digits of FNV-1a 64 bits over its canonical JSON.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        // A definition is made of strings, numbers, booleans, sequences and maps only: its serialization cannot fail,
        // so the empty fallback is never taken.
        let value = serde_json::to_value(self).unwrap_or_default();
        let text = serde_json::to_string(&canonical(value)).unwrap_or_default();
        let hash = fnv1a_64(text.as_bytes());
        format!("{hash:016x}")
    }

    /// Builds the refusal of the definition for `reason`.
    fn refuse(&self, reason: &'static str) -> AgentsResult<()> {
        Err(AgentsError::InvalidDefinition {
            id: self.id.clone(),
            reason,
        })
    }
}

/// Modules reserved to a configuration agent: a work agent and a sub-agent never receive them (`agent-sessions#BR-8`).
const CONFIGURATION_MODULES: [&str; 4] = ["toolset_creator", "inspect", "contexts", "sessions"];

/// Tells whether two entries of `items` are equal.
fn has_duplicate(items: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    items.iter().any(|item| !seen.insert(item.as_str()))
}

/// Sorts the keys of every object of `value`, recursively. The result does not depend on the map type of `serde_json`,
/// so the fingerprint stays the same whatever features another crate enables.
fn canonical(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        Value::Object(entries) => {
            let mut sorted: Vec<(String, Value)> = entries
                .into_iter()
                .map(|(key, item)| (key, canonical(item)))
                .collect();
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(sorted.into_iter().collect())
        }
        scalar @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => scalar,
    }
}

/// FNV-1a offset basis for 64 bits.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a prime for 64 bits.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over 64 bits. The multiplication is taken modulo 2^64 by definition of the hash, which is what `wrapping_mul`
/// states; no other arithmetic is involved.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Line that opens and closes the frontmatter of a text.
const FENCE: &str = "---";

/// Splits a text into its frontmatter and its body. The opening `---` line must be the first line; the frontmatter runs to
/// the first following `---` line, and the body is what follows that line, unchanged. CRLF line endings are accepted.
fn split_text(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (opening, rest) = lines.split_first()?;
    if line_text(opening) != FENCE {
        return None;
    }
    let closing = rest.iter().position(|line| line_text(line) == FENCE)?;
    let (frontmatter, after) = rest.split_at_checked(closing)?;
    let body: String = after.iter().skip(1).copied().collect();
    Some((frontmatter.concat(), body))
}

/// The text of a line without its line break, LF or CRLF.
fn line_text(line: &str) -> &str {
    let without_newline = line.strip_suffix('\n').unwrap_or(line);
    without_newline.strip_suffix('\r').unwrap_or(without_newline)
}

/// Identifier that a refusal of a text carries: the definition is not known for certain when the text does not read.
const TEXT_ID: &str = "<AGENT.md>";

/// The refusal of a text that does not read as a definition, with a fixed reason.
fn text_refusal(reason: &'static str) -> AgentsError {
    AgentsError::InvalidDefinition {
        id: TEXT_ID.to_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{AgentDefinition, AgentKind, SandboxLimits, fnv1a_64};
    use crate::agents::{AgentsError, SamplingParams};

    const PROMPT: &str = "You are the assistant of {{project}}.";
    const PROFILE: &str = "strix-qwen";

    fn main_agent() -> AgentDefinition {
        AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Work,
            "You are the assistant of {{project}}.".to_owned(),
            "strix-qwen".to_owned(),
        )
    }

    fn sub_agent() -> AgentDefinition {
        AgentDefinition::new(
            "reviewer".to_owned(),
            AgentKind::Sub,
            "Review the change.".to_owned(),
            "strix-qwen".to_owned(),
        )
    }

    /// A sandbox with a script delay of `seconds`, a render cap of `render_cap` and the three optional memory limits.
    fn sandbox(
        seconds: u64,
        render_cap: u64,
        string: Option<u64>,
        array: Option<u64>,
        map: Option<u64>,
    ) -> SandboxLimits {
        let mut limits = SandboxLimits::new(Duration::from_secs(seconds), render_cap);
        if let Some(bytes) = string {
            limits = limits.with_max_string_bytes(bytes);
        }
        if let Some(len) = array {
            limits = limits.with_max_array_len(len);
        }
        if let Some(len) = map {
            limits = limits.with_max_map_len(len);
        }
        limits
    }

    /// A definition with every field set, under the identifier `id`, the kind `kind`, the prompt `prompt` and the profile
    /// `profile`.
    fn full_definition(id: &str, kind: AgentKind, prompt: &str, profile: &str) -> AgentDefinition {
        let sampling = SamplingParams::default()
            .with_temperature(0.7)
            .with_top_p(0.9)
            .with_top_k(40)
            .with_extra("seed".to_owned(), serde_json::json!(42));
        AgentDefinition::new(id.to_owned(), kind, prompt.to_owned(), profile.to_owned())
            .with_sampling(sampling)
            .with_modules(vec!["files".to_owned(), "git".to_owned()])
            .with_toolsets(vec!["k8s".to_owned(), "logs".to_owned()])
            .with_sandbox(sandbox(30, 10_000, Some(1_000), Some(500), Some(200)))
            .with_context_strategy("summarize".to_owned())
            .with_delegable(vec!["reviewer".to_owned()])
    }

    fn full_main_agent() -> AgentDefinition {
        full_definition("main", AgentKind::Work, PROMPT, PROFILE)
    }

    fn assert_invalid(definition: &AgentDefinition, case: &str) {
        let result = definition.validate();
        assert!(
            matches!(result, Err(AgentsError::InvalidDefinition { .. })),
            "{case} must fail with InvalidDefinition, got {result:?}"
        );
    }

    /// The prompt of the text tests, after the frontmatter.
    fn body_of(text: &str) -> &str {
        text.split_once("\n---\n").map_or("", |(_head, body)| body)
    }

    // Scenario: définition aller-retour texte
    #[test]
    fn text_round_trip_is_identical() {
        let definition = full_main_agent();
        let text = definition.to_text().expect("a definition is written");
        let back = AgentDefinition::from_text(&text).expect("the text is read back");
        assert_eq!(back, definition, "the round trip must be identical");
        assert_eq!(
            back.fingerprint(),
            definition.fingerprint(),
            "the fingerprint is the same"
        );
    }

    #[test]
    fn text_round_trip_keeps_the_output_schema_of_a_sub_agent() {
        let definition = sub_agent().with_output_schema(serde_json::json!({
            "type": "object",
            "properties": { "ok": { "type": "boolean" } }
        }));
        let text = definition.to_text().expect("a sub-agent is written");
        let back = AgentDefinition::from_text(&text).expect("the text is read back");
        assert_eq!(back, definition, "the output schema survives the round trip");
    }

    // Scenario: le texte ne nomme que le profil
    #[test]
    fn text_names_the_profile_and_no_provider() {
        let text = main_agent().to_text().expect("a definition is written");
        assert!(
            text.starts_with("---\n"),
            "the frontmatter opens the text: {text}"
        );
        assert!(
            text.contains("\nprofile: strix-qwen\n"),
            "the profile name is written: {text}"
        );
        assert!(
            !text.contains("provider"),
            "no provider field in the text: {text}"
        );
        assert!(
            text.ends_with(&format!("\n---\n{PROMPT}")),
            "the prompt follows the closing line exactly: {text}"
        );
    }

    // Scenario: champ inconnu toléré à la relecture
    #[test]
    fn unknown_text_field_is_tolerated() {
        let text = main_agent().to_text().expect("a definition is written");
        let with_extra = text.replacen(
            "profile: strix-qwen\n",
            "profile: strix-qwen\nauthor: someone\n",
            1,
        );
        let back = AgentDefinition::from_text(&with_extra).expect("the unknown field is tolerated");
        assert_eq!(back, main_agent(), "the unknown field changes nothing");
    }

    // Scenario: prompt exact, lignes de tirets comprises
    #[test]
    fn prompt_is_kept_byte_for_byte() {
        let prompt = "\nFirst line.\n---\nLast line.\n\n";
        let definition = AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Work,
            prompt.to_owned(),
            PROFILE.to_owned(),
        );
        let text = definition.to_text().expect("a definition is written");
        assert_eq!(
            body_of(&text),
            prompt,
            "the body is the prompt, nothing added or trimmed"
        );
        let back = AgentDefinition::from_text(&text).expect("the text is read back");
        assert_eq!(back.prompt(), prompt, "the prompt is read back exactly");
    }

    #[test]
    fn absent_memory_limits_are_not_written() {
        let definition = AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Work,
            PROMPT.to_owned(),
            PROFILE.to_owned(),
        )
        .with_sandbox(sandbox(30, 10_000, None, None, None));
        let text = definition.to_text().expect("a definition is written");
        assert!(
            !text.contains("max_string_bytes"),
            "an absent limit is not written: {text}"
        );
        assert!(
            !text.contains("max_map_len"),
            "an absent limit is not written: {text}"
        );
    }

    // Scenario: texte refusé sans contenu
    #[test]
    fn malformed_text_refused_without_its_content() {
        let text = "---\nid: [SECRET-ID\nkind: work\n---\nSECRET-PROMPT-VALUE\n";
        match AgentDefinition::from_text(text) {
            Err(error @ AgentsError::InvalidDefinition { .. }) => {
                let shown = error.to_string();
                assert!(
                    !shown.contains("SECRET"),
                    "the refusal carries no value of the text: {shown}"
                );
            }
            other => panic!("malformed YAML must fail with InvalidDefinition, got {other:?}"),
        }
    }

    #[test]
    fn field_of_the_wrong_type_refused_without_its_content() {
        let text = "---\nid: [1, 2]\nkind: work\nprofile: strix-qwen\n---\nbody\n";
        match AgentDefinition::from_text(text) {
            Err(error @ AgentsError::InvalidDefinition { .. }) => assert!(
                !error.to_string().contains("1, 2"),
                "the refusal carries no value of the text: {error}"
            ),
            other => panic!("a wrong type must fail with InvalidDefinition, got {other:?}"),
        }
    }

    #[test]
    fn prompt_field_in_the_frontmatter_refused() {
        let text = "---\nid: main\nkind: work\nprofile: strix-qwen\nprompt: PROMPT-IN-HEAD\n---\nbody\n";
        match AgentDefinition::from_text(text) {
            Err(error @ AgentsError::InvalidDefinition { .. }) => assert!(
                !error.to_string().contains("PROMPT-IN-HEAD"),
                "the refusal carries no value of the text: {error}"
            ),
            other => panic!("a prompt field in the frontmatter must be refused, got {other:?}"),
        }
    }

    #[test]
    fn crlf_text_is_read_and_the_prompt_keeps_its_line_breaks() {
        let text = "---\r\nid: main\r\nkind: work\r\nprofile: strix-qwen\r\n---\r\nbody\r\n";
        let back = AgentDefinition::from_text(text).expect("CRLF line endings are read");
        assert_eq!(
            back.prompt(),
            "body\r\n",
            "the closing line and its line break are removed, nothing else"
        );
    }

    #[test]
    fn frontmatter_must_open_the_text() {
        let text = "intro\nid: main\nkind: work\nprofile: strix-qwen\n---\nbody\n";
        assert!(
            matches!(
                AgentDefinition::from_text(text),
                Err(AgentsError::InvalidDefinition { .. })
            ),
            "a text whose first line is not `---` has no frontmatter"
        );
    }

    // Scenario: frontmatter absent ou non fermé refusé
    #[test]
    fn missing_or_unclosed_frontmatter_refused() {
        for text in [
            "id: main\nkind: work\nprofile: strix-qwen\nbody\n",
            "---\nid: main\nkind: work\nprofile: strix-qwen\nbody\n",
        ] {
            assert!(
                matches!(
                    AgentDefinition::from_text(text),
                    Err(AgentsError::InvalidDefinition { .. })
                ),
                "a text without a closed frontmatter must fail: {text:?}"
            );
        }
    }

    #[test]
    fn model_by_profile_name_only() {
        let value = serde_json::to_value(main_agent()).expect("a definition serializes");
        assert_eq!(
            value.get("profile").and_then(serde_json::Value::as_str),
            Some("strix-qwen"),
            "the JSON must carry the profile name"
        );
        assert!(
            value.get("provider").is_none(),
            "the JSON must have no provider field: {value}"
        );
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let definition = full_main_agent();
        let first = definition.fingerprint();
        assert_eq!(
            first,
            definition.fingerprint(),
            "two computations on the same definition must agree"
        );
        let text = serde_json::to_string(&definition).expect("a definition serializes");
        let back: AgentDefinition = serde_json::from_str(&text).expect("a definition deserializes");
        assert_eq!(
            first,
            back.fingerprint(),
            "the fingerprint must survive a JSON round trip"
        );
        let changed = definition.with_toolsets(vec!["k8s".to_owned(), "other".to_owned()]);
        assert_ne!(
            first,
            changed.fingerprint(),
            "changing one toolset must change the fingerprint"
        );
        assert_eq!(first.len(), 16, "the fingerprint has 16 hexadecimal digits");
    }

    #[test]
    fn no_capability_by_default() {
        let text = r#"{"id":"main","kind":"main","prompt":"hello","profile":"strix-qwen"}"#;
        let definition: AgentDefinition = serde_json::from_str(text).expect("the required fields are enough");
        assert!(definition.modules().is_empty(), "no module is granted by default");
        assert!(
            definition.toolsets().is_empty(),
            "no toolset is granted by default"
        );
    }

    #[test]
    fn no_memory_rights_in_the_definition() {
        let definition = main_agent();
        let value = serde_json::to_value(&definition).expect("a definition serializes");
        assert!(
            value.get("memory").is_none(),
            "the definition carries no Memory right: its JSON must have no `memory` field, got {value}"
        );
        let old = r#"{"id":"main","kind":"main","prompt":"You are the assistant of {{project}}.","profile":"strix-qwen","memory":{"read":true,"write_user":true}}"#;
        let read: AgentDefinition =
            serde_json::from_str(old).expect("an old JSON with a memory field still reads");
        assert_eq!(
            read, definition,
            "the `memory` field of an old JSON is ignored: the definition reads as without it"
        );
        assert_eq!(
            read.fingerprint(),
            definition.fingerprint(),
            "the ignored `memory` field must not change the fingerprint"
        );
    }

    #[test]
    fn sub_agent_cannot_delegate() {
        let definition = sub_agent().with_delegable(vec!["other".to_owned()]);
        assert_invalid(&definition, "a sub-agent declaring delegable sub-agents");
    }

    #[test]
    fn main_agent_has_no_output_schema() {
        let definition = main_agent().with_output_schema(serde_json::json!({"type": "object"}));
        assert_invalid(&definition, "a main agent declaring an output schema");
    }

    #[test]
    fn self_delegation_and_duplicates_refused() {
        let self_delegation = main_agent().with_delegable(vec!["main".to_owned()]);
        assert_invalid(&self_delegation, "a main agent listing itself as delegable");
        let duplicated = main_agent().with_toolsets(vec!["k8s".to_owned(), "k8s".to_owned()]);
        assert_invalid(&duplicated, "a definition with a duplicated toolset");
    }

    #[test]
    fn round_trip() {
        let definition = full_main_agent();
        let text = serde_json::to_string(&definition).expect("a definition serializes");
        let back: AgentDefinition = serde_json::from_str(&text).expect("a definition deserializes");
        assert_eq!(back, definition, "serialized then deserialized must be identical");
    }

    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(
            fnv1a_64(b""),
            0xcbf2_9ce4_8422_2325,
            "the empty input hashes to the offset basis"
        );
        assert_eq!(
            fnv1a_64(b"a"),
            0xaf63_dc4c_8601_ec8c,
            "FNV-1a 64 of `a` must match the reference vector"
        );
        assert_eq!(
            fnv1a_64(b"foobar"),
            0x8594_4171_f739_67e8,
            "FNV-1a 64 of `foobar` must match the reference vector"
        );
    }

    #[test]
    fn fingerprint_changes_for_each_field() {
        let base = full_main_agent().fingerprint();
        let sampling = SamplingParams::default().with_temperature(0.1);
        let cases = [
            (
                "identifier",
                full_definition("other", AgentKind::Work, PROMPT, PROFILE),
            ),
            ("kind", full_definition("main", AgentKind::Sub, PROMPT, PROFILE)),
            (
                "prompt",
                full_definition("main", AgentKind::Work, "Another prompt.", PROFILE),
            ),
            (
                "profile",
                full_definition("main", AgentKind::Work, PROMPT, "other-profile"),
            ),
            ("sampling", full_main_agent().with_sampling(sampling)),
            (
                "modules",
                full_main_agent().with_modules(vec!["files".to_owned()]),
            ),
            (
                "toolsets",
                full_main_agent().with_toolsets(vec!["k8s".to_owned()]),
            ),
            (
                "sandbox script timeout",
                full_main_agent().with_sandbox(sandbox(31, 10_000, Some(1_000), Some(500), Some(200))),
            ),
            (
                "sandbox render cap",
                full_main_agent().with_sandbox(sandbox(30, 10_001, Some(1_000), Some(500), Some(200))),
            ),
            (
                "sandbox string limit value",
                full_main_agent().with_sandbox(sandbox(30, 10_000, Some(1_001), Some(500), Some(200))),
            ),
            (
                "sandbox array limit value",
                full_main_agent().with_sandbox(sandbox(30, 10_000, Some(1_000), Some(501), Some(200))),
            ),
            (
                "sandbox map limit value",
                full_main_agent().with_sandbox(sandbox(30, 10_000, Some(1_000), Some(500), Some(201))),
            ),
            (
                "sandbox string limit removed",
                full_main_agent().with_sandbox(sandbox(30, 10_000, None, Some(500), Some(200))),
            ),
            (
                "sandbox array limit removed",
                full_main_agent().with_sandbox(sandbox(30, 10_000, Some(1_000), None, Some(200))),
            ),
            (
                "sandbox map limit removed",
                full_main_agent().with_sandbox(sandbox(30, 10_000, Some(1_000), Some(500), None)),
            ),
            (
                "context strategy",
                full_main_agent().with_context_strategy("other".to_owned()),
            ),
            (
                "delegable sub-agents",
                full_main_agent().with_delegable(vec!["other".to_owned()]),
            ),
        ];
        for (field, changed) in cases {
            assert_ne!(
                changed.fingerprint(),
                base,
                "changing the {field} must change the fingerprint"
            );
        }
    }

    #[test]
    fn fingerprint_changes_for_output_schema() {
        let plain = sub_agent().fingerprint();
        let with_object = sub_agent().with_output_schema(serde_json::json!({"type": "object"}));
        assert_ne!(
            with_object.fingerprint(),
            plain,
            "adding an output schema must change the fingerprint"
        );
        let with_string = sub_agent().with_output_schema(serde_json::json!({"type": "string"}));
        assert_ne!(
            with_string.fingerprint(),
            with_object.fingerprint(),
            "changing the output schema must change the fingerprint"
        );
    }

    #[test]
    fn complete_definitions_pass_validation() {
        let main = full_main_agent().validate();
        assert!(
            matches!(main, Ok(())),
            "a complete main agent must be valid, got {main:?}"
        );
        let sub = sub_agent()
            .with_output_schema(serde_json::json!({"type": "object"}))
            .validate();
        assert!(
            matches!(sub, Ok(())),
            "a sub-agent with an output schema must be valid, got {sub:?}"
        );
    }

    #[test]
    fn blank_identifier_prompt_and_profile_refused() {
        let cases = [
            (
                "an empty identifier",
                full_definition("", AgentKind::Work, PROMPT, PROFILE),
            ),
            (
                "a blank identifier",
                full_definition("  ", AgentKind::Work, PROMPT, PROFILE),
            ),
            (
                "an empty prompt",
                full_definition("main", AgentKind::Work, "", PROFILE),
            ),
            (
                "a blank prompt",
                full_definition("main", AgentKind::Work, " \n\t ", PROFILE),
            ),
            (
                "an empty profile name",
                full_definition("main", AgentKind::Work, PROMPT, ""),
            ),
            (
                "a blank profile name",
                full_definition("main", AgentKind::Work, PROMPT, "   "),
            ),
        ];
        for (case, definition) in cases {
            assert_invalid(&definition, case);
        }
    }

    #[test]
    fn duplicate_module_refused() {
        let duplicated = main_agent().with_modules(vec!["files".to_owned(), "files".to_owned()]);
        assert_invalid(&duplicated, "a definition with a duplicated module");
    }

    #[test]
    fn memory_limits_absent_by_default() {
        let limits = SandboxLimits::new(Duration::from_secs(30), 10_000);
        assert_eq!(
            limits.max_string_bytes(),
            None,
            "the string limit is absent by default"
        );
        assert_eq!(
            limits.max_array_len(),
            None,
            "the array limit is absent by default"
        );
        assert_eq!(limits.max_map_len(), None, "the map limit is absent by default");
        let value = serde_json::to_value(main_agent().with_sandbox(limits)).expect("a definition serializes");
        let json = value.get("sandbox").expect("the sandbox is serialized");
        for key in ["max_string_bytes", "max_array_len", "max_map_len"] {
            assert!(
                json.get(key).is_none(),
                "the absent `{key}` must be omitted: {json}"
            );
        }
    }

    #[test]
    fn memory_limits_set_and_read_back() {
        let limits = SandboxLimits::new(Duration::from_secs(30), 10_000)
            .with_max_string_bytes(1_000)
            .with_max_array_len(500)
            .with_max_map_len(200);
        assert_eq!(
            limits.max_string_bytes(),
            Some(1_000),
            "the string limit is read back"
        );
        assert_eq!(limits.max_array_len(), Some(500), "the array limit is read back");
        assert_eq!(limits.max_map_len(), Some(200), "the map limit is read back");
        assert_eq!(
            limits.script_timeout(),
            Duration::from_secs(30),
            "the script delay is kept"
        );
        assert_eq!(limits.render_cap_chars(), 10_000, "the render cap is kept");
        let definition = main_agent().with_sandbox(limits);
        let text = serde_json::to_string(&definition).expect("a definition serializes");
        let back: AgentDefinition = serde_json::from_str(&text).expect("a definition deserializes");
        assert_eq!(back, definition, "the memory limits survive a round trip");
    }

    #[test]
    fn old_sandbox_json_reads_without_memory_limits() {
        let text = r#"{"id":"main","kind":"main","prompt":"hello","profile":"strix-qwen","sandbox":{"script_timeout":{"secs":30,"nanos":0},"render_cap_chars":10000}}"#;
        let definition: AgentDefinition =
            serde_json::from_str(text).expect("an old sandbox JSON still reads");
        let limits = definition.sandbox().expect("the old sandbox is read");
        assert_eq!(limits.render_cap_chars(), 10_000, "the render cap is read");
        assert_eq!(
            limits.max_string_bytes(),
            None,
            "an absent string limit reads as None"
        );
        assert_eq!(
            limits.max_array_len(),
            None,
            "an absent array limit reads as None"
        );
        assert_eq!(limits.max_map_len(), None, "an absent map limit reads as None");
    }

    #[test]
    fn zero_memory_limit_refused() {
        let cases = [
            (
                "a zero string limit",
                sandbox(30, 10_000, Some(0), Some(500), Some(200)),
            ),
            (
                "a zero array limit",
                sandbox(30, 10_000, Some(1_000), Some(0), Some(200)),
            ),
            (
                "a zero map limit",
                sandbox(30, 10_000, Some(1_000), Some(500), Some(0)),
            ),
        ];
        for (case, limits) in cases {
            assert_invalid(&main_agent().with_sandbox(limits), case);
        }
    }

    #[test]
    fn complete_definition_with_memory_limits_is_valid() {
        let limits = sandbox(30, 10_000, Some(1_000), Some(500), Some(200));
        let valid = main_agent().with_sandbox(limits).validate();
        assert!(
            matches!(valid, Ok(())),
            "a sandbox with positive memory limits must be valid, got {valid:?}"
        );
    }

    /// Builds a definition of nature `kind` that grants the modules `modules`.
    fn with_modules_of(kind: AgentKind, modules: &[&str]) -> AgentDefinition {
        AgentDefinition::new(
            "agent".to_owned(),
            kind,
            "You are an agent.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_modules(modules.iter().map(|module| (*module).to_owned()).collect())
    }

    // Scenario: configuration agent cannot delegate
    #[test]
    fn configuration_agent_cannot_delegate() {
        let config = AgentDefinition::new(
            "setup".to_owned(),
            AgentKind::Config,
            "You configure the host.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_delegable(vec!["worker".to_owned()]);
        assert_invalid(&config, "a configuration agent declaring a delegable sub-agent");
    }

    // Scenario: configuration agent with its modules is valid
    #[test]
    fn configuration_agent_with_its_modules_is_valid() {
        let config = with_modules_of(
            AgentKind::Config,
            &["toolset_creator", "inspect", "contexts", "sessions"],
        );
        let valid = config.validate();
        assert!(
            matches!(valid, Ok(())),
            "a configuration agent granted the configuration modules must be valid, got {valid:?}"
        );
    }

    // Scenario: work agent cannot receive the creation module
    #[test]
    fn work_agent_cannot_receive_the_creation_module() {
        let work = with_modules_of(AgentKind::Work, &["toolset_creator"]);
        assert_invalid(&work, "a work agent granted the toolset-creation module");
    }

    // Scenario: sub-agent cannot receive an inspection module
    #[test]
    fn sub_agent_cannot_receive_an_inspection_module() {
        let sub = with_modules_of(AgentKind::Sub, &["inspect"]);
        assert_invalid(&sub, "a sub-agent granted the inspection module");
    }

    /// Every reserved module refused for a work agent and for a sub-agent: the list is that of the definition.
    #[test]
    fn every_reserved_module_refused_for_work_and_sub_agents() {
        for module in ["toolset_creator", "inspect", "contexts", "sessions"] {
            assert_invalid(
                &with_modules_of(AgentKind::Work, &[module]),
                &format!("a work agent granted `{module}`"),
            );
            assert_invalid(
                &with_modules_of(AgentKind::Sub, &[module]),
                &format!("a sub-agent granted `{module}`"),
            );
        }
    }

    #[test]
    fn configuration_agent_with_output_schema_is_refused() {
        let config = AgentDefinition::new(
            "setup".to_owned(),
            AgentKind::Config,
            "You configure the host.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_output_schema(serde_json::json!({"type": "object"}));
        assert_invalid(&config, "a configuration agent declaring an output schema");
    }

    #[test]
    fn legacy_main_kind_reads_as_work() {
        let text = r#"{"id":"main","kind":"main","prompt":"hello","profile":"strix-qwen"}"#;
        let definition: AgentDefinition = serde_json::from_str(text).expect("the legacy kind still reads");
        assert_eq!(
            definition.kind(),
            AgentKind::Work,
            "the legacy `main` kind must read as `work`"
        );
        let value = serde_json::to_value(&definition).expect("a definition serializes");
        assert_eq!(
            value.get("kind").and_then(serde_json::Value::as_str),
            Some("work"),
            "a work agent serializes as `work`"
        );
    }

    #[test]
    fn duplicate_delegable_sub_agent_refused() {
        let duplicated = main_agent().with_delegable(vec!["worker".to_owned(), "worker".to_owned()]);
        assert_invalid(
            &duplicated,
            "a work agent listing the same delegable sub-agent twice",
        );
    }
}
