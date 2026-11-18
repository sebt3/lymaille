//! Production summarizer: a [`Summarizer`] over an [`InferenceProvider`] and a host-resolved profile, so the host does
//! not rewrite that call (`agent-sessions/context-window#FR-11`, `FR-7`).

use crate::agents::{InferenceProvider, InferenceRequest, ResolvedProfile};
use crate::context::{ContextError, ContextResult, Digest, Summarizer, summary_prompt};
use crate::session::Message;

/// Production summarizer: sends the transcript to `provider` as one user message with no tool, under the summary
/// prompt, and returns the text of the answer (`agent-sessions/context-window#FR-11`).
///
/// The profile is the one the host resolved for the summarizing model; the digest is not sent, the strategy appends
/// it to the summary message. The provider failure becomes [`ContextError::Summarizer`] carrying its message, never
/// the transcript. It waits and retries nothing: the turn loop owns the backoff.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProviderSummarizer<P> {
    provider: P,
    profile: ResolvedProfile,
    prompt: String,
}

impl<P: InferenceProvider> ProviderSummarizer<P> {
    /// Builds the summarizer over `provider` (most often an `Arc`, see [`InferenceProvider`]) and the
    /// [`ResolvedProfile`] of the summarizing model, under the default summary prompt.
    #[must_use]
    pub fn new(provider: P, profile: ResolvedProfile) -> Self {
        Self {
            provider,
            profile,
            prompt: summary_prompt(None).to_owned(),
        }
    }

    /// Sets the summary prompt (chained constructor): through [`summary_prompt`], a blank or empty override falls
    /// back to [`crate::context::DEFAULT_SUMMARY_PROMPT`].
    #[must_use]
    pub fn with_prompt(mut self, prompt: &str) -> Self {
        summary_prompt(Some(prompt)).clone_into(&mut self.prompt);
        self
    }
}

impl<P: InferenceProvider> Summarizer for ProviderSummarizer<P> {
    fn summarize(
        &self,
        transcript: &str,
        _digest: Option<&Digest>,
    ) -> impl Future<Output = ContextResult<String>> + Send {
        let request = InferenceRequest::new(
            self.profile.clone(),
            self.prompt.clone(),
            vec![Message::user(transcript.to_owned())],
            Vec::new(),
            vec![None],
        );
        async move {
            let response = self
                .provider
                .complete(request)
                .await
                .map_err(|error| ContextError::Summarizer(error.message().to_owned()))?;
            Ok(answer_text(response.message()))
        }
    }
}

/// Text of the answer: the content of the assistant message, an empty text for any other message.
fn answer_text(message: &Message) -> String {
    match message {
        Message::Assistant { content, .. } => content.clone(),
        Message::System { .. }
        | Message::User { .. }
        | Message::ToolResult { .. }
        | Message::Boundary { .. }
        | Message::Discarded { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::ProviderSummarizer;
    use crate::agents::{
        ErrorClass, InferenceError, InferenceProvider as _, InferenceRequest, InferenceResponse,
        ModelProfile, ResolvedProfile, ScriptedProvider,
    };
    use crate::context::{
        CompactionStrategy as _, ContextError, DEFAULT_SUMMARY_PROMPT, DefaultStrategy, Digest, ReplayMode,
        Summarizer as _,
    };
    use crate::session::{BoundaryKind, History, Message};

    // "Résumé" written with escapes: the harness denies non-ASCII string literals (`non_ascii_literal`).
    const SUMMARY: &str = "R\u{e}sum\u{e}";

    fn profile() -> ResolvedProfile {
        ModelProfile::new(
            "strix-qwen".to_owned(),
            "vllm-strix".to_owned(),
            "qwen3.8-flash-next".to_owned(),
        )
        .with_context_size(32_768)
        .resolve(None)
        .expect("a profile with a context size resolves")
    }

    fn answer(text: &str) -> InferenceResponse {
        InferenceResponse::new(Message::assistant(text.to_owned(), None, Vec::new()), None)
    }

    /// The request the summarizer must build for `transcript` under the system prompt `prompt`.
    fn expected_request(prompt: &str, transcript: &str) -> InferenceRequest {
        InferenceRequest::new(
            profile(),
            prompt.to_owned(),
            vec![Message::user(transcript.to_owned())],
            Vec::new(),
            vec![None],
        )
    }

    // Scenario: le résumeur envoie la transcription sous le prompt par défaut
    #[tokio::test]
    async fn summarize_sends_the_transcript_under_the_default_prompt() {
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(answer(SUMMARY))]));
        let summarizer = ProviderSummarizer::new(Arc::clone(&provider), profile());
        // The digest is given to show it is not sent: the strategy appends it to the summary message.
        let summary = summarizer
            .summarize("user: bonjour", Some(&Digest::new(3, 7)))
            .await
            .expect("the scripted answer must be given");
        assert_eq!(
            summary, SUMMARY,
            "the text of the provider answer is returned as it is"
        );
        let requests = provider.requests();
        assert_eq!(requests.len(), 1, "the provider must see exactly one call");
        assert_eq!(
            requests[0],
            expected_request(DEFAULT_SUMMARY_PROMPT, "user: bonjour"),
            "the request carries the given profile, the default summary prompt as system prompt, the exact \
             transcript as its single user message, and nothing else"
        );
        assert!(requests[0].tools().is_empty(), "no tool is defined");
        assert!(
            requests[0].reasoning().iter().all(Option::is_none),
            "no reasoning is replayed"
        );
    }

    // Scenario: la surcharge du prompt remplace le défaut
    #[tokio::test]
    async fn prompt_override_replaces_the_default() {
        // "Résume en une ligne" written with escapes (see `SUMMARY`).
        let custom = "R\u{e}sume en une ligne";
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(answer(SUMMARY)),
            Ok(answer(SUMMARY)),
        ]));
        let overriding = ProviderSummarizer::new(Arc::clone(&provider), profile()).with_prompt(custom);
        let blank = ProviderSummarizer::new(Arc::clone(&provider), profile()).with_prompt("   ");
        let first = overriding
            .summarize("user: one", None)
            .await
            .expect("the first scripted answer");
        let second = blank
            .summarize("user: two", None)
            .await
            .expect("the second scripted answer");
        assert_eq!(
            (first.as_str(), second.as_str()),
            (SUMMARY, SUMMARY),
            "both summarizers return the provider answer"
        );
        let requests = provider.requests();
        assert_eq!(requests.len(), 2, "one call per summarizer");
        assert_eq!(
            requests[0].system_prompt(),
            custom,
            "the override replaces the default prompt"
        );
        assert_eq!(
            requests[1].system_prompt(),
            DEFAULT_SUMMARY_PROMPT,
            "a blank override falls back to the default prompt"
        );
    }

    // Scenario: l'échec du fournisseur devient l'erreur du résumeur
    #[tokio::test]
    async fn provider_failure_becomes_the_summarizer_error() {
        let provider = Arc::new(ScriptedProvider::new(vec![Err(InferenceError::new(
            ErrorClass::Transient,
            "rate limited by the server".to_owned(),
        ))]));
        let summarizer = ProviderSummarizer::new(Arc::clone(&provider), profile());
        let result = summarizer.summarize("user: bonjour", None).await;
        assert!(
            matches!(&result, Err(ContextError::Summarizer(message))
                if message == "rate limited by the server" && !message.contains("bonjour")),
            "the provider failure must become ContextError::Summarizer carrying its message without the transcript, \
             got {result:?}"
        );
        assert_eq!(
            provider.requests().len(),
            1,
            "no retry here: the provider must see one call only"
        );
    }

    // Scenario: le fournisseur partagé sert la boucle de tour et le résumeur
    #[tokio::test]
    async fn shared_provider_serves_the_turn_loop_and_the_summarizer() {
        let shared = Arc::new(ScriptedProvider::new(vec![
            Ok(answer(SUMMARY)),
            Ok(answer("hello")),
        ]));
        let summarizer = ProviderSummarizer::new(Arc::clone(&shared), profile());
        let summary = summarizer
            .summarize("user: bonjour", None)
            .await
            .expect("the scripted summary");
        assert_eq!(summary, SUMMARY, "the summarizer answer goes through the Arc");
        let turn_request = InferenceRequest::new(
            profile(),
            "system turn".to_owned(),
            vec![Message::user("hello")],
            Vec::new(),
            vec![None],
        );
        let response = shared
            .complete(turn_request.clone())
            .await
            .expect("the scripted turn answer");
        assert_eq!(
            response.message(),
            &Message::assistant("hello", None, Vec::new()),
            "the turn loop answers through the same provider"
        );
        assert_eq!(
            shared.requests(),
            vec![
                expected_request(DEFAULT_SUMMARY_PROMPT, "user: bonjour"),
                turn_request
            ],
            "the shared provider records the summarizer request then the turn request, in order"
        );
    }

    // Scenario: la stratégie par défaut s'appuie sur le résumeur de production
    #[tokio::test]
    async fn default_strategy_uses_the_production_summarizer() {
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(answer(SUMMARY))]));
        let strategy = DefaultStrategy::new(ProviderSummarizer::new(provider, profile()), ReplayMode::None);
        let mut history = History::new();
        history.push(Message::user("draw a map"));
        history.push(Message::assistant("here it is", None, Vec::new()));
        history.push(Message::user("and a legend"));
        let messages = strategy
            .compact(&history, None)
            .await
            .expect("the strategy compacts through the production summarizer");
        assert!(
            matches!(
                messages.first(),
                Some(Message::Boundary {
                    kind: BoundaryKind::Compaction
                })
            ),
            "the new segment opens with a Compaction boundary, got {messages:?}"
        );
        assert!(
            matches!(messages.get(1), Some(Message::User { content }) if content.contains(SUMMARY)),
            "the boundary is followed by a user message carrying the summary, got {messages:?}"
        );
    }
}
