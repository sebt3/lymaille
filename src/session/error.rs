//! Typed error of the session module.

use super::SessionId;

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
    #[error(
        "incompatible session format: saved version {saved_major}.{saved_minor}, current version {current_major}.{current_minor}"
    )]
    IncompatibleFormat {
        /// Major version recorded in the saved state.
        saved_major: u32,
        /// Minor version recorded in the saved state.
        saved_minor: u32,
        /// Major version this library reads and writes.
        current_major: u32,
        /// Minor version this library reads and writes.
        current_minor: u32,
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
}

/// Result of the session module.
pub type SessionResult<T> = Result<T, SessionError>;

#[cfg(test)]
mod tests {
    use super::{SessionError, SessionId};

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
                    saved_major: 2,
                    saved_minor: 0,
                    current_major: 1,
                    current_minor: 0,
                },
                "incompatible session format: saved version 2.0, current version 1.0",
            ),
            (
                SessionError::TurnActive(session),
                "a turn is already active for session s-1",
            ),
            (
                SessionError::PendingToolCall,
                "a user message cannot be inserted between a tool call and its result",
            ),
            (SessionError::Poisoned, "an internal lock was poisoned"),
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
