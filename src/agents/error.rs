//! Typed error of the agents module.

/// Error of the agents module. Texts carry names, identifiers and fixed reasons, never the content of a prompt.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentsError {
    /// A named model profile is not found.
    #[error("model profile `{name}` is unknown")]
    UnknownProfile {
        /// Name of the profile that was looked up.
        name: String,
    },
    /// A named provider is not found.
    #[error("provider `{name}` is unknown")]
    UnknownProvider {
        /// Name of the provider that was looked up.
        name: String,
    },
    /// Neither the detection nor the profile gives the context size.
    #[error("context size of profile `{name}` is unknown: neither the override nor the detection gives it")]
    ContextSizeUnknown {
        /// Name of the profile without a context size.
        name: String,
    },
    /// An agent definition is inconsistent.
    #[error("invalid agent definition `{id}`: {reason}")]
    InvalidDefinition {
        /// Identifier of the refused definition.
        id: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
    },
    /// The host storage failed. The message is produced by the host.
    #[error("agent store failed: {0}")]
    Store(String),
}

/// Result of the agents module.
pub type AgentsResult<T> = Result<T, AgentsError>;

#[cfg(test)]
mod tests {
    use super::AgentsError;

    #[test]
    fn display_names_the_cause_and_the_object() {
        let cases = [
            (
                AgentsError::UnknownProfile {
                    name: "strix-qwen".to_owned(),
                },
                ["unknown", "strix-qwen"].as_slice(),
            ),
            (
                AgentsError::UnknownProvider {
                    name: "vllm-strix".to_owned(),
                },
                ["unknown", "vllm-strix"].as_slice(),
            ),
            (
                AgentsError::ContextSizeUnknown {
                    name: "strix-qwen".to_owned(),
                },
                ["context size", "strix-qwen"].as_slice(),
            ),
            (
                AgentsError::InvalidDefinition {
                    id: "reviewer".to_owned(),
                    reason: "a sub-agent cannot declare delegable sub-agents",
                },
                ["invalid", "reviewer", "sub-agent cannot"].as_slice(),
            ),
            (
                AgentsError::Store("disk full".to_owned()),
                ["store failed", "disk full"].as_slice(),
            ),
        ];
        for (error, expected) in cases {
            let text = error.to_string();
            for fragment in expected {
                assert!(
                    text.contains(fragment),
                    "the text `{text}` must name the cause and carry `{fragment}`"
                );
            }
        }
    }
}
