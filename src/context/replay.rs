//! Replay mode of the reasoning, declared by a model profile, and selection of the reasoning sent back to the model
//! (`agent-sessions/context-window#FR-9`, `AC-5`).

use serde::{Deserialize, Serialize};

use crate::session::{Message, Reasoning};

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

/// Returns, for each message of `messages` and in the same order, the reasoning to send back to the model, or nothing.
/// The history itself is never modified.
///
/// Reasoning is sent back only when it has an emitter. Under [`ReplayMode::IntraTurn`], only the assistant messages
/// after the last user message are concerned; when there is no user message, every message is in the current turn.
#[must_use]
pub fn replayed_reasoning(messages: &[Message], mode: ReplayMode) -> Vec<Option<&Reasoning>> {
    let turn_start = messages
        .iter()
        .rposition(|message| matches!(message, Message::User { .. }))
        .map_or(0, |last_user| last_user.saturating_add(1));
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let reasoning = match message {
                Message::Assistant { reasoning, .. } => reasoning.as_ref(),
                Message::System { .. }
                | Message::User { .. }
                | Message::ToolResult { .. }
                | Message::Boundary { .. } => None,
            };
            let in_scope = match mode {
                ReplayMode::None => false,
                ReplayMode::IntraTurn => index >= turn_start,
                ReplayMode::AllTurns => true,
            };
            reasoning.filter(|reasoning| in_scope && !reasoning.emitter().is_empty())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ReplayMode, replayed_reasoning};
    use crate::session::{History, Message, Reasoning};

    fn texts<'text>(replayed: &[Option<&'text Reasoning>]) -> Vec<&'text str> {
        replayed
            .iter()
            .flatten()
            .map(|reasoning| reasoning.text())
            .collect()
    }

    #[test]
    fn none_stores_but_does_not_replay() {
        let mut history = History::new();
        history.push(Message::user("question"));
        history.push(Message::assistant(
            "answer",
            Some(Reasoning::new("provider-a", "thinking")),
            Vec::new(),
        ));
        let replayed = replayed_reasoning(history.messages(), ReplayMode::None);
        assert!(
            replayed.iter().all(Option::is_none),
            "no reasoning may be returned in mode none"
        );
        assert!(
            matches!(
                history.messages().get(1),
                Some(Message::Assistant {
                    reasoning: Some(_),
                    ..
                })
            ),
            "the history must still hold the reasoning"
        );
    }

    #[test]
    fn all_turns_replays_everything() {
        let mut history = History::new();
        history.push(Message::user("first"));
        history.push(Message::assistant(
            "a1",
            Some(Reasoning::new("provider-a", "r1")),
            Vec::new(),
        ));
        history.push(Message::assistant(
            "a2",
            Some(Reasoning::new("provider-a", "r2")),
            Vec::new(),
        ));
        history.push(Message::user("second"));
        history.push(Message::assistant(
            "a3",
            Some(Reasoning::new("provider-a", "r3")),
            Vec::new(),
        ));
        let replayed = replayed_reasoning(history.messages(), ReplayMode::AllTurns);
        assert_eq!(
            replayed.len(),
            history.messages().len(),
            "the result is parallel to the messages"
        );
        assert_eq!(texts(&replayed), vec!["r1", "r2", "r3"]);
    }

    #[test]
    fn intra_turn_replays_the_current_turn_only() {
        let mut history = History::new();
        history.push(Message::user("first"));
        history.push(Message::assistant(
            "before",
            Some(Reasoning::new("provider-a", "before")),
            Vec::new(),
        ));
        history.push(Message::user("second"));
        history.push(Message::assistant(
            "after",
            Some(Reasoning::new("provider-a", "after")),
            Vec::new(),
        ));
        let replayed = replayed_reasoning(history.messages(), ReplayMode::IntraTurn);
        assert_eq!(texts(&replayed), vec!["after"]);
    }

    #[test]
    fn reasoning_without_emitter_is_not_replayed() {
        let mut history = History::new();
        history.push(Message::user("question"));
        history.push(Message::assistant(
            "answer",
            Some(Reasoning::new("", "orphan")),
            Vec::new(),
        ));
        let replayed = replayed_reasoning(history.messages(), ReplayMode::AllTurns);
        assert!(
            replayed.iter().all(Option::is_none),
            "reasoning without an emitter is never replayed"
        );
    }

    #[test]
    fn mode_text_round_trips() {
        for mode in [ReplayMode::None, ReplayMode::IntraTurn, ReplayMode::AllTurns] {
            assert_eq!(
                ReplayMode::parse(mode.as_str()),
                Some(mode),
                "parse must invert as_str for {mode:?}"
            );
            let json = serde_json::to_string(&mode).expect("a mode serializes");
            assert_eq!(
                json,
                format!("\"{}\"", mode.as_str()),
                "the JSON form is the quoted text"
            );
            let back: ReplayMode = serde_json::from_str(&json).expect("a quoted text deserializes");
            assert_eq!(back, mode);
        }
        assert_eq!(ReplayMode::parse("other"), None, "any other text is refused");
        assert!(
            serde_json::from_str::<ReplayMode>("\"other\"").is_err(),
            "any other JSON text is refused"
        );
    }
}
