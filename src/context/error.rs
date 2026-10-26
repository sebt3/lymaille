//! Typed error of the context module.

/// Error of the context module. Texts carry sizes and causes only, never the content of a session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContextError {
    /// The system prompt and the native tool definitions do not fit at startup.
    #[error("system prompt of {system} tokens exceeds the startup limit of {limit} tokens")]
    SystemTooLarge {
        /// Size of the system prompt and tool definitions, in tokens.
        system: u64,
        /// Largest size accepted at startup, in tokens.
        limit: u64,
    },
    /// A user message does not fit next to the system prompt.
    #[error(
        "user message of {size} tokens does not fit: {available} tokens are available next to the system prompt"
    )]
    MessageTooLarge {
        /// Size of the user message, in tokens.
        size: u64,
        /// Room left for the message after the system prompt, in tokens.
        available: u64,
    },
    /// Compaction was requested while a tool call waits for its result.
    #[error("compaction is refused while a tool call waits for its result")]
    PendingToolCall,
    /// The summarizer failed. The message is produced by the summarizer.
    #[error("summarizer failed: {0}")]
    Summarizer(String),
    /// The summarizer returned an empty or blank summary.
    #[error("the summarizer returned an empty summary")]
    EmptySummary,
}

/// Result of the context module.
pub type ContextResult<T> = Result<T, ContextError>;

#[cfg(test)]
mod tests {
    use super::ContextError;

    #[test]
    fn display_names_the_cause() {
        let cases = [
            (
                ContextError::SystemTooLarge {
                    system: 6001,
                    limit: 6000,
                },
                ["system prompt", "6001", "6000"].as_slice(),
            ),
            (
                ContextError::MessageTooLarge {
                    size: 2101,
                    available: 2100,
                },
                ["user message", "2101", "2100"].as_slice(),
            ),
            (ContextError::PendingToolCall, ["tool call"].as_slice()),
            (
                ContextError::Summarizer("model unreachable".to_owned()),
                ["summarizer", "model unreachable"].as_slice(),
            ),
            (ContextError::EmptySummary, ["empty summary"].as_slice()),
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
