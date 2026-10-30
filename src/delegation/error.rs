//! Typed error of the `delegation` domain.

/// Error of the `delegation` domain. Texts carry names, identifiers and fixed reasons, never the content of an event,
/// a task, a result, nor a secret (`agent-sessions/delegation`, `Must`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DelegationError {
    /// A sub-agent is not in the definition of the main agent.
    #[error("sub-agent `{name}` is not declared by the main agent")]
    UnknownSubagent {
        /// Name of the sub-agent that was asked for.
        name: String,
    },
    /// A run identifier is not known to the registry.
    #[error("run `{id}` is unknown")]
    UnknownRun {
        /// Identifier of the unknown run.
        id: String,
    },
    /// A watcher is refused. The reason is a fixed phrase, never the content of an event.
    #[error("invalid watcher `{id}`: {reason}")]
    InvalidWatcher {
        /// Identifier of the refused watcher, possibly empty.
        id: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
    },
    /// A timer is refused. The reason is a fixed phrase.
    #[error("invalid timer `{id}`: {reason}")]
    InvalidTimer {
        /// Identifier of the refused timer, possibly empty.
        id: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
    },
    /// The result schema of a sub-agent is not a usable JSON schema.
    #[error("invalid result schema of sub-agent `{subagent}`: {reason}")]
    InvalidSchema {
        /// Name of the sub-agent whose schema is refused.
        subagent: String,
        /// Fixed reason of the refusal.
        reason: &'static str,
    },
    /// The host failed to launch a sub-agent. The text is fixed and carries no content.
    #[error("the host failed to launch the sub-agent")]
    Launch,
    /// An internal lock was poisoned.
    #[error("an internal lock was poisoned")]
    Poisoned,
}

/// Result of the `delegation` domain.
pub type DelegationResult<T> = Result<T, DelegationError>;

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::DelegationError;

    fn assert_error_send_sync<E: Error + Send + Sync + 'static>() {}

    // Scenario: les messages ne portent pas de contenu
    #[test]
    fn messages_carry_no_content() {
        // The event that the predicate read holds a secret. The error is built from the watcher identifier and a fixed
        // reason only, so the secret cannot reach the text.
        let event_secret = "tok-live-4f2a9c";
        let error = DelegationError::InvalidWatcher {
            id: "grafana-alerts".to_owned(),
            reason: "the predicate does not compile",
        };
        let text = error.to_string();
        assert!(
            text.contains("grafana-alerts"),
            "the message `{text}` must name the watcher"
        );
        assert!(
            !text.contains(event_secret),
            "the message `{text}` must not carry the event content"
        );
    }

    #[test]
    fn each_variant_names_its_object_and_cause() {
        let cases = [
            (
                DelegationError::UnknownSubagent {
                    name: "reviewer".to_owned(),
                },
                ["reviewer", "not declared"].as_slice(),
            ),
            (
                DelegationError::UnknownRun {
                    id: "run-7".to_owned(),
                },
                ["run-7", "unknown"].as_slice(),
            ),
            (
                DelegationError::InvalidWatcher {
                    id: "mcp-tools".to_owned(),
                    reason: "unknown source",
                },
                ["mcp-tools", "unknown source"].as_slice(),
            ),
            (
                DelegationError::InvalidTimer {
                    id: "daily".to_owned(),
                    reason: "duration is zero",
                },
                ["daily", "duration is zero"].as_slice(),
            ),
            (
                DelegationError::InvalidSchema {
                    subagent: "extractor".to_owned(),
                    reason: "not a JSON schema",
                },
                ["extractor", "not a JSON schema"].as_slice(),
            ),
            (DelegationError::Launch, ["launch"].as_slice()),
            (DelegationError::Poisoned, ["poisoned"].as_slice()),
        ];
        for (error, expected) in cases {
            let text = error.to_string();
            for fragment in expected {
                assert!(
                    text.contains(fragment),
                    "the text `{text}` must carry `{fragment}`"
                );
            }
        }
    }

    #[test]
    fn error_is_send_sync_and_std_error() {
        assert_error_send_sync::<DelegationError>();
    }
}
