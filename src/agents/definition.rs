//! Agent definition, main or sub-agent: identifier, kind, templated system prompt, model profile name and parameters,
//! reachable modules and toolsets, Memory rights per level, sandbox limits, context strategy, delegable sub-agents or
//! output schema, and its fingerprint (`agent-sessions/agents#FR-1`, `FR-2`, `FR-8`, `agent-sessions/session#FR-2`).

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AgentsError, AgentsResult, SamplingParams};

/// Nature of an agent: a main agent starts a session, a sub-agent is delegated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentKind {
    /// A main agent, chosen at the start of a session. It may delegate to sub-agents.
    Main,
    /// A sub-agent, delegated to by a main agent. It returns an output schema and does not delegate.
    Sub,
}

/// Rights of the Memory granted to an agent. Every right is absent by default: nothing is granted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four rights are independent permissions of the Memory, not a state machine"
)]
#[non_exhaustive]
pub struct MemoryRights {
    #[serde(default)]
    read: bool,
    #[serde(default)]
    write_user: bool,
    #[serde(default)]
    write_project: bool,
    #[serde(default)]
    write_session: bool,
}

impl MemoryRights {
    /// Sets the read right.
    #[must_use]
    pub const fn with_read(mut self, allowed: bool) -> Self {
        self.read = allowed;
        self
    }

    /// Sets the write right on the user level.
    #[must_use]
    pub const fn with_write_user(mut self, allowed: bool) -> Self {
        self.write_user = allowed;
        self
    }

    /// Sets the write right on the project level.
    #[must_use]
    pub const fn with_write_project(mut self, allowed: bool) -> Self {
        self.write_project = allowed;
        self
    }

    /// Sets the write right on the session level.
    #[must_use]
    pub const fn with_write_session(mut self, allowed: bool) -> Self {
        self.write_session = allowed;
        self
    }

    /// Tells whether the Memory can be read.
    #[must_use]
    pub const fn read(self) -> bool {
        self.read
    }

    /// Tells whether the user level can be written.
    #[must_use]
    pub const fn write_user(self) -> bool {
        self.write_user
    }

    /// Tells whether the project level can be written.
    #[must_use]
    pub const fn write_project(self) -> bool {
        self.write_project
    }

    /// Tells whether the session level can be written.
    #[must_use]
    pub const fn write_session(self) -> bool {
        self.write_session
    }
}

/// Limits of the sandbox of an agent: the delay of a script and the render cap of an output, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SandboxLimits {
    script_timeout: Duration,
    render_cap_chars: u64,
}

impl SandboxLimits {
    /// Builds the limits from the script delay and the render cap in characters.
    #[must_use]
    pub const fn new(script_timeout: Duration, render_cap_chars: u64) -> Self {
        Self {
            script_timeout,
            render_cap_chars,
        }
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
    #[serde(default)]
    memory: MemoryRights,
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
            memory: MemoryRights {
                read: false,
                write_user: false,
                write_project: false,
                write_session: false,
            },
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

    /// Sets the Memory rights of the agent.
    #[must_use]
    pub const fn with_memory(mut self, memory: MemoryRights) -> Self {
        self.memory = memory;
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

    /// Sets the sub-agents a main agent may delegate to.
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

    /// Returns the Memory rights of the agent.
    #[must_use]
    pub const fn memory(&self) -> MemoryRights {
        self.memory
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

    /// Returns the sub-agents a main agent may delegate to.
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
    /// sub-agent declares delegable sub-agents, when a main agent declares an output schema, when a delegable sub-agent
    /// is the agent itself, or when the toolsets or the modules contain a duplicate.
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
        if self.kind == AgentKind::Sub && !self.delegable.is_empty() {
            return self.refuse("a sub-agent cannot declare delegable sub-agents");
        }
        if self.kind == AgentKind::Main && self.output_schema.is_some() {
            return self.refuse("a main agent cannot declare an output schema");
        }
        if self.delegable.contains(&self.id) {
            return self.refuse("a delegable sub-agent is the agent itself");
        }
        if has_duplicate(&self.toolsets) {
            return self.refuse("duplicate toolset");
        }
        if has_duplicate(&self.modules) {
            return self.refuse("duplicate module");
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{AgentDefinition, AgentKind, MemoryRights, SandboxLimits, fnv1a_64};
    use crate::agents::{AgentsError, SamplingParams};

    const PROMPT: &str = "You are the assistant of {{project}}.";
    const PROFILE: &str = "strix-qwen";

    fn main_agent() -> AgentDefinition {
        AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Main,
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
            .with_memory(
                MemoryRights::default()
                    .with_read(true)
                    .with_write_user(true)
                    .with_write_session(true),
            )
            .with_sandbox(SandboxLimits::new(Duration::from_secs(30), 10_000))
            .with_context_strategy("summarize".to_owned())
            .with_delegable(vec!["reviewer".to_owned()])
    }

    fn full_main_agent() -> AgentDefinition {
        full_definition("main", AgentKind::Main, PROMPT, PROFILE)
    }

    fn assert_invalid(definition: &AgentDefinition, case: &str) {
        let result = definition.validate();
        assert!(
            matches!(result, Err(AgentsError::InvalidDefinition { .. })),
            "{case} must fail with InvalidDefinition, got {result:?}"
        );
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
        let memory = definition.memory();
        assert!(
            !memory.read() && !memory.write_user() && !memory.write_project() && !memory.write_session(),
            "the Memory rights absent from the JSON grant nothing"
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
        let memory = MemoryRights::default().with_read(true).with_write_user(true);
        let cases = [
            (
                "identifier",
                full_definition("other", AgentKind::Main, PROMPT, PROFILE),
            ),
            ("kind", full_definition("main", AgentKind::Sub, PROMPT, PROFILE)),
            (
                "prompt",
                full_definition("main", AgentKind::Main, "Another prompt.", PROFILE),
            ),
            (
                "profile",
                full_definition("main", AgentKind::Main, PROMPT, "other-profile"),
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
                "memory read",
                full_main_agent().with_memory(memory.with_read(false).with_write_session(true)),
            ),
            (
                "memory write user",
                full_main_agent()
                    .with_memory(MemoryRights::default().with_read(true).with_write_session(true)),
            ),
            (
                "memory write project",
                full_main_agent().with_memory(
                    MemoryRights::default()
                        .with_read(true)
                        .with_write_user(true)
                        .with_write_project(true)
                        .with_write_session(true),
                ),
            ),
            (
                "memory write session",
                full_main_agent().with_memory(MemoryRights::default().with_read(true).with_write_user(true)),
            ),
            (
                "sandbox script timeout",
                full_main_agent().with_sandbox(SandboxLimits::new(Duration::from_secs(31), 10_000)),
            ),
            (
                "sandbox render cap",
                full_main_agent().with_sandbox(SandboxLimits::new(Duration::from_secs(30), 10_001)),
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
                full_definition("", AgentKind::Main, PROMPT, PROFILE),
            ),
            (
                "a blank identifier",
                full_definition("  ", AgentKind::Main, PROMPT, PROFILE),
            ),
            (
                "an empty prompt",
                full_definition("main", AgentKind::Main, "", PROFILE),
            ),
            (
                "a blank prompt",
                full_definition("main", AgentKind::Main, " \n\t ", PROFILE),
            ),
            (
                "an empty profile name",
                full_definition("main", AgentKind::Main, PROMPT, ""),
            ),
            (
                "a blank profile name",
                full_definition("main", AgentKind::Main, PROMPT, "   "),
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
}
