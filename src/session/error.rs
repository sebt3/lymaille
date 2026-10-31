//! Typed error of the session module.

use super::{FormatVersion, SessionId};

/// Error of the session module. Texts carry identifiers and versions only, never the content of a session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The host storage failed. The message is produced by the caller.
    #[error("session store failed: {0}")]
    Store(String),
    /// State could not be serialized or deserialized. The message is produced by the caller.
    #[error("session serialization failed: {0}")]
    Serialization(String),
    /// The saved major format version differs from the current one. Both versions are carried.
    #[error("incompatible session format: saved version {saved}, current version {current}")]
    IncompatibleFormat {
        /// Format version recorded in the saved state.
        saved: FormatVersion,
        /// Format version this library reads and writes.
        current: FormatVersion,
    },
    /// A turn is already active for the session.
    #[error("a turn is already active for session {0}")]
    TurnActive(SessionId),
    /// A user message cannot be inserted between a tool call and its result.
    #[error("a user message cannot be inserted between a tool call and its result")]
    PendingToolCall,
    /// An internal lock was poisoned.
    #[error("an internal lock was poisoned")]
    Poisoned,
    /// A secret shorter than six characters was offered to the redactor. It is not retained, and the text carries no part of it.
    #[error("a secret must be at least 6 characters long")]
    SecretTooShort,
    /// An inspection target was set on a session that is not of the configuration nature. Carries the session identifier.
    #[error("an inspection target can only be set on a configuration session, not on session {0}")]
    NotConfigSession(SessionId),
    /// A parent session was set on a session that is not of the sub-agent nature. Carries the session identifier.
    #[error("a parent session can only be set on a sub-agent session, not on session {0}")]
    NotSubSession(SessionId),
    /// A native tool cannot be registered: its name is `rhai_pad`, or a native tool of this name is already registered.
    /// Carries the name of the refused tool, which the host chose, never a value of the session.
    #[error("invalid native tool `{name}`: its name is reserved or already registered")]
    InvalidNativeTool {
        /// Name of the refused tool.
        name: String,
    },
    /// No state is saved under the identifier. Carries the identifier, which the caller asked for.
    #[error("no saved session `{id}`")]
    UnknownSession {
        /// Identifier of the session that was asked for.
        id: SessionId,
    },
}

/// Result of the session module.
pub type SessionResult<T> = Result<T, SessionError>;

#[cfg(test)]
mod tests {
    use super::{FormatVersion, SessionError, SessionId};

    #[test]
    fn display_texts_are_stable() {
        let session = SessionId::new("s-1").expect("a non-empty text makes a session identifier");
        let cases = [
            (
                SessionError::Store("disk full".to_owned()),
                "session store failed: disk full",
            ),
            (
                SessionError::Serialization("invalid JSON".to_owned()),
                "session serialization failed: invalid JSON",
            ),
            (
                SessionError::IncompatibleFormat {
                    saved: FormatVersion::new(2, 0),
                    current: FormatVersion::new(1, 0),
                },
                "incompatible session format: saved version 2.0, current version 1.0",
            ),
            (
                SessionError::TurnActive(session.clone()),
                "a turn is already active for session s-1",
            ),
            (
                SessionError::PendingToolCall,
                "a user message cannot be inserted between a tool call and its result",
            ),
            (SessionError::Poisoned, "an internal lock was poisoned"),
            (
                SessionError::SecretTooShort,
                "a secret must be at least 6 characters long",
            ),
            (
                SessionError::UnknownSession { id: session.clone() },
                "no saved session `s-1`",
            ),
            (
                SessionError::NotSubSession(session.clone()),
                "a parent session can only be set on a sub-agent session, not on session s-1",
            ),
            (
                SessionError::NotConfigSession(session),
                "an inspection target can only be set on a configuration session, not on session s-1",
            ),
            (
                SessionError::InvalidNativeTool {
                    name: "rhai_pad".to_owned(),
                },
                "invalid native tool `rhai_pad`: its name is reserved or already registered",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(
                error.to_string(),
                expected,
                "each variant must display its documented wording"
            );
        }
    }
}
