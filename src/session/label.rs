//! Labels added by the host to a session, a turn or a run, during or after the session. Entries are only added, never
//! changed nor removed (`agent-sessions/session#FR-15`, `agent-sessions#DEC-13`, `agent-sessions/session#AC-10`).

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::title::single_line;
use super::{Redactor, RunId, TurnId};
use crate::clock::{Clock, Timestamp};

/// Message of the deserialization error raised for an empty label text.
const EMPTY_TEXT: &str = "a label text must not be empty";

/// What a label is attached to: the session as a whole, one turn, or one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LabelTarget {
    /// The session as a whole.
    Session,
    /// One turn of the session, by its rank.
    Turn(TurnId),
    /// One run, by its identifier.
    Run(RunId),
}

/// Who added a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LabelAuthor {
    /// A person, through the host.
    Human,
    /// The host or an automatic process.
    Automatic,
}

/// One label: a free text, its target, its author and the date set by the injected clock. Its text is never empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Label {
    text: String,
    target: LabelTarget,
    author: LabelAuthor,
    date: Timestamp,
}

/// Wire form of a label, checked before it becomes a [`Label`].
#[derive(Deserialize)]
struct LabelRecord {
    text: String,
    target: LabelTarget,
    author: LabelAuthor,
    date: Timestamp,
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = LabelRecord::deserialize(deserializer)?;
        if record.text.is_empty() {
            return Err(<D::Error as de::Error>::custom(EMPTY_TEXT));
        }
        Ok(Self {
            text: record.text,
            target: record.target,
            author: record.author,
            date: record.date,
        })
    }
}

impl Label {
    /// Returns the label text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the target of the label.
    #[must_use]
    pub const fn target(&self) -> &LabelTarget {
        &self.target
    }

    /// Returns the author of the label.
    #[must_use]
    pub const fn author(&self) -> LabelAuthor {
        self.author
    }

    /// Returns the date the label was added, read from the injected clock.
    #[must_use]
    pub const fn date(&self) -> Timestamp {
        self.date
    }
}

/// Append-only list of labels of one session, in the order they were added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct Labels {
    entries: Vec<Label>,
}

impl<'list> IntoIterator for &'list Labels {
    type Item = &'list Label;
    type IntoIter = std::slice::Iter<'list, Label>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Labels {
    /// Builds an empty list of labels.
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Adds a label. The date comes from `clock`. Returns the label added, or `None` when the text is empty, in which
    /// case nothing is added. No existing label is changed.
    pub fn add<S: Into<String>>(
        &mut self,
        text: S,
        target: LabelTarget,
        author: LabelAuthor,
        clock: &dyn Clock,
    ) -> Option<&Label> {
        let text = text.into();
        if text.is_empty() {
            return None;
        }
        self.entries.push(Label {
            text,
            target,
            author,
            date: clock.now(),
        });
        self.entries.last()
    }

    /// Adds the label `review-requested: <reason>` (just `review-requested` when nothing is left of the reason) on the
    /// session, authored automatically and dated by `clock`. The raw `reason` is masked, put on one line, bounded to 200
    /// characters and masked again. Returns the reason as stored.
    pub(crate) fn add_review_requested(
        &mut self,
        reason: &str,
        redactor: &Redactor,
        clock: &dyn Clock,
    ) -> String {
        let reason = single_line(&redactor.redact(reason), MAX_REASON_CHARS)
            .map_or_else(String::new, |bounded| redactor.redact(&bounded));
        let text = if reason.is_empty() {
            REVIEW_REQUESTED.to_owned()
        } else {
            format!("{REVIEW_REQUESTED}: {reason}")
        };
        let _added = self.add(text, LabelTarget::Session, LabelAuthor::Automatic, clock);
        reason
    }

    /// Adds the label `review-done` on the session, authored automatically and dated by `clock`.
    pub(crate) fn add_review_done(&mut self, clock: &dyn Clock) {
        let _added = self.add(REVIEW_DONE, LabelTarget::Session, LabelAuthor::Automatic, clock);
    }

    /// Returns every label, in the order they were added.
    pub fn iter(&self) -> std::slice::Iter<'_, Label> {
        self.entries.iter()
    }

    /// Returns the labels attached to `target`, in the order they were added.
    pub fn for_target(&self, target: &LabelTarget) -> impl Iterator<Item = &Label> {
        self.entries.iter().filter(move |label| label.target == *target)
    }
}

/// Longest reason of a review request, in characters, ellipsis included.
const MAX_REASON_CHARS: usize = 200;

/// Reserved label text of a review request, optionally followed by `: <reason>`.
const REVIEW_REQUESTED: &str = "review-requested";

/// Reserved label text of a session marked as analyzed.
const REVIEW_DONE: &str = "review-done";

/// State of the review of a session, read from its labels. Labels are only added: marking a session analyzed erases
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReviewState {
    /// The session was flagged for analysis, with the reason given (possibly empty).
    Requested(String),
    /// The session was marked as analyzed.
    Done,
}

/// Returns the review state of a session: the last review label wins (`review-requested[: <reason>]` or `review-done`),
/// `None` when the session carries none.
#[must_use]
pub fn review_state(labels: &Labels) -> Option<ReviewState> {
    labels.iter().rev().find_map(|label| {
        let text = label.text();
        if text == REVIEW_DONE {
            return Some(ReviewState::Done);
        }
        let rest = text.strip_prefix(REVIEW_REQUESTED)?;
        if rest.is_empty() {
            return Some(ReviewState::Requested(String::new()));
        }
        rest.strip_prefix(": ")
            .map(|reason| ReviewState::Requested(reason.to_owned()))
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Label, LabelAuthor, LabelTarget, Labels, ReviewState, review_state};
    use crate::clock::{ManualClock, Timestamp};
    use crate::session::Redactor;
    use crate::session::{RunId, TurnId};

    fn run(id: &str) -> RunId {
        RunId::new(id).expect("a non-empty text makes a run identifier")
    }

    /// Adds a label that must be accepted, the text being non-empty.
    fn add_accepted(
        labels: &mut Labels,
        text: &str,
        target: LabelTarget,
        author: LabelAuthor,
        clock: &ManualClock,
    ) {
        assert!(
            labels.add(text, target, author, clock).is_some(),
            "the non-empty label {text} must be added"
        );
    }

    #[test]
    fn label_recorded_with_author_and_date() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        let mut labels = Labels::new();
        let label = labels
            .add("good", LabelTarget::Run(run("r1")), LabelAuthor::Human, &clock)
            .expect("a non-empty label must be added");
        assert_eq!(
            label.author(),
            LabelAuthor::Human,
            "the author given to add must be recorded"
        );
        assert_eq!(
            label.date(),
            Timestamp::from_millis(1000),
            "the date must be read from the injected clock"
        );
        assert_eq!(
            label.target(),
            &LabelTarget::Run(run("r1")),
            "the target given to add must be recorded"
        );
        assert_eq!(label.text(), "good", "the text given to add must be recorded");
    }

    #[test]
    fn second_label_keeps_the_first() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        let mut labels = Labels::new();
        add_accepted(
            &mut labels,
            "first",
            LabelTarget::Session,
            LabelAuthor::Human,
            &clock,
        );
        clock.advance(Duration::from_millis(500));
        add_accepted(
            &mut labels,
            "second",
            LabelTarget::Session,
            LabelAuthor::Automatic,
            &clock,
        );

        let all: Vec<_> = labels
            .iter()
            .map(|label| (label.text().to_owned(), label.date()))
            .collect();
        assert_eq!(
            all,
            vec![
                ("first".to_owned(), Timestamp::from_millis(1000)),
                ("second".to_owned(), Timestamp::from_millis(1500)),
            ],
            "both labels must exist in the order they were added, each with its own date"
        );
    }

    #[test]
    fn labels_after_session_end() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        let mut labels = Labels::new();
        add_accepted(
            &mut labels,
            "early",
            LabelTarget::Session,
            LabelAuthor::Human,
            &clock,
        );
        let json = serde_json::to_string(&labels).expect("labels serialize with the session");

        let mut restored: Labels = serde_json::from_str(&json).expect("labels deserialize");
        clock.advance(Duration::from_millis(5000));
        add_accepted(
            &mut restored,
            "late",
            LabelTarget::Turn(TurnId::new(4)),
            LabelAuthor::Automatic,
            &clock,
        );

        let all: Vec<_> = restored.iter().collect();
        assert_eq!(
            all.len(),
            2,
            "the restored and the new label must both be present"
        );
        let early = all.first().expect("the earlier label must be present");
        assert_eq!(
            (early.text(), early.author(), early.date()),
            ("early", LabelAuthor::Human, Timestamp::from_millis(1000)),
            "the earlier label must be intact with its author and date"
        );
    }

    #[test]
    fn filter_by_target() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        let mut labels = Labels::new();
        add_accepted(
            &mut labels,
            "on-session",
            LabelTarget::Session,
            LabelAuthor::Human,
            &clock,
        );
        add_accepted(
            &mut labels,
            "on-turn-2",
            LabelTarget::Turn(TurnId::new(2)),
            LabelAuthor::Human,
            &clock,
        );
        add_accepted(
            &mut labels,
            "on-run-r1",
            LabelTarget::Run(run("r1")),
            LabelAuthor::Human,
            &clock,
        );

        let turn_two = LabelTarget::Turn(TurnId::new(2));
        let texts: Vec<&str> = labels.for_target(&turn_two).map(Label::text).collect();
        assert_eq!(
            texts,
            vec!["on-turn-2"],
            "for_target must return only the labels attached to that turn"
        );
    }

    #[test]
    fn empty_label_refused_on_deserialization() {
        let json = serde_json::json!([
            {"text": "", "target": "Session", "author": "Human", "date": 1000}
        ]);
        let result: Result<Labels, _> = serde_json::from_value(json);
        assert!(
            result.is_err(),
            "a serialized list holding a label with an empty text must fail to deserialize"
        );
    }

    #[test]
    fn empty_label_refused() {
        let clock = ManualClock::new(Timestamp::from_millis(1000));
        let mut labels = Labels::new();
        let added = labels.add("", LabelTarget::Session, LabelAuthor::Human, &clock);
        assert!(added.is_none(), "an empty label text must be refused");
        assert_eq!(
            labels.iter().count(),
            0,
            "a refused label must leave the list unchanged"
        );
    }

    // Scenario: le dernier mot gagne dans l'état de relecture
    #[test]
    fn the_last_word_wins_in_the_review_state() {
        let clock = ManualClock::new(Timestamp::from_millis(10));
        let mut labels = Labels::new();
        assert_eq!(
            review_state(&labels),
            None,
            "a session without review label has no state"
        );
        let _reason = labels.add_review_requested("lent", &Redactor::new(), &clock);
        assert_eq!(
            review_state(&labels),
            Some(ReviewState::Requested("lent".to_owned())),
            "a flag gives Requested with its reason"
        );
        clock.advance(Duration::from_millis(5));
        labels.add_review_done(&clock);
        assert_eq!(
            review_state(&labels),
            Some(ReviewState::Done),
            "marking gives Done"
        );
        let _second = labels.add_review_requested("encore lent", &Redactor::new(), &clock);
        assert_eq!(
            review_state(&labels),
            Some(ReviewState::Requested("encore lent".to_owned())),
            "a later flag gives Requested again"
        );
        let texts: Vec<_> = labels
            .iter()
            .map(|label| (label.text().to_owned(), label.author()))
            .collect();
        assert_eq!(
            texts,
            vec![
                ("review-requested: lent".to_owned(), LabelAuthor::Automatic),
                ("review-done".to_owned(), LabelAuthor::Automatic),
                ("review-requested: encore lent".to_owned(), LabelAuthor::Automatic),
            ],
            "labels are only added, automatic, in the reserved vocabulary"
        );
        assert!(
            labels.iter().all(|label| *label.target() == LabelTarget::Session),
            "review labels target the session"
        );
        assert_eq!(
            labels.iter().map(Label::date).collect::<Vec<_>>(),
            vec![
                Timestamp::from_millis(10),
                Timestamp::from_millis(15),
                Timestamp::from_millis(15)
            ],
            "each label is dated by the clock"
        );
    }

    #[test]
    fn a_reason_is_masked_on_one_line_and_bounded() {
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let mut redactor = Redactor::new();
        redactor.add_secret("s3cr3t-token").expect("a long enough secret");
        let mut labels = Labels::new();
        let stored = labels.add_review_requested("leak of s3cr3t-token\nsecond line", &redactor, &clock);
        assert_eq!(
            stored, "leak of *redacted secret* second line",
            "the reason is masked and on one line"
        );
        let long = labels.add_review_requested(&"x".repeat(300), &redactor, &clock);
        assert_eq!(
            long.chars().count(),
            200,
            "the reason is bounded to 200 characters"
        );
        assert!(long.ends_with('\u{2026}'), "a cut reason ends with an ellipsis");
        let texts: Vec<_> = labels.iter().map(|label| label.text().to_owned()).collect();
        assert_eq!(
            texts.first().map(String::as_str),
            Some("review-requested: leak of *redacted secret* second line"),
            "the label carries the stored reason"
        );
        assert!(
            texts.iter().all(|text| !text.contains("s3cr3t-token")),
            "no label holds the secret"
        );
    }

    #[test]
    fn an_empty_reason_still_flags_and_other_labels_are_ignored() {
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let mut labels = Labels::new();
        let _reason = labels.add_review_requested("", &Redactor::new(), &clock);
        assert_eq!(
            review_state(&labels),
            Some(ReviewState::Requested(String::new())),
            "an empty reason is a flag without reason"
        );
        add_accepted(
            &mut labels,
            "good",
            LabelTarget::Session,
            LabelAuthor::Human,
            &clock,
        );
        add_accepted(
            &mut labels,
            "review-done-ish",
            LabelTarget::Session,
            LabelAuthor::Human,
            &clock,
        );
        assert_eq!(
            review_state(&labels),
            Some(ReviewState::Requested(String::new())),
            "labels outside the reserved vocabulary do not change the state"
        );
        let mut plain = Labels::new();
        add_accepted(
            &mut plain,
            "good",
            LabelTarget::Session,
            LabelAuthor::Human,
            &clock,
        );
        assert_eq!(review_state(&plain), None, "free labels give no review state");
    }

    // Mutant: the first masking removed from add_review_requested. The secret straddles the 200th character.
    #[test]
    fn a_secret_straddling_the_reason_bound_leaves_no_piece() {
        let secret = "s3cr3t-token";
        let mut redactor = Redactor::new();
        redactor.add_secret(secret).expect("a long enough secret");
        let mut labels = Labels::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let stored =
            labels.add_review_requested(&format!("{}{secret} tail", "z".repeat(192)), &redactor, &clock);
        let chars: Vec<char> = secret.chars().collect();
        let text = labels.iter().map(Label::text).collect::<String>();
        for window in chars.windows(6) {
            let piece: String = window.iter().collect();
            assert!(
                !stored.contains(&piece),
                "the piece `{piece}` is in the reason: {stored}"
            );
            assert!(
                !text.contains(&piece),
                "the piece `{piece}` is in the label: {text}"
            );
        }
    }

    // Mutant: the second masking removed from add_review_requested. The normalization rebuilds `ab cd ef`.
    #[test]
    fn a_secret_rebuilt_by_the_normalization_is_masked_in_a_reason() {
        let mut redactor = Redactor::new();
        redactor.add_secret("ab cd ef").expect("a long enough secret");
        for raw in ["ab\ncd ef", "ab  cd   ef"] {
            let mut labels = Labels::new();
            let clock = ManualClock::new(Timestamp::from_millis(0));
            let stored = labels.add_review_requested(raw, &redactor, &clock);
            assert_eq!(stored, "*redacted secret*", "only the mask is left");
            assert!(
                labels.iter().all(|label| !label.text().contains("ab cd ef")),
                "no label holds the rebuilt secret"
            );
        }
    }
}
