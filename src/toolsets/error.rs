//! Typed error of the toolsets module. Texts carry names, fixed reasons and counts, never a parameter value or the
//! reference of a secret.

/// Error of the toolsets module.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolsetsError {
    /// A context type is inconsistent: empty name, parameter schema that is not a JSON Schema object, role declared
    /// twice, or an inclusion that is missing, too old or cyclic.
    #[error("invalid context type `{name}`: {reason}")]
    InvalidContextType {
        /// Name of the refused context type, or of the type whose inclusion failed.
        name: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
    },
    /// A toolset is inconsistent: empty name, missing or uncompilable module, unreadable version, duplicated requirement,
    /// or a malformed version requirement.
    #[error("invalid toolset `{name}`: {reason}{}", detail_note(.detail))]
    InvalidToolset {
        /// Name of the refused toolset, or the offending requirement text when no name can be read, or `<source>` for a
        /// module source that has no name yet.
        name: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
        /// Detail of the refusal, empty when there is none: the position and the kind of a compilation error, or the
        /// offending version text. Never a fragment of a source text.
        detail: String,
    },
    /// A skill is inconsistent or unknown: name or description out of bounds, an annex document declared twice, or a
    /// skill or annex document requested by a name that does not exist.
    #[error("invalid skill `{name}`: {reason}{}", known_note(.known))]
    InvalidSkill {
        /// Name that was refused: the skill, or the annex document, as declared or requested.
        name: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
        /// Names that do exist, in place of the refused one (for an unknown name); empty when there is none.
        known: Vec<String>,
    },
    /// A toolset context is inconsistent: empty name, empty role or parameter name, or a value that looks like a secret.
    /// The name is the one of the context, role or parameter concerned; the value is never carried.
    #[error("invalid toolset context `{name}`: {reason}")]
    InvalidContext {
        /// Name of the context, role or parameter concerned.
        name: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
    },
    /// A skill body or an annex document exceeds the render cap.
    #[error("skill `{name}` exceeds the render cap of {cap_chars} characters")]
    SkillTooLarge {
        /// Name of the refused skill or annex document.
        name: String,
        /// Render cap that was exceeded, in characters.
        cap_chars: u64,
    },
    /// A Handlebars template does not compile, or references a variable that is absent.
    #[error("template `{name}` cannot be rendered: {reason}")]
    Template {
        /// Name of the template that failed.
        name: String,
        /// Reason produced by the template engine.
        reason: String,
    },
    /// The toolset context does not satisfy the context types required by the toolsets. Carries every gap found.
    #[error("incompatible toolset context: {}", .issues.join("; "))]
    Incompatible {
        /// Gaps between the requirements and the context, one text per gap.
        issues: Vec<String>,
    },
    /// An agent definition violates a rule of the toolset-creation module. The message names the rule.
    #[error("forbidden by an agent definition rule: {rule}")]
    Forbidden {
        /// Fixed text of the violated rule.
        rule: &'static str,
    },
    /// The host storage failed. The message is produced by the host.
    #[error("toolsets store failed: {0}")]
    Store(String),
    /// An internal lock of an in-memory store was poisoned.
    #[error("an internal lock was poisoned")]
    Poisoned,
}

/// Result of the toolsets module.
pub type ToolsetsResult<T> = Result<T, ToolsetsError>;

/// The note that follows a reason in a message: the detail in parentheses, or nothing when the detail is empty.
fn detail_note(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    }
}

/// The note that lists the names that exist, after the reason of an unknown name: nothing when there is none.
fn known_note(known: &[String]) -> String {
    if known.is_empty() {
        String::new()
    } else {
        format!(" (known: {})", known.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::ToolsetsError;

    // Scenario: les messages cachent les valeurs
    #[test]
    fn messages_hide_values() {
        let error = ToolsetsError::InvalidContext {
            name: "api_url".to_owned(),
            reason: "value looks like a secret; the context holds references only",
        };
        let text = error.to_string();
        assert!(
            text.contains("api_url"),
            "the message must name the parameter: {text}"
        );
        assert!(
            !text.contains("sk-abcdef"),
            "the message must never carry the value: {text}"
        );
    }

    #[test]
    fn invalid_skill_message_lists_known_names_after_the_reason() {
        let error = ToolsetsError::InvalidSkill {
            name: "nope".to_owned(),
            reason: "unknown skill name",
            known: vec!["deploy".to_owned(), "rhai-pad".to_owned()],
        };
        assert_eq!(
            error.to_string(),
            "invalid skill `nope`: unknown skill name (known: deploy, rhai-pad)"
        );
    }

    #[test]
    fn invalid_skill_message_has_no_note_without_known_names() {
        let error = ToolsetsError::InvalidSkill {
            name: "Grafana".to_owned(),
            reason: "unknown skill name",
            known: Vec::new(),
        };
        assert_eq!(error.to_string(), "invalid skill `Grafana`: unknown skill name");
    }

    #[test]
    fn value_free_variants_carry_no_value() {
        let incompatible = ToolsetsError::Incompatible {
            issues: vec!["missing role `k8s`".to_owned()],
        };
        let text = incompatible.to_string();
        assert!(text.contains("missing role `k8s`"), "the gaps are listed: {text}");
        let too_large = ToolsetsError::SkillTooLarge {
            name: "rhai_pad".to_owned(),
            cap_chars: 4000,
        };
        assert!(
            too_large.to_string().contains("4000"),
            "the cap is named in characters"
        );
    }
}
