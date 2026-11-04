//! Replay mode of the reasoning, declared by a model profile, and selection of the reasoning sent back to the model
//! (`agent-sessions/context-window#FR-9`, `AC-5`).

use crate::session::{Message, Reasoning};

/// The replay mode is defined in the `session` domain, which persists it; it is re-exported here so that the selection of
/// the reasoning and the existing paths keep working.
pub use crate::session::ReplayMode;

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
                | Message::Boundary { .. }
                | Message::Discarded { .. } => None,
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
    use crate::session::{DiscardCause, History, Message, Reasoning};

    /// A history where a `Discarded` message follows the assistant message of the current turn, both carrying reasoning
    /// with an emitter.
    fn history_with_a_discarded_message() -> History {
        let mut history = History::new();
        history.push(Message::user("question"));
        history.push(Message::assistant(
            "answer",
            Some(Reasoning::new("provider-a", "kept")),
            Vec::new(),
        ));
        history.push(Message::discarded(
            DiscardCause::Empty,
            Some(Reasoning::new("provider-a", "never replayed")),
        ));
        history
    }

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
    fn intra_turn_without_user_message_replays_every_message() {
        let mut history = History::new();
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
        let replayed = replayed_reasoning(history.messages(), ReplayMode::IntraTurn);
        assert_eq!(
            texts(&replayed),
            vec!["r1", "r2"],
            "without any user message, every assistant message belongs to the current turn"
        );
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

    // Test de non-régression (../session/history.sdd #4, ../context/replay.sdd #5) : le message écarté garde une
    // place vide dans la liste parallèle, quel que soit le mode.
    #[test]
    fn discarded_message_has_an_empty_slot_in_none_mode() {
        let history = history_with_a_discarded_message();
        let replayed = replayed_reasoning(history.messages(), ReplayMode::None);
        assert_eq!(
            replayed.len(),
            history.messages().len(),
            "the result is parallel to the messages"
        );
        assert!(
            replayed.iter().all(Option::is_none),
            "mode none replays no reasoning, including that of a discarded message"
        );
        assert!(
            matches!(
                history.messages().last(),
                Some(Message::Discarded {
                    reasoning: Some(_),
                    ..
                })
            ),
            "the history must still hold the reasoning of the discarded message"
        );
    }

    #[test]
    fn discarded_message_has_an_empty_slot_in_intra_turn_mode() {
        let history = history_with_a_discarded_message();
        let replayed = replayed_reasoning(history.messages(), ReplayMode::IntraTurn);
        assert_eq!(
            replayed.len(),
            history.messages().len(),
            "the result is parallel to the messages"
        );
        assert_eq!(
            texts(&replayed),
            vec!["kept"],
            "in the current turn the assistant reasoning is replayed but the discarded message stays an empty slot"
        );
    }

    #[test]
    fn discarded_message_has_an_empty_slot_in_all_turns_mode() {
        let history = history_with_a_discarded_message();
        let replayed = replayed_reasoning(history.messages(), ReplayMode::AllTurns);
        assert_eq!(
            replayed.len(),
            history.messages().len(),
            "the result is parallel to the messages"
        );
        assert_eq!(
            texts(&replayed),
            vec!["kept"],
            "even replayed by every turn, the reasoning of a discarded message is never returned"
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
