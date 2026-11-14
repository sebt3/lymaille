//! Model profile: names a provider and a model, overrides what the provider detects, and resolves into the profile the rest
//! of the crate consumes. An override always wins over the detection (`agent-sessions/agents#FR-7`, `AC-2`).

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use super::{AgentsError, AgentsResult};
use crate::context::{Budget, ReplayMode};

/// Default tokenization hint: 2500 thousandths of a character per token, that is 2.5 characters per token.
const DEFAULT_TOKENIZATION_HINT: NonZeroU64 = match NonZeroU64::new(2500) {
    Some(hint) => hint,
    // Unreachable: 2500 is not zero. The fallback keeps the constant free of any panic.
    None => NonZeroU64::MIN,
};

/// Sampling parameters sent to the provider. Absent fields stay absent; extra parameters are passed through as they are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[non_exhaustive]
pub struct SamplingParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(type = "number"))]
    top_k: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, serde_json::Value>,
}

impl SamplingParams {
    /// Sets the temperature.
    #[must_use]
    pub const fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the nucleus sampling threshold.
    #[must_use]
    pub const fn with_top_p(mut self, top_p: f64) -> Self {
        self.top_p = Some(top_p);
        self
    }

    /// Sets the top-k cut-off.
    #[must_use]
    pub const fn with_top_k(mut self, top_k: u64) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// Adds a free parameter, passed to the provider as it is.
    #[must_use]
    pub fn with_extra(mut self, key: String, value: serde_json::Value) -> Self {
        self.extra.extend(std::iter::once((key, value)));
        self
    }

    /// Returns the temperature, if set.
    #[must_use]
    pub const fn temperature(&self) -> Option<f64> {
        self.temperature
    }

    /// Returns the nucleus sampling threshold, if set.
    #[must_use]
    pub const fn top_p(&self) -> Option<f64> {
        self.top_p
    }

    /// Returns the top-k cut-off, if set.
    #[must_use]
    pub const fn top_k(&self) -> Option<u64> {
        self.top_k
    }

    /// Returns the free parameters, sorted by key.
    #[must_use]
    pub const fn extra(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.extra
    }
}

/// What the provider announces for a model. Every field is optional: an absent field is not detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DetectedModel {
    context_size: Option<u64>,
    max_output: Option<u64>,
    tools: Option<bool>,
}

impl DetectedModel {
    /// Builds what the provider announces: the context size, the maximum output size and the tool support.
    #[must_use]
    pub const fn new(context_size: Option<u64>, max_output: Option<u64>, tools: Option<bool>) -> Self {
        Self {
            context_size,
            max_output,
            tools,
        }
    }

    /// Returns the announced context size, if any.
    #[must_use]
    pub const fn context_size(&self) -> Option<u64> {
        self.context_size
    }

    /// Returns the announced maximum output size, if any.
    #[must_use]
    pub const fn max_output(&self) -> Option<u64> {
        self.max_output
    }

    /// Returns the announced tool support, if any.
    #[must_use]
    pub const fn tools(&self) -> Option<bool> {
        self.tools
    }
}

/// Named model profile. It names a provider; it holds no address and no secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[non_exhaustive]
pub struct ModelProfile {
    name: String,
    provider: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(type = "number"))]
    context_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(type = "number"))]
    max_response: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sampling: Option<SamplingParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(type = "number"))]
    tokenization_hint: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-rs", ts(type = "number"))]
    concurrency: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replay: Option<ReplayMode>,
}

impl ModelProfile {
    /// Builds a profile naming the provider `provider` and the model `model`, with no override.
    #[must_use]
    pub const fn new(name: String, provider: String, model: String) -> Self {
        Self {
            name,
            provider,
            model,
            context_size: None,
            max_response: None,
            tools: None,
            sampling: None,
            tokenization_hint: None,
            concurrency: None,
            replay: None,
        }
    }

    /// Overrides the maximum context size.
    #[must_use]
    pub const fn with_context_size(mut self, context_size: u64) -> Self {
        self.context_size = Some(context_size);
        self
    }

    /// Overrides the maximum response size.
    #[must_use]
    pub const fn with_max_response(mut self, max_response: u64) -> Self {
        self.max_response = Some(max_response);
        self
    }

    /// Overrides the tool support.
    #[must_use]
    pub const fn with_tools(mut self, tools: bool) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Overrides the sampling parameters.
    #[must_use]
    pub fn with_sampling(mut self, sampling: SamplingParams) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// Overrides the tokenization hint, in thousandths of a character per token.
    #[must_use]
    pub const fn with_tokenization_hint(mut self, milli: NonZeroU64) -> Self {
        self.tokenization_hint = Some(milli);
        self
    }

    /// Overrides the maximum number of simultaneous requests.
    #[must_use]
    pub const fn with_concurrency(mut self, concurrency: NonZeroU64) -> Self {
        self.concurrency = Some(concurrency);
        self
    }

    /// Overrides the replay mode of the reasoning.
    #[must_use]
    pub const fn with_replay(mut self, replay: ReplayMode) -> Self {
        self.replay = Some(replay);
        self
    }

    /// Returns the name of the profile.
    #[cfg_attr(
        feature = "ts-rs",
        expect(
            clippy::same_name_method,
            reason = "the `TS` derive implements the `name` associated function of `ts_rs::TS`; the accessor takes `&self` and the trait function takes `&Config`, so method resolution never sees an ambiguity"
        )
    )]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the name of the provider the profile names.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the model identifier.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Resolves the profile against what the provider detects. Each value comes from the override if there is one, else
    /// from the detection, else from the documented default.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::ContextSizeUnknown`] when neither the override nor the detection gives the context size.
    pub fn resolve(&self, detected: Option<DetectedModel>) -> AgentsResult<ResolvedProfile> {
        let context_size = self
            .context_size
            .or_else(|| detected.and_then(|model| model.context_size))
            .ok_or_else(|| AgentsError::ContextSizeUnknown {
                name: self.name.clone(),
            })?;
        let max_response = self
            .max_response
            .or_else(|| detected.and_then(|model| model.max_output))
            .unwrap_or_else(|| context_size.saturating_div(8));
        let tools = self
            .tools
            .or_else(|| detected.and_then(|model| model.tools))
            .unwrap_or(true);
        Ok(ResolvedProfile {
            name: self.name.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            context_size,
            max_response,
            tools,
            sampling: self.sampling.clone(),
            tokenization_hint: self.tokenization_hint.unwrap_or(DEFAULT_TOKENIZATION_HINT),
            concurrency: self.concurrency.unwrap_or(NonZeroU64::MIN),
            replay: self.replay.unwrap_or(ReplayMode::None),
        })
    }
}

/// Model profile after resolution: every value is known. The rest of the crate consumes this type.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ResolvedProfile {
    name: String,
    provider: String,
    model: String,
    context_size: u64,
    max_response: u64,
    tools: bool,
    sampling: Option<SamplingParams>,
    tokenization_hint: NonZeroU64,
    concurrency: NonZeroU64,
    replay: ReplayMode,
}

impl ResolvedProfile {
    /// Returns the name of the profile.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the name of the provider.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the model identifier.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the context size, in tokens.
    #[must_use]
    pub const fn context_size(&self) -> u64 {
        self.context_size
    }

    /// Returns the maximum response size, in tokens.
    #[must_use]
    pub const fn max_response(&self) -> u64 {
        self.max_response
    }

    /// Tells whether the model accepts tool calls.
    #[must_use]
    pub const fn tools(&self) -> bool {
        self.tools
    }

    /// Returns the sampling parameters, if any.
    #[must_use]
    pub const fn sampling(&self) -> Option<&SamplingParams> {
        self.sampling.as_ref()
    }

    /// Returns the tokenization hint, in thousandths of a character per token.
    #[must_use]
    pub const fn tokenization_hint(&self) -> NonZeroU64 {
        self.tokenization_hint
    }

    /// Returns the maximum number of simultaneous requests.
    #[must_use]
    pub const fn concurrency(&self) -> NonZeroU64 {
        self.concurrency
    }

    /// Returns the replay mode of the reasoning.
    #[must_use]
    pub const fn replay(&self) -> ReplayMode {
        self.replay
    }

    /// Builds the context budget of the profile, with the summary prompt size `prompt` and the render cap
    /// `render_cap_chars` in characters.
    #[must_use]
    pub fn budget(&self, prompt: u64, render_cap_chars: u64) -> Budget {
        let base = Budget::new(self.context_size, prompt, self.max_response, render_cap_chars);
        base.with_chars_per_token_milli(self.tokenization_hint.get())
            .unwrap_or(base)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::{DetectedModel, ModelProfile, SamplingParams};
    use crate::context::ReplayMode;

    fn profile() -> ModelProfile {
        ModelProfile::new(
            "strix-qwen".to_owned(),
            "vllm-strix".to_owned(),
            "qwen3.8-flash-next".to_owned(),
        )
    }

    #[test]
    fn override_beats_detection() {
        let detected = DetectedModel::new(Some(8192), None, None);
        let resolved = profile()
            .with_context_size(262_144)
            .resolve(Some(detected))
            .expect("the profile overrides the context size");
        assert_eq!(
            resolved.context_size(),
            262_144,
            "the override must win over the detected 8192"
        );
    }

    #[test]
    fn detection_fills_what_is_not_overridden() {
        let detected = DetectedModel::new(Some(32_768), None, None);
        let resolved = profile()
            .resolve(Some(detected))
            .expect("the detection gives the context size");
        assert_eq!(
            resolved.context_size(),
            32_768,
            "without an override the detected context size is used"
        );
    }

    #[test]
    fn undetected_model_needs_an_override() {
        let resolved = profile().with_context_size(4096).resolve(None);
        assert!(
            resolved.is_ok(),
            "a profile with a context override resolves without detection"
        );
        let refused = profile().resolve(None);
        assert!(
            matches!(
                &refused,
                Err(crate::agents::AgentsError::ContextSizeUnknown { name }) if name == "strix-qwen"
            ),
            "a profile without override and without detection must fail naming the profile, got {refused:?}"
        );
    }

    #[test]
    fn defaults_documented() {
        let detected = DetectedModel::new(Some(40_000), None, None);
        let resolved = profile()
            .resolve(Some(detected))
            .expect("the detection gives the context size");
        assert_eq!(
            resolved.max_response(),
            5_000,
            "the default response is an eighth of the context"
        );
        assert!(resolved.tools(), "tools are supported by default");
        assert_eq!(resolved.replay(), ReplayMode::None, "replay is none by default");
        assert_eq!(
            resolved.concurrency(),
            NonZeroU64::MIN,
            "concurrency is 1 by default"
        );
        assert_eq!(
            resolved.tokenization_hint().get(),
            2500,
            "the tokenization hint is 2500 by default"
        );
    }

    #[test]
    fn compaction_uses_the_profile_context_size() {
        let detected = DetectedModel::new(Some(8192), None, None);
        let resolved = profile()
            .with_context_size(262_144)
            .resolve(Some(detected))
            .expect("the profile overrides the context size");
        let budget = resolved.budget(500, 10_000);
        assert!(
            !budget.needs_compaction(100_000),
            "with MAX 262144, PROMPT 500, R 32768 and T 4000, a session of 100000 tokens needs no compaction"
        );
    }

    #[test]
    fn profile_round_trips() {
        let sampling = SamplingParams::default()
            .with_temperature(0.7)
            .with_top_p(0.95)
            .with_top_k(40)
            .with_extra("min_p".to_owned(), serde_json::json!(0.05));
        let full = profile()
            .with_context_size(262_144)
            .with_max_response(8192)
            .with_tools(false)
            .with_sampling(sampling)
            .with_tokenization_hint(NonZeroU64::new(3000).expect("3000 is not zero"))
            .with_concurrency(NonZeroU64::new(4).expect("4 is not zero"))
            .with_replay(ReplayMode::AllTurns);
        let text = serde_json::to_string(&full).expect("a profile serializes");
        let back: ModelProfile = serde_json::from_str(&text).expect("a profile deserializes");
        assert_eq!(back, full, "the round trip must be identical");

        let bare = serde_json::to_value(profile()).expect("a profile serializes");
        for field in ["context_size", "max_response", "tools", "sampling", "replay"] {
            assert!(
                bare.get(field).is_none(),
                "a profile without override must not serialize `{field}`: {bare}"
            );
        }
    }

    #[test]
    fn override_beats_a_larger_detection() {
        let detected = DetectedModel::new(Some(524_288), None, None);
        let resolved = profile()
            .with_context_size(262_144)
            .resolve(Some(detected))
            .expect("the profile overrides the context size");
        assert_eq!(
            resolved.context_size(),
            262_144,
            "the override must win over a detected 524288"
        );
    }

    #[test]
    fn max_response_override_and_detection() {
        let detected = DetectedModel::new(Some(40_000), Some(5_000), None);
        let overridden = profile()
            .with_max_response(8_000)
            .resolve(Some(detected))
            .expect("the detection gives the context size");
        assert_eq!(
            overridden.max_response(),
            8_000,
            "the override must win over the detected maximum output"
        );
        let detected_only = profile()
            .resolve(Some(detected))
            .expect("the detection gives the context size");
        assert_eq!(
            detected_only.max_response(),
            5_000,
            "without override the detected maximum output is used, not the eighth default"
        );
    }

    #[test]
    fn tools_override_and_detection() {
        let supported = DetectedModel::new(Some(40_000), None, Some(true));
        let unsupported = DetectedModel::new(Some(40_000), None, Some(false));
        let overridden = profile()
            .with_tools(false)
            .resolve(Some(supported))
            .expect("the detection gives the context size");
        assert!(
            !overridden.tools(),
            "the override `false` must win over the detected support"
        );
        let forced = profile()
            .with_tools(true)
            .resolve(Some(unsupported))
            .expect("the detection gives the context size");
        assert!(
            forced.tools(),
            "the override `true` must win over the detected absence of tools"
        );
        let detected_absent = profile()
            .resolve(Some(unsupported))
            .expect("the detection gives the context size");
        assert!(
            !detected_absent.tools(),
            "without override the detected absence of tools must be used, not the default"
        );
        let detected_present = profile()
            .resolve(Some(supported))
            .expect("the detection gives the context size");
        assert!(
            detected_present.tools(),
            "without override the detected support of tools must be used"
        );
    }

    #[test]
    fn budget_depends_on_the_tokenization_hint() {
        let detected = DetectedModel::new(Some(40_000), None, None);
        let default_hint = profile()
            .resolve(Some(detected))
            .expect("the detection gives the context size");
        let dense = profile()
            .with_tokenization_hint(NonZeroU64::new(5_000).expect("5000 is not zero"))
            .resolve(Some(detected))
            .expect("the detection gives the context size");
        let default_cap = default_hint.budget(500, 10_000).render_cap_tokens();
        let dense_cap = dense.budget(500, 10_000).render_cap_tokens();
        assert_eq!(
            default_cap, 4_000,
            "with the default hint of 2.5 characters per token, 10000 characters are 4000 tokens"
        );
        assert_eq!(
            dense_cap, 2_000,
            "with a hint of 5 characters per token, 10000 characters are 2000 tokens"
        );
    }

    #[test]
    fn absent_hint_and_concurrency_stay_absent() {
        let bare = serde_json::to_value(profile()).expect("a profile serializes");
        for field in ["tokenization_hint", "concurrency"] {
            assert!(
                bare.get(field).is_none(),
                "a profile without override must not serialize `{field}`: {bare}"
            );
        }
        let text = r#"{"name":"strix-qwen","provider":"vllm-strix","model":"qwen3.8-flash-next"}"#;
        let read: ModelProfile = serde_json::from_str(text).expect("the required fields are enough");
        assert_eq!(
            read,
            profile(),
            "a profile with only the required fields must read back as the bare profile"
        );
        let back_text = serde_json::to_string(&read).expect("a profile serializes");
        let back: ModelProfile = serde_json::from_str(&back_text).expect("a profile deserializes");
        assert_eq!(back, read, "the round trip of a bare profile must be identical");
    }
}
