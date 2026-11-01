//! Audit records of an executed script and of an MCP call: complete script text, result truncated to the rendering cap
//! with its original size, duration, error, then role, function, arguments, status and duration of the call. Every
//! field is masked of the secrets first (`agent-sessions/session#FR-8`, `agent-sessions#CON-3`,
//! `agent-sessions/session#AC-4`).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Redactor;

/// Inputs of a script record, grouped so that [`ScriptRecord::new`] keeps a short signature. Built with
/// [`ScriptInput::new`] and, when the script raised an error, [`ScriptInput::with_error`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ScriptInput<'input> {
    /// Name of the agent that ran the script.
    agent: &'input str,
    /// Complete script text.
    script: &'input str,
    /// Result of the script, before masking and truncation.
    result: &'input str,
    /// Duration of the script.
    duration: Duration,
    /// Error raised by the script, when there was one.
    error: Option<&'input str>,
}

impl<'input> ScriptInput<'input> {
    /// Inputs of a script run that raised no error.
    #[must_use]
    pub const fn new(
        agent: &'input str,
        script: &'input str,
        result: &'input str,
        duration: Duration,
    ) -> Self {
        Self {
            agent,
            script,
            result,
            duration,
            error: None,
        }
    }

    /// Sets the error raised by the script.
    #[must_use]
    pub const fn with_error(mut self, error: &'input str) -> Self {
        self.error = Some(error);
        self
    }
}

/// Audit record of one executed script. Every text is masked; the result is also truncated to the rendering cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScriptRecord {
    /// Name of the agent that ran the script.
    pub agent: String,
    /// Complete script text, masked and never truncated.
    pub script: String,
    /// Result, masked then truncated to the rendering cap in characters.
    pub result: String,
    /// Number of characters of the masked result before truncation.
    pub original_len: usize,
    /// Whether the result lost characters to the cap.
    pub truncated: bool,
    /// Duration of the script in milliseconds.
    pub duration_ms: u64,
    /// Error raised by the script, masked, when there was one.
    pub error: Option<String>,
}

impl ScriptRecord {
    /// Builds the record of a script run, masking every field and truncating the result to `render_cap` characters.
    #[must_use]
    pub fn new(redactor: &Redactor, render_cap: usize, input: ScriptInput<'_>) -> Self {
        // Masking comes first, so that a secret cut by the cap leaves no fragment behind.
        let masked_result = redactor.redact(input.result);
        let original_len = masked_result.chars().count();
        Self {
            agent: input.agent.to_owned(),
            script: redactor.redact(input.script),
            result: masked_result.chars().take(render_cap).collect(),
            original_len,
            truncated: original_len > render_cap,
            duration_ms: duration_millis(input.duration),
            error: input.error.map(|text| redactor.redact(text)),
        }
    }
}

/// Inputs of an MCP call record, grouped so that [`McpRecord::new`] keeps a short signature.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct McpInput<'input> {
    /// Role of the MCP server in the session.
    role: &'input str,
    /// Name of the called MCP function.
    function: &'input str,
    /// Arguments of the call.
    arguments: &'input Value,
    /// Status of the call.
    status: &'input str,
    /// Duration of the call.
    duration: Duration,
}

impl<'input> McpInput<'input> {
    /// Inputs of an MCP call: role, function, arguments, status and duration.
    #[must_use]
    pub const fn new(
        role: &'input str,
        function: &'input str,
        arguments: &'input Value,
        status: &'input str,
        duration: Duration,
    ) -> Self {
        Self {
            role,
            function,
            arguments,
            status,
            duration,
        }
    }
}

/// Audit record of one MCP call. Role, function, arguments and status are masked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct McpRecord {
    /// Role of the MCP server in the session.
    pub role: String,
    /// Name of the called MCP function.
    pub function: String,
    /// Arguments of the call, masked in every string and key.
    pub arguments: Value,
    /// Status of the call, masked.
    pub status: String,
    /// Duration of the call in milliseconds.
    pub duration_ms: u64,
}

impl McpRecord {
    /// Builds the record of an MCP call, masking its texts and arguments.
    #[must_use]
    pub fn new(redactor: &Redactor, input: McpInput<'_>) -> Self {
        Self {
            role: redactor.redact(input.role),
            function: redactor.redact(input.function),
            arguments: redactor.redact_json(input.arguments),
            status: redactor.redact(input.status),
            duration_ms: duration_millis(input.duration),
        }
    }
}

/// Whole milliseconds of `duration`, saturating at the maximum of `u64`.
fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{McpInput, McpRecord, ScriptInput, ScriptRecord};
    use crate::session::Redactor;

    fn script_record(
        redactor: &Redactor,
        cap: usize,
        script: &str,
        result: &str,
        error: Option<&str>,
    ) -> ScriptRecord {
        let input = ScriptInput::new("agent-a", script, result, Duration::from_millis(12));
        let input = error.map_or(input, |text| input.with_error(text));
        ScriptRecord::new(redactor, cap, input)
    }

    #[test]
    fn result_truncated_at_cap_with_original_size() {
        let redactor = Redactor::new();
        let result = "r".repeat(50);
        let record = script_record(&redactor, 20, "print(1)", &result, None);
        assert_eq!(
            record.result.chars().count(),
            20,
            "a result of 50 characters with a cap of 20 must be cut to 20 characters"
        );
        assert_eq!(
            record.original_len, 50,
            "original_len must keep the size of the result before truncation"
        );
        assert!(
            record.truncated,
            "a result that lost characters must be flagged as truncated"
        );
    }

    #[test]
    fn result_under_the_cap_is_untouched() {
        let redactor = Redactor::new();
        let record = script_record(&redactor, 20, "print(1)", "0123456789", None);
        assert!(
            !record.truncated,
            "a result under the cap must not be flagged as truncated"
        );
        assert_eq!(
            record.result, "0123456789",
            "a result under the cap must come back unchanged"
        );
        assert_eq!(
            record.original_len, 10,
            "original_len must be the size of the result even when nothing was cut"
        );
        assert_eq!(
            record.duration_ms, 12,
            "the duration must be recorded in whole milliseconds"
        );
    }

    #[test]
    fn script_and_result_never_leak_a_secret() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
        let record = script_record(
            &redactor,
            100,
            "call(\"tok-123\")",
            "answer tok-123",
            Some("failed with tok-123"),
        );
        assert!(
            !record.script.contains("tok-123"),
            "the script text must be masked, got {}",
            record.script
        );
        assert!(
            !record.result.contains("tok-123"),
            "the result must be masked, got {}",
            record.result
        );
        assert!(
            !record.error.unwrap_or_default().contains("tok-123"),
            "the error must be masked"
        );
    }

    #[test]
    fn truncation_does_not_split_a_multi_byte_character() {
        let redactor = Redactor::new();
        let result = "\u{e9}".repeat(20);
        let record = script_record(&redactor, 7, "print(1)", &result, None);
        assert_eq!(
            record.result,
            "\u{e9}".repeat(7),
            "truncation must count characters, never cut inside a multi-byte character"
        );
    }

    #[test]
    fn mcp_arguments_are_masked() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
        let arguments = json!({"auth": "tok-123"});
        let input = McpInput::new("monitoring", "query", &arguments, "ok", Duration::from_millis(3));
        let record = McpRecord::new(&redactor, input);
        assert_eq!(
            record.arguments,
            json!({"auth": "*redacted secret*"}),
            "the secret must be masked in the arguments of an MCP call"
        );
    }

    #[test]
    fn redaction_happens_before_truncation() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
        let record = script_record(&redactor, 8, "print(1)", "ab tok-123", None);
        assert_eq!(
            record.result, "ab *reda",
            "the secret must be masked before the cap cuts the text, so no fragment of it survives"
        );
        assert_eq!(record.original_len, 20, "original_len counts the masked result");
        assert!(
            record.truncated,
            "the masked result of twenty characters is cut to eight"
        );
    }

    #[test]
    fn zero_cap_truncates_the_whole_result_and_script_is_never_cut() {
        let redactor = Redactor::new();
        let script = "a long script that stays whole";
        let record = script_record(&redactor, 0, script, "some result", None);
        assert_eq!(record.result, "", "a cap of zero must truncate the whole result");
        assert!(
            record.truncated,
            "a result cut to nothing must be flagged as truncated"
        );
        assert_eq!(record.script, script, "the script text must never be truncated");
    }

    #[test]
    fn records_serialize_without_secret_fields() {
        let mut redactor = Redactor::new();
        redactor
            .add_secret("tok-123")
            .expect("a seven-character secret is accepted");
        let record = script_record(&redactor, 100, "s", "tok-123", None);
        let json = serde_json::to_string(&record).expect("a script record serializes");
        assert!(
            !json.contains("tok-123"),
            "the serialized record must not carry a secret, got {json}"
        );
        let back: ScriptRecord = serde_json::from_str(&json).expect("a script record deserializes");
        assert_eq!(back, record, "the record must deserialize to the same value");
    }
}
