//! Labels added by the host to a session, a turn or a run, during or after the session. Entries are only added, never
//! changed nor removed (`agent-sessions/session#FR-15`, `agent-sessions#DEC-13`, `agent-sessions/session#AC-10`).

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::{RunId, TurnId};
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

    /// Returns every label, in the order they were added.
    pub fn iter(&self) -> std::slice::Iter<'_, Label> {
        self.entries.iter()
    }

    /// Returns the labels attached to `target`, in the order they were added.
    pub fn for_target(&self, target: &LabelTarget) -> impl Iterator<Item = &Label> {
        self.entries.iter().filter(move |label| label.target == *target)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Label, LabelAuthor, LabelTarget, Labels};
    use crate::clock::{ManualClock, Timestamp};
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
}
