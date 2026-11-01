//! Context of a segment as the model received it: the rendered system prompt, the native tool definitions and a snapshot
//! of the resolved model profile. Persisted at the start of each segment so that a faithful export stays possible
//! (`agent-sessions/session#FR-11`, `agent-sessions#DEC-12`, `agent-sessions/session#AC-7`).

use serde::{Deserialize, Serialize};

use super::ReplayMode;
use crate::clock::Timestamp;

/// Why a segment starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BoundaryKind {
    /// The start of the session.
    Start,
    /// The start of the segment that follows a compaction.
    Compaction,
    /// The start of the segment created when the session is restored.
    Restore,
}

/// Snapshot of the resolved model profile as it was sent.
///
/// It holds the model identifier and the replay mode of its reasoning. The mode is persisted as its text (`none`,
/// `intra_turn`, `all_turns`), and a text that is not a mode is refused on read. It is a copy of the values used, not the
/// agents' `ModelProfile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ModelSnapshot {
    model_id: String,
    reasoning_replay: ReplayMode,
}

impl ModelSnapshot {
    /// Builds a model snapshot from the model identifier and the reasoning replay mode.
    #[must_use]
    pub fn new<M: Into<String>>(model_id: M, reasoning_replay: ReplayMode) -> Self {
        Self {
            model_id: model_id.into(),
            reasoning_replay,
        }
    }

    /// Returns the model identifier.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Returns the reasoning replay mode.
    #[must_use]
    pub const fn reasoning_replay(&self) -> ReplayMode {
        self.reasoning_replay
    }
}

/// What the model received for a segment: the rendered system prompt, the tool definitions in their order, and the
/// model snapshot.
///
/// It holds rendered text and definitions only, never a secret value (`agent-sessions#CON-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SegmentContext {
    system_prompt: String,
    tool_definitions: Vec<serde_json::Value>,
    profile: ModelSnapshot,
}

impl SegmentContext {
    /// Builds the context of a segment. The tool definitions keep the order given here.
    #[must_use]
    pub fn new<S: Into<String>>(
        system_prompt: S,
        tool_definitions: Vec<serde_json::Value>,
        profile: ModelSnapshot,
    ) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            tool_definitions,
            profile,
        }
    }

    /// Returns the rendered system prompt.
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Returns the tool definitions, in the order they were given.
    #[must_use]
    pub fn tool_definitions(&self) -> &[serde_json::Value] {
        &self.tool_definitions
    }

    /// Returns the model snapshot.
    #[must_use]
    pub const fn profile(&self) -> &ModelSnapshot {
        &self.profile
    }
}

/// One segment of a session. Immutable once created: no accessor changes its context.
///
/// A restore with newer definitions creates a new segment and leaves the sent one untouched
/// (`agent-sessions/session#FR-11`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Segment {
    boundary: BoundaryKind,
    context: SegmentContext,
    started_at: Timestamp,
}

impl Segment {
    /// Builds a segment that starts at `started_at` for the given reason, with the context the model receives.
    #[must_use]
    pub const fn new(boundary: BoundaryKind, context: SegmentContext, started_at: Timestamp) -> Self {
        Self {
            boundary,
            context,
            started_at,
        }
    }

    /// Returns why the segment starts.
    #[must_use]
    pub const fn boundary(&self) -> BoundaryKind {
        self.boundary
    }

    /// Returns the context the model received for this segment.
    #[must_use]
    pub const fn context(&self) -> &SegmentContext {
        &self.context
    }

    /// Returns the instant the segment started.
    #[must_use]
    pub const fn started_at(&self) -> Timestamp {
        self.started_at
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{BoundaryKind, ModelSnapshot, ReplayMode, Segment, SegmentContext};
    use crate::clock::Timestamp;

    fn profile() -> ModelSnapshot {
        ModelSnapshot::new("model-x", ReplayMode::AllTurns)
    }

    // The snapshot of a segment written before the replay mode became a type: its text is read back as the same mode.
    #[test]
    fn an_older_snapshot_reads_back_from_its_text() {
        let text = r#"{"model_id":"model-x","reasoning_replay":"all_turns"}"#;
        let snapshot: ModelSnapshot =
            serde_json::from_str(text).expect("a snapshot written as text reads back");
        assert_eq!(
            snapshot,
            ModelSnapshot::new("model-x", ReplayMode::AllTurns),
            "the text all_turns must read back as the same mode"
        );
        assert_eq!(snapshot.reasoning_replay(), ReplayMode::AllTurns);
    }

    #[test]
    fn a_snapshot_writes_the_mode_as_its_text() {
        for mode in [ReplayMode::None, ReplayMode::IntraTurn, ReplayMode::AllTurns] {
            let value =
                serde_json::to_value(ModelSnapshot::new("model-x", mode)).expect("a snapshot serializes");
            assert_eq!(
                value.get("reasoning_replay"),
                Some(&json!(mode.as_str())),
                "the mode is written as its text, so no format change"
            );
        }
    }

    #[test]
    fn an_unknown_replay_text_is_refused_on_read() {
        let text = r#"{"model_id":"model-x","reasoning_replay":"replay-verbatim"}"#;
        assert!(
            serde_json::from_str::<ModelSnapshot>(text).is_err(),
            "a text that is not a replay mode must be refused on read"
        );
    }

    fn definitions() -> Vec<serde_json::Value> {
        vec![
            json!({"name": "rhai_pad", "parameters": {"type": "object"}}),
            json!({"name": "skill", "parameters": {"type": "object", "required": ["name"]}}),
        ]
    }

    #[test]
    fn segment_keeps_the_context_as_sent() {
        let context = SegmentContext::new("You are a work agent.", definitions(), profile());
        let segment = Segment::new(BoundaryKind::Start, context, Timestamp::from_millis(1000));

        let json = serde_json::to_string(&segment).expect("a segment serializes");
        let back: Segment = serde_json::from_str(&json).expect("a serialized segment deserializes");

        assert_eq!(
            back.context().system_prompt(),
            "You are a work agent.",
            "the system prompt must be identical after a round trip"
        );
        assert_eq!(
            back.context().tool_definitions(),
            definitions().as_slice(),
            "the tool definitions must be identical and in the same order after a round trip"
        );
        assert_eq!(
            back.context().profile(),
            &profile(),
            "the model profile must be identical after a round trip"
        );
        assert_eq!(
            back, segment,
            "the whole segment must be identical after a round trip"
        );
    }

    #[test]
    fn a_restore_creates_a_new_segment() {
        let start_context = SegmentContext::new("prompt v1", definitions(), profile());
        let start = Segment::new(
            BoundaryKind::Start,
            start_context.clone(),
            Timestamp::from_millis(1000),
        );

        let newer_context = SegmentContext::new("prompt v2", definitions(), profile());
        let restore = Segment::new(BoundaryKind::Restore, newer_context, Timestamp::from_millis(2000));

        let segments = [start.clone(), restore];
        assert_eq!(
            segments.len(),
            2,
            "the start and the restore segments must both exist"
        );
        assert_eq!(
            segments.get(1).map(Segment::boundary),
            Some(BoundaryKind::Restore),
            "the second segment must be the restore boundary"
        );
        assert_eq!(
            start.context(),
            &start_context,
            "the start segment must be unchanged by the restore"
        );
        assert_eq!(
            segments.get(1).map(|segment| segment.context().system_prompt()),
            Some("prompt v2"),
            "the restore segment must carry the newer prompt next to the start segment"
        );

        let json = serde_json::to_string(&start).expect("the start segment serializes");
        let back: Segment = serde_json::from_str(&json).expect("the start segment deserializes");
        assert_eq!(
            back, start,
            "the start segment must survive a serialization unchanged, next to the restore segment"
        );
    }
}
