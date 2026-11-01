//! Replay mode of the reasoning, declared by a model profile. It lives in the `session` domain, because a segment snapshot
//! persists it; `context` re-exports it (`agent-sessions/context-window#FR-9`). Its text is the persisted form, so it must
//! not change: `none`, `intra_turn` or `all_turns`.

use serde::{Deserialize, Serialize};

/// How the reasoning of the assistant is sent back to the model. `AllTurns` corresponds to `preserve_thinking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReplayMode {
    /// No reasoning is sent back. It stays in the history for the audit.
    None,
    /// The reasoning of the assistant messages of the current turn is sent back.
    IntraTurn,
    /// The reasoning of every assistant message is sent back.
    AllTurns,
}

impl ReplayMode {
    /// Returns the text of the mode: `none`, `intra_turn` or `all_turns`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::IntraTurn => "intra_turn",
            Self::AllTurns => "all_turns",
        }
    }

    /// Returns the mode named by `text`, or nothing for any other text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [Self::None, Self::IntraTurn, Self::AllTurns]
            .into_iter()
            .find(|mode| mode.as_str() == text)
    }
}
