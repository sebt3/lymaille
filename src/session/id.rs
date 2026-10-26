//! Typed identifiers of sessions, turns and runs, so that a label, a signal or a handle cannot confuse two kinds of
//! identifier.

use std::fmt;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// Message of the deserialization error raised for an empty identifier.
const EMPTY_IDENTIFIER: &str = "an identifier must not be empty";

/// Identifier of a session, provided by the host. Never empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct SessionId {
    value: String,
}

impl SessionId {
    /// Builds a session identifier, or `None` when the text is empty.
    #[must_use]
    pub fn new<S: Into<String>>(value: S) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()).then_some(Self { value })
    }

    /// Returns the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| <D::Error as de::Error>::custom(EMPTY_IDENTIFIER))
    }
}

/// Identifier of a run, provided by the host. Never empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct RunId {
    value: String,
}

impl RunId {
    /// Builds a run identifier, or `None` when the text is empty.
    #[must_use]
    pub fn new<S: Into<String>>(value: S) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()).then_some(Self { value })
    }

    /// Returns the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for RunId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| <D::Error as de::Error>::custom(EMPTY_IDENTIFIER))
    }
}

/// Rank of a turn in its session, starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct TurnId {
    value: u64,
}

impl TurnId {
    /// Builds a turn identifier from its rank.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self { value }
    }

    /// Returns the rank of the turn.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.value
    }

    /// Returns the following turn, saturating at the maximum value.
    #[must_use]
    pub const fn next(self) -> Self {
        Self {
            value: self.value.saturating_add(1),
        }
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

#[cfg(test)]
mod tests {
    use super::{RunId, SessionId, TurnId};

    #[test]
    fn empty_identifiers_are_refused() {
        assert!(
            SessionId::new("").is_none(),
            "an empty session identifier must not be produced"
        );
        assert!(
            RunId::new(String::new()).is_none(),
            "an empty run identifier must not be produced"
        );
    }

    #[test]
    fn identifiers_serialize_bare() {
        let session = SessionId::new("abc").expect("a non-empty text makes a session identifier");
        let session_json = serde_json::to_string(&session).expect("a session identifier serializes");
        assert_eq!(
            session_json, "\"abc\"",
            "a session identifier must serialize as a bare string"
        );

        let turn_json = serde_json::to_string(&TurnId::new(3)).expect("a turn identifier serializes");
        assert_eq!(
            turn_json, "3",
            "a turn identifier must serialize as a bare number"
        );
    }

    #[test]
    fn deserialization_refuses_the_empty_identifier() {
        let session: SessionId =
            serde_json::from_str("\"abc\"").expect("a non-empty text deserializes as a session identifier");
        assert_eq!(
            session.as_str(),
            "abc",
            "a session identifier must deserialize to the text it was written as"
        );
        let run: RunId =
            serde_json::from_str("\"abc\"").expect("a non-empty text deserializes as a run identifier");
        assert_eq!(
            run.as_str(),
            "abc",
            "a run identifier must deserialize to the text it was written as"
        );
        assert!(
            serde_json::from_str::<SessionId>("\"\"").is_err(),
            "an empty text must not deserialize as a session identifier"
        );
        assert!(
            serde_json::from_str::<RunId>("\"\"").is_err(),
            "an empty text must not deserialize as a run identifier"
        );
        let turn: TurnId =
            serde_json::from_str("3").expect("a bare number deserializes as a turn identifier");
        assert_eq!(
            turn,
            TurnId::new(3),
            "a turn identifier must deserialize from a bare number"
        );
    }

    #[test]
    fn turn_ids_are_successive() {
        assert_eq!(
            TurnId::new(1).next(),
            TurnId::new(2),
            "the turn after 1 must be 2"
        );
        assert_eq!(
            TurnId::new(u64::MAX).next(),
            TurnId::new(u64::MAX),
            "the turn after the maximum must stay the maximum"
        );
    }

    #[test]
    fn identifiers_expose_their_text_and_display_it_bare() {
        let session = SessionId::new("s-1").expect("a non-empty text makes a session identifier");
        let run = RunId::new("r-1").expect("a non-empty text makes a run identifier");
        assert_eq!(session.as_str(), "s-1", "as_str must return the identifier text");
        assert_eq!(run.as_str(), "r-1", "as_str must return the identifier text");
        assert_eq!(
            session.to_string(),
            "s-1",
            "a session identifier must display its bare text"
        );
        assert_eq!(
            run.to_string(),
            "r-1",
            "a run identifier must display its bare text"
        );
    }

    #[test]
    fn turn_id_returns_its_rank_and_displays_it() {
        assert_eq!(TurnId::new(5).get(), 5, "get must return the rank given to new");
        assert_eq!(
            TurnId::new(5).to_string(),
            "5",
            "a turn identifier must display its bare rank"
        );
    }
}
