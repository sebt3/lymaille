//! Structured result of a sub-agent run: validation against the JSON Schema declared by its definition, corrections
//! within the same run, and explicit failure (`agent-sessions/delegation#FR-4`, `AC-2`, `AC-8`).
//!
//! The collector is shared between the Rhai module `result` and the run registry, so every method takes `&self` and the
//! state sits behind a lock. Validation messages name the place and the rule of the schema, never the submitted value.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard};

use jsonschema::error::{ValidationError, ValidationErrorKind};
use jsonschema::paths::LocationSegment;
use serde_json::Value;

use super::error::{DelegationError, DelegationResult};
use crate::session::SessionId;

/// Corrections allowed after the first attempt, unless a definition overrides them (`agent-sessions/delegation#FR-4`).
pub const DEFAULT_CORRECTIONS: u32 = 3;

/// Status of a sub-agent run as the result collector knows it.
///
/// The collector concludes with `Valid` or `Failed` only. `Cancelled` and `Interrupted` are set by the run registry
/// (`./run.sdd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubagentStatus {
    /// The run produced a result that validates against the schema.
    Valid,
    /// The run ended without a valid result: exhausted corrections, or an explicit failure.
    Failed,
    /// The run was cancelled.
    Cancelled,
    /// The run was interrupted by a session restore.
    Interrupted,
}

/// Answer of the collector to one submission.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Submission {
    /// The value validates: the result is accepted and the collector is concluded.
    Accepted {
        /// The accepted value.
        value: Value,
    },
    /// The value does not validate, but corrections remain: the errors go back to the sub-agent in the same run.
    Rejected {
        /// One line per validation error: the place and the violated rule, never the value.
        errors: Vec<String>,
        /// Corrections still allowed after this rejection.
        remaining: u32,
    },
    /// The value does not validate and no correction remains: the run fails with the errors of this attempt.
    Exhausted {
        /// One line per validation error of the last attempt.
        errors: Vec<String>,
    },
    /// The collector is already concluded: the submission is refused and the outcome does not change.
    Closed,
}

/// Outcome of a run as the result collector holds it once concluded.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SubagentOutcome {
    status: SubagentStatus,
    result: Option<Value>,
    errors: Vec<String>,
    reason: Option<String>,
    session: Option<SessionId>,
}

impl SubagentOutcome {
    /// Status of the run.
    #[must_use]
    pub const fn status(&self) -> SubagentStatus {
        self.status
    }

    /// Session of the sub-agent, when the conclusion carried it (`Interrupted`).
    #[must_use]
    pub const fn session(&self) -> Option<&SessionId> {
        self.session.as_ref()
    }

    /// The validated result, when the run produced one.
    #[must_use]
    pub const fn result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    /// Validation errors of the last attempt, when the run ended on them or on a later explicit failure.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Reason of an explicit failure, as the sub-agent gave it; the caller masks it before exposure.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// Collects the result of one sub-agent run: validates each submission, counts the corrections and concludes once.
#[derive(Debug)]
pub struct ResultCollector {
    validator: jsonschema::Validator,
    corrections: u32,
    /// Every property name the schema declares, at any depth: the only object keys an error path may render.
    declared: BTreeSet<String>,
    state: Mutex<State>,
}

/// Mutable part of the collector, behind the lock.
#[derive(Debug, Default)]
struct State {
    /// Number of invalid submissions so far.
    rejected: u32,
    /// Errors of the latest invalid submission.
    last_errors: Vec<String>,
    /// Set once the collector is concluded.
    outcome: Option<SubagentOutcome>,
}

impl ResultCollector {
    /// Builds a collector for the schema of `subagent`, with `corrections` corrections after the first attempt. The
    /// schema is compiled once, here.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidSchema`] when the schema does not compile as a JSON Schema, including a
    /// remote `$ref`.
    pub fn new(subagent: &str, schema: &Value, corrections: u32) -> DelegationResult<Self> {
        // The compiler's message is not kept: it can quote the schema, and the reason stays a fixed phrase.
        let validator =
            jsonschema::validator_for(schema).map_err(|_refusal| DelegationError::InvalidSchema {
                subagent: subagent.to_owned(),
                reason: "the schema does not compile as a JSON Schema",
            })?;
        let mut declared = BTreeSet::new();
        collect_declared(schema, &mut declared);
        Ok(Self {
            validator,
            corrections,
            declared,
            state: Mutex::new(State::default()),
        })
    }

    /// Builds a collector with [`DEFAULT_CORRECTIONS`] corrections.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::InvalidSchema`] under the same conditions as [`ResultCollector::new`].
    pub fn with_default_corrections(subagent: &str, schema: &Value) -> DelegationResult<Self> {
        Self::new(subagent, schema, DEFAULT_CORRECTIONS)
    }

    /// Submits a value. Concludes the collector when the value validates or when no correction remains.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the internal lock was poisoned.
    pub fn submit(&self, value: Value) -> DelegationResult<Submission> {
        let mut state = self.lock()?;
        if state.outcome.is_some() {
            return Ok(Submission::Closed);
        }
        let errors: Vec<String> = self
            .validator
            .iter_errors(&value)
            .map(|error| describe(&error, &self.declared))
            .collect();
        if errors.is_empty() {
            state.outcome = Some(SubagentOutcome {
                status: SubagentStatus::Valid,
                result: Some(value.clone()),
                errors: Vec::new(),
                reason: None,
                session: None,
            });
            return Ok(Submission::Accepted { value });
        }
        state.rejected = state.rejected.saturating_add(1);
        state.last_errors.clone_from(&errors);
        if state.rejected > self.corrections {
            state.outcome = Some(SubagentOutcome {
                status: SubagentStatus::Failed,
                result: None,
                errors: errors.clone(),
                reason: None,
                session: None,
            });
            drop(state);
            return Ok(Submission::Exhausted { errors });
        }
        // The first attempt is not a correction: after `rejected` invalid submissions, `corrections + 1 - rejected`
        // remain (three corrections give 3, 2, 1 after the first, second and third rejection).
        let remaining = self.corrections.saturating_add(1).saturating_sub(state.rejected);
        drop(state);
        Ok(Submission::Rejected { errors, remaining })
    }

    /// Concludes the collector as failed, with the reason given by the sub-agent. Refused (returns `false`) when the
    /// collector is already concluded.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the internal lock was poisoned.
    pub fn fail(&self, reason: &str) -> DelegationResult<bool> {
        let mut state = self.lock()?;
        if state.outcome.is_some() {
            return Ok(false);
        }
        let errors = state.last_errors.clone();
        state.outcome = Some(SubagentOutcome {
            status: SubagentStatus::Failed,
            result: None,
            errors,
            reason: Some(reason.to_owned()),
            session: None,
        });
        drop(state);
        Ok(true)
    }

    /// Concludes the collector with a status the registry sets: `Cancelled` or `Interrupted`. `session` is the session
    /// of the sub-agent, carried by an interruption (`run.sdd`, `RunInterrupted`). Returns `false`, with no effect, when
    /// the collector is already concluded or when `status` is neither `Cancelled` nor `Interrupted`: the refusal is not a
    /// distinct error, so a `false` always means that this call did not conclude the collector.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the internal lock was poisoned.
    pub fn conclude_as(
        &self,
        status: SubagentStatus,
        reason: &str,
        session: Option<SessionId>,
    ) -> DelegationResult<bool> {
        if !matches!(status, SubagentStatus::Cancelled | SubagentStatus::Interrupted) {
            return Ok(false);
        }
        let mut state = self.lock()?;
        if state.outcome.is_some() {
            return Ok(false);
        }
        let errors = state.last_errors.clone();
        state.outcome = Some(SubagentOutcome {
            status,
            result: None,
            errors,
            reason: Some(reason.to_owned()),
            session,
        });
        drop(state);
        Ok(true)
    }

    /// The outcome, once the collector is concluded; `None` before.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Poisoned`] when the internal lock was poisoned.
    pub fn outcome(&self) -> DelegationResult<Option<SubagentOutcome>> {
        let state = self.lock()?;
        Ok(state.outcome.clone())
    }

    fn lock(&self) -> DelegationResult<MutexGuard<'_, State>> {
        // The poisoning guard is not kept: it would hand out the state the panicking thread left behind.
        self.state.lock().map_err(|_poisoned| DelegationError::Poisoned)
    }
}

/// Renders one validation error: the place in the instance and the violated rule. The instance value is never rendered,
/// and a missing required property is named by its schema name. A submitted object key is rendered only when the schema
/// declares it; any other key becomes the fixed marker `*`. Array indexes are kept.
fn describe(error: &ValidationError<'_>, declared: &BTreeSet<String>) -> String {
    let segments: Vec<String> = error
        .instance_path()
        .iter()
        .map(|segment| match segment {
            LocationSegment::Property(name) => {
                let text: &str = name.as_ref();
                if declared.contains(text) {
                    text.to_owned()
                } else {
                    "*".to_owned()
                }
            }
            LocationSegment::Index(index) => index.to_string(),
        })
        .collect();
    let place = if segments.is_empty() {
        "the root".to_owned()
    } else {
        format!("`/{}`", segments.join("/"))
    };
    if let ValidationErrorKind::Required { property } = error.kind() {
        let name = property
            .as_str()
            .map_or_else(|| property.to_string(), str::to_owned);
        format!("{place}: rule `required` for property `{name}`")
    } else {
        format!("{place}: rule `{}`", error.kind().keyword())
    }
}

/// Collects every property name declared under a `properties` keyword, at any depth of the schema.
fn collect_declared(schema: &Value, out: &mut BTreeSet<String>) {
    match schema {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "properties"
                    && let Value::Object(properties) = child
                {
                    out.extend(properties.keys().cloned());
                }
                collect_declared(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_declared(item, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use serde_json::{Value, json};

    use super::{DEFAULT_CORRECTIONS, ResultCollector, SubagentStatus, Submission};
    use crate::delegation::error::DelegationError;
    use crate::session::SessionId;

    fn answer_schema() -> Value {
        json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"]
        })
    }

    fn collector(corrections: u32) -> ResultCollector {
        ResultCollector::new("extractor", &answer_schema(), corrections).expect("the answer schema compiles")
    }

    fn submit(collector: &ResultCollector, value: Value) -> Submission {
        collector.submit(value).expect("the state is not poisoned")
    }

    fn rejected_errors(submission: &Submission) -> &[String] {
        if let Submission::Rejected { errors, .. } = submission {
            errors
        } else {
            panic!("expected a rejection, got {submission:?}")
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

    // Scenario: résultat valide du premier coup
    #[test]
    fn first_valid_submission_is_accepted() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let value = json!({ "answer": "ok" });
        let submission = submit(&collector, value.clone());
        assert!(
            matches!(&submission, Submission::Accepted { value: accepted } if *accepted == value),
            "the first valid value must be accepted, got {submission:?}"
        );
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Valid,
            "the final status must be Valid"
        );
        assert_eq!(
            outcome.result(),
            Some(&value),
            "the outcome must carry the accepted result"
        );
    }

    // Scenario: correction dans le même run
    #[test]
    fn invalid_submission_is_corrected_in_the_same_run() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let first = submit(&collector, json!({}));
        let errors = rejected_errors(&first);
        assert!(
            errors.iter().any(|line| line.contains("answer")),
            "an error must name `answer`, got {errors:?}"
        );
        let second = submit(&collector, json!({ "answer": "ok" }));
        assert!(
            matches!(second, Submission::Accepted { .. }),
            "the corrected value must be accepted, got {second:?}"
        );
    }

    // Scenario: quatre soumissions invalides font échouer le run
    #[test]
    fn four_invalid_submissions_fail_the_run() {
        let collector = collector(DEFAULT_CORRECTIONS);
        // The three first attempts miss the required property; the fourth has the wrong type. Their errors differ, so the
        // exhausted outcome must carry the errors of the last attempt, not those of an earlier one.
        // (corrections still allowed after each rejection)
        for (attempt, remaining) in [(1u32, 3u32), (2, 2), (3, 1)] {
            let submission = submit(&collector, json!({}));
            assert!(
                matches!(submission, Submission::Rejected { remaining: left, .. } if left == remaining),
                "attempt {attempt} must be rejected with {remaining} corrections left, got {submission:?}"
            );
        }
        let last = submit(&collector, json!({ "answer": true }));
        let Submission::Exhausted { errors } = &last else {
            panic!("the fourth invalid submission must be exhausted, got {last:?}");
        };
        assert!(
            errors.iter().any(|line| line.contains("/answer")),
            "the exhausted errors must be those of the last attempt, got {errors:?}"
        );
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "the final status must be Failed"
        );
        assert_eq!(
            outcome.errors(),
            errors.as_slice(),
            "the outcome must carry the exhausted errors"
        );
    }

    // Scenario: la valeur n'est pas dans l'erreur
    #[test]
    fn validation_errors_never_carry_the_submitted_value() {
        let schema = json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"]
        });
        let collector =
            ResultCollector::new("counter", &schema, DEFAULT_CORRECTIONS).expect("the schema compiles");
        let submission = submit(&collector, json!({ "n": "s3cr3t" }));
        let errors = rejected_errors(&submission);
        assert!(
            errors
                .iter()
                .any(|line| line.contains("/n") && line.contains("type")),
            "an error must name the place `/n` and the rule `type`, got {errors:?}"
        );
        assert!(
            errors.iter().all(|line| !line.contains("s3cr3t")),
            "no error may carry the submitted value, got {errors:?}"
        );
    }

    // Scenario: échec explicite
    #[test]
    fn explicit_failure_after_a_rejection_carries_the_reason() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let _rejected = submit(&collector, json!({}));
        let concluded = collector
            .fail("the source document is missing")
            .expect("not poisoned");
        assert!(
            concluded,
            "a failure after a rejection must conclude the collector"
        );
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Failed,
            "the final status must be Failed"
        );
        assert_eq!(
            outcome.reason(),
            Some("the source document is missing"),
            "the outcome must carry the reason"
        );
    }

    // Scenario: résultat unique
    #[test]
    fn accepted_result_is_unique() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let first = json!({ "answer": "first" });
        let _accepted = submit(&collector, first.clone());
        let second = submit(&collector, json!({ "answer": "second" }));
        assert!(
            matches!(second, Submission::Closed),
            "a submission after acceptance must be refused, got {second:?}"
        );
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.result(),
            Some(&first),
            "the accepted result must be unchanged"
        );
    }

    // Scenario: nombre de corrections surchargeable
    #[test]
    fn corrections_are_overridable_per_definition() {
        let collector = collector(1);
        let first = submit(&collector, json!({}));
        assert!(
            matches!(first, Submission::Rejected { remaining: 1, .. }),
            "with one correction, the first rejection leaves one, got {first:?}"
        );
        let second = submit(&collector, json!({}));
        assert!(
            matches!(second, Submission::Exhausted { .. }),
            "the second invalid submission must be exhausted, got {second:?}"
        );
    }

    // Scenario: schéma invalide refusé
    #[test]
    fn unknown_schema_type_is_refused_at_creation() {
        let schema = json!({ "type": "entier" });
        let refused = ResultCollector::new("extractor", &schema, DEFAULT_CORRECTIONS);
        assert!(
            matches!(&refused, Err(DelegationError::InvalidSchema { subagent, .. }) if subagent == "extractor"),
            "an unknown type must be refused with InvalidSchema naming the sub-agent"
        );
    }

    // Must: a remote $ref is refused at compilation, with no network access.
    #[test]
    fn remote_reference_is_refused_at_compilation() {
        let schema = json!({ "$ref": "https://schemas.example.invalid/answer.json" });
        let refused = ResultCollector::with_default_corrections("extractor", &schema);
        assert!(
            matches!(refused, Err(DelegationError::InvalidSchema { .. })),
            "a remote $ref must be refused with InvalidSchema"
        );
    }

    // Must: the outcome is absent until the collector is concluded.
    #[test]
    fn outcome_is_absent_until_concluded() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let _rejected = submit(&collector, json!({}));
        assert!(
            collector.outcome().expect("not poisoned").is_none(),
            "a rejection alone must not conclude the collector"
        );
    }

    // Must: a failure after an accepted result is refused and the result stays.
    #[test]
    fn failure_after_acceptance_is_refused() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let _accepted = submit(&collector, json!({ "answer": "ok" }));
        let concluded = collector.fail("too late").expect("not poisoned");
        assert!(!concluded, "a failure after acceptance must be refused");
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Valid,
            "the final status must stay Valid"
        );
    }

    // Must: after exhaustion, any further submission is refused.
    #[test]
    fn submission_after_exhaustion_is_closed() {
        let collector = collector(0);
        let exhausted = submit(&collector, json!({}));
        assert!(
            matches!(exhausted, Submission::Exhausted { .. }),
            "with no correction, the first invalid submission must be exhausted, got {exhausted:?}"
        );
        let later = submit(&collector, json!({ "answer": "ok" }));
        assert!(
            matches!(later, Submission::Closed),
            "a submission after exhaustion must be refused, got {later:?}"
        );
    }

    // Must: the collector is Send + Sync and shared between the module and the registry.
    #[test]
    fn collector_is_send_and_sync() {
        assert_send_sync::<ResultCollector>();
    }

    // Must: a run cancelled or interrupted is concluded by the registry, with the session reference for `Interrupted`.
    #[test]
    fn cancelled_run_is_concluded_on_an_open_collector() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let concluded = collector
            .conclude_as(SubagentStatus::Cancelled, "the parent was cancelled", None)
            .expect("not poisoned");
        assert!(concluded, "a cancellation must conclude an open collector");
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "the final status must be Cancelled"
        );
        assert_eq!(
            outcome.reason(),
            Some("the parent was cancelled"),
            "the outcome must carry the reason"
        );
        assert!(
            outcome.session().is_none(),
            "a cancellation without a session carries none"
        );
    }

    // Must: `Interrupted` carries the session reference of the sub-agent, as `RunInterrupted` does (run.sdd, FR-14).
    #[test]
    fn interrupted_run_carries_the_session_reference() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let session = SessionId::new("session-of-the-subagent").expect("the identifier is not empty");
        let concluded = collector
            .conclude_as(
                SubagentStatus::Interrupted,
                "the session was restored",
                Some(session.clone()),
            )
            .expect("not poisoned");
        assert!(concluded, "an interruption must conclude an open collector");
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Interrupted,
            "the final status must be Interrupted"
        );
        assert_eq!(
            outcome.session(),
            Some(&session),
            "the outcome must carry the session reference"
        );
    }

    // Must: the conclusion is unique: a second one, of any kind, is refused and the first stays.
    #[test]
    fn conclusion_is_refused_once_concluded() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let _cancelled = collector
            .conclude_as(SubagentStatus::Cancelled, "first", None)
            .expect("not poisoned");
        let again = collector
            .conclude_as(SubagentStatus::Interrupted, "second", None)
            .expect("not poisoned");
        assert!(!again, "a second conclusion must be refused");
        let refused_failure = collector.fail("third").expect("not poisoned");
        assert!(!refused_failure, "a failure after a conclusion must be refused");
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.status(),
            SubagentStatus::Cancelled,
            "the first conclusion must stay"
        );
        assert_eq!(outcome.reason(), Some("first"), "the first reason must stay");
    }

    // Must: `conclude_as` refuses `Valid` and `Failed`, which only the collector itself reaches.
    #[test]
    fn conclude_as_refuses_valid_and_failed() {
        for status in [SubagentStatus::Valid, SubagentStatus::Failed] {
            let collector = collector(DEFAULT_CORRECTIONS);
            let concluded = collector
                .conclude_as(status, "refused", None)
                .expect("not poisoned");
            assert!(!concluded, "{status:?} must be refused by conclude_as");
            assert!(
                collector.outcome().expect("not poisoned").is_none(),
                "a refused status must leave the collector open"
            );
        }
    }

    // Must: validation errors never render a submitted key that the schema does not declare.
    #[test]
    fn undeclared_key_is_masked_in_the_error_path() {
        let schema = json!({ "type": "object", "additionalProperties": { "type": "integer" } });
        let collector =
            ResultCollector::new("counter", &schema, DEFAULT_CORRECTIONS).expect("the schema compiles");
        let submission = submit(&collector, json!({ "s3cr3t-key": "x" }));
        let errors = rejected_errors(&submission);
        assert!(
            errors
                .iter()
                .any(|line| line.contains("`/*`") && line.contains("type")),
            "the undeclared key must render as the fixed marker `*`, got {errors:?}"
        );
        assert!(
            errors.iter().all(|line| !line.contains("s3cr3t")),
            "no error may carry an undeclared key, got {errors:?}"
        );
    }

    // Must: the path of a declared property stays readable, at any depth, and array indexes are kept.
    #[test]
    fn declared_property_path_stays_readable() {
        let schema = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": { "items": { "type": "array", "items": { "type": "integer" } } }
                }
            }
        });
        let collector =
            ResultCollector::new("counter", &schema, DEFAULT_CORRECTIONS).expect("the schema compiles");
        let submission = submit(&collector, json!({ "outer": { "items": [1, "x"] } }));
        let errors = rejected_errors(&submission);
        assert!(
            errors
                .iter()
                .any(|line| line.contains("`/outer/items/1`") && line.contains("type")),
            "the declared path and the index must be readable, got {errors:?}"
        );
    }

    // Must: `fail` after a rejection reports the errors of the last attempt, not an empty list.
    #[test]
    fn failure_after_a_rejection_carries_the_last_errors() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let rejected = submit(&collector, json!({}));
        let expected = rejected_errors(&rejected).to_vec();
        assert!(!expected.is_empty(), "the rejection must carry errors");
        let concluded = collector.fail("gave up").expect("not poisoned");
        assert!(
            concluded,
            "a failure after a rejection must conclude the collector"
        );
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert_eq!(
            outcome.errors(),
            expected.as_slice(),
            "the failure must carry the errors of the last attempt"
        );
    }

    // Must: the default constructor allows DEFAULT_CORRECTIONS corrections: four invalid submissions exhaust the run.
    #[test]
    fn default_constructor_allows_the_default_corrections() {
        let collector = ResultCollector::with_default_corrections("extractor", &answer_schema())
            .expect("the answer schema compiles");
        for attempt in 1..=DEFAULT_CORRECTIONS {
            let submission = submit(&collector, json!({}));
            assert!(
                matches!(submission, Submission::Rejected { .. }),
                "attempt {attempt} must be rejected while corrections remain, got {submission:?}"
            );
        }
        let fourth = submit(&collector, json!({}));
        assert!(
            matches!(fourth, Submission::Exhausted { .. }),
            "the fourth invalid submission must be exhausted, got {fourth:?}"
        );
        let fifth = submit(&collector, json!({ "answer": "ok" }));
        assert!(
            matches!(fifth, Submission::Closed),
            "a submission after exhaustion must be Closed, got {fifth:?}"
        );
    }

    // Must: `conclude_as` reports the errors of the last rejected attempt, as `fail` does.
    #[test]
    fn conclusion_after_a_rejection_carries_the_last_errors() {
        let collector = collector(DEFAULT_CORRECTIONS);
        let rejected = submit(&collector, json!({}));
        let expected = rejected_errors(&rejected).to_vec();
        assert!(!expected.is_empty(), "the rejection must carry errors");
        let concluded = collector
            .conclude_as(SubagentStatus::Cancelled, "the parent was cancelled", None)
            .expect("not poisoned");
        assert!(concluded, "a cancellation must conclude the collector");
        let outcome = collector
            .outcome()
            .expect("not poisoned")
            .expect("the collector is concluded");
        assert!(
            !outcome.errors().is_empty(),
            "the cancelled outcome must carry the last errors"
        );
        assert_eq!(
            outcome.errors(),
            expected.as_slice(),
            "the cancelled outcome must carry the errors of the last attempt"
        );
    }

    // Must: an undeclared key whose name is declared elsewhere in the schema is rendered as schema text. This freezes
    // that case on purpose: the rendered text is then the schema's own name, which reveals nothing the schema does not
    // already contain (see the `Must` of result.sdd).
    #[test]
    fn undeclared_key_named_like_a_property_declared_elsewhere_is_rendered_as_schema_text() {
        let schema = json!({
            "type": "object",
            "properties": {
                "outer": { "type": "object", "additionalProperties": { "type": "integer" } },
                "secret-name": { "type": "string" }
            }
        });
        let collector =
            ResultCollector::new("counter", &schema, DEFAULT_CORRECTIONS).expect("the schema compiles");
        let submission = submit(&collector, json!({ "outer": { "secret-name": "x" } }));
        let errors = rejected_errors(&submission);
        assert!(
            errors
                .iter()
                .any(|line| line.contains("`/outer/secret-name`") && line.contains("type")),
            "a key declared elsewhere in the schema is rendered as schema text, got {errors:?}"
        );
    }

    // Must: a poisoned lock is reported, never a panic.
    #[test]
    fn poisoned_lock_is_reported_without_panic() {
        let collector = Arc::new(collector(DEFAULT_CORRECTIONS));
        let poisoner = Arc::clone(&collector);
        let joined = thread::spawn(move || {
            let _guard = poisoner.state.lock();
            panic!("poisoning the collector on purpose");
        })
        .join();
        assert!(joined.is_err(), "the poisoning thread must have panicked");
        assert!(
            matches!(collector.submit(json!({})), Err(DelegationError::Poisoned)),
            "submit must report the poisoned lock"
        );
        assert!(
            matches!(collector.fail("reason"), Err(DelegationError::Poisoned)),
            "fail must report the poisoned lock"
        );
        assert!(
            matches!(collector.outcome(), Err(DelegationError::Poisoned)),
            "outcome must report the poisoned lock"
        );
    }
}
