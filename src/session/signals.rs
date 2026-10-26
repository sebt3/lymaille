//! Quality signals recorded for each turn and each run (`agent-sessions/session#FR-13`): the outcome type and the run
//! signals kept after the run, for later selection (`agent-sessions#DEC-12`, `agent-sessions#DEC-13`).

use std::collections::BTreeMap;

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

/// Quality signals of one run: outcome, Rhai error and correction counts, and the labels that identify the run.
///
/// The labels are the producing agent, the model profile, the toolset versions, and an optional sub-agent and
/// workflow step.
///
/// Counters saturate. The outcome is set once: the first call to [`RunSignals::finish`] wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunSignals {
    agent: String,
    model_profile: String,
    toolsets: BTreeMap<String, String>,
    subagent: Option<String>,
    workflow_step: Option<String>,
    rhai_errors: u32,
    corrections: u32,
    outcome: Option<Outcome>,
}

impl RunSignals {
    /// Builds the signals of a run that has not finished yet.
    ///
    /// `agent` is the producing agent, `model_profile` the model profile, and `toolsets` maps each toolset name to its
    /// version. The toolsets are kept in name order.
    #[must_use]
    pub fn new<A: Into<String>, M: Into<String>>(
        agent: A,
        model_profile: M,
        toolsets: BTreeMap<String, String>,
    ) -> Self {
        Self {
            agent: agent.into(),
            model_profile: model_profile.into(),
            toolsets,
            subagent: None,
            workflow_step: None,
            rhai_errors: 0,
            corrections: 0,
            outcome: None,
        }
    }

    /// Sets the sub-agent label of the run.
    #[must_use]
    pub fn with_subagent<S: Into<String>>(mut self, subagent: S) -> Self {
        self.subagent = Some(subagent.into());
        self
    }

    /// Sets the workflow step label of the run.
    #[must_use]
    pub fn with_workflow_step<S: Into<String>>(mut self, step: S) -> Self {
        self.workflow_step = Some(step.into());
        self
    }

    /// Counts one Rhai error, saturating at the maximum.
    pub const fn record_rhai_error(&mut self) {
        self.rhai_errors = self.rhai_errors.saturating_add(1);
    }

    /// Counts one correction, saturating at the maximum.
    pub const fn record_correction(&mut self) {
        self.corrections = self.corrections.saturating_add(1);
    }

    /// Records the outcome of the run. Only the first outcome is kept: a later call has no effect.
    pub const fn finish(&mut self, outcome: Outcome) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }

    /// Returns the outcome, or `None` while the run has not finished.
    #[must_use]
    pub const fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// Returns the number of Rhai errors.
    #[must_use]
    pub const fn rhai_errors(&self) -> u32 {
        self.rhai_errors
    }

    /// Returns the number of corrections.
    #[must_use]
    pub const fn corrections(&self) -> u32 {
        self.corrections
    }

    /// Returns the producing agent.
    #[must_use]
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Returns the model profile.
    #[must_use]
    pub fn model_profile(&self) -> &str {
        &self.model_profile
    }

    /// Returns the toolset versions, in name order.
    #[must_use]
    pub const fn toolsets(&self) -> &BTreeMap<String, String> {
        &self.toolsets
    }

    /// Returns the sub-agent label, when there is one.
    #[must_use]
    pub fn subagent(&self) -> Option<&str> {
        self.subagent.as_deref()
    }

    /// Returns the workflow step label, when there is one.
    #[must_use]
    pub fn workflow_step(&self) -> Option<&str> {
        self.workflow_step.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Outcome, RunSignals};

    fn new_signals() -> RunSignals {
        RunSignals::new("main-agent", "profile-a", BTreeMap::new())
    }

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

    #[test]
    fn counters_and_outcome() {
        let mut signals = new_signals();
        signals.record_rhai_error();
        signals.record_rhai_error();
        signals.record_correction();
        signals.finish(Outcome::Valid);
        assert_eq!(signals.rhai_errors(), 2, "two recorded Rhai errors must count 2");
        assert_eq!(signals.corrections(), 1, "one recorded correction must count 1");
        assert_eq!(
            signals.outcome(),
            Some(Outcome::Valid),
            "the outcome given to finish must be read back"
        );
    }

    #[test]
    fn first_outcome_wins() {
        let mut signals = new_signals();
        signals.finish(Outcome::Failed);
        signals.finish(Outcome::Valid);
        assert_eq!(
            signals.outcome(),
            Some(Outcome::Failed),
            "a second finish must not replace the first outcome"
        );
    }

    #[test]
    fn unfinished_run_has_no_outcome() {
        assert_eq!(
            new_signals().outcome(),
            None,
            "a run that has not finished must have no outcome"
        );
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let versions: BTreeMap<String, String> = [("zeta", "2.0.0"), ("alpha", "1.4.2")]
            .into_iter()
            .map(|(name, version)| (name.to_owned(), version.to_owned()))
            .collect();
        let mut signals = RunSignals::new("producer", "model-profile", versions)
            .with_subagent("sub-1")
            .with_workflow_step("step-3");
        signals.record_correction();
        signals.finish(Outcome::Cancelled);

        let json = serde_json::to_string(&signals).expect("run signals serialize");
        let back: RunSignals = serde_json::from_str(&json).expect("run signals deserialize");

        assert_eq!(
            back, signals,
            "the run signals must be identical after a round trip"
        );
        assert_eq!(
            back.subagent(),
            Some("sub-1"),
            "the sub-agent must survive the round trip"
        );
        assert_eq!(
            back.workflow_step(),
            Some("step-3"),
            "the workflow step must survive the round trip"
        );
        assert_eq!(
            back.agent(),
            "producer",
            "the producing agent must survive the round trip"
        );
        assert_eq!(
            back.model_profile(),
            "model-profile",
            "the model profile must survive the round trip"
        );
        assert_eq!(
            back.corrections(),
            1,
            "the correction count must survive the round trip"
        );
        assert_eq!(
            back.outcome(),
            Some(Outcome::Cancelled),
            "the outcome must survive the round trip"
        );
        assert_eq!(
            back.toolsets().len(),
            2,
            "both toolset versions must survive the round trip"
        );
        let alpha = json.find("\"alpha\"");
        let zeta = json.find("\"zeta\"");
        assert!(
            matches!((alpha, zeta), (Some(a), Some(z)) if a < z),
            "toolsets must serialize in name order, got {json}"
        );
    }

    #[test]
    fn counters_saturate() {
        let mut value = serde_json::to_value(new_signals()).expect("run signals serialize as a value");
        value["rhai_errors"] = serde_json::json!(u32::MAX - 1);
        let mut signals: RunSignals = serde_json::from_value(value).expect("the edited value deserializes");
        signals.record_rhai_error();
        assert_eq!(
            signals.rhai_errors(),
            u32::MAX,
            "the first record must reach the maximum"
        );
        signals.record_rhai_error();
        assert_eq!(
            signals.rhai_errors(),
            u32::MAX,
            "a counter at its maximum must stay at the maximum"
        );
    }
}
