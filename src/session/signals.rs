//! Quality signals recorded for each turn and each run (`agent-sessions/session#FR-13`). This file holds only the
//! outcome type for now: the run signals (`RunSignals`) come with a later batch.

use serde::{Deserialize, Serialize};

/// Outcome of a turn or of a run: a valid result, a failure, or a cancellation by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Outcome {
    /// The result is valid.
    Valid,
    /// The turn or the run failed.
    Failed,
    /// The host cancelled the turn or the run.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::Outcome;

    #[test]
    fn outcome_round_trips_each_variant() {
        for outcome in [Outcome::Valid, Outcome::Failed, Outcome::Cancelled] {
            let json = serde_json::to_string(&outcome).expect("an outcome serializes");
            let back: Outcome = serde_json::from_str(&json).expect("a serialized outcome deserializes");
            assert_eq!(
                back, outcome,
                "the outcome {json} must deserialize to the same variant"
            );
        }
    }
}
