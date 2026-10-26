//! Inference provider trait that the turn loop calls: request and response, classification of errors into transient,
//! permanent or context overflow, and a scripted provider to test without a real model
//! (`agent-sessions#FR-6`, `agent-sessions#NFR-1`, `agent-sessions/session#AC-8`, `AC-9`, `AC-11`). No wait and no retry
//! here: the backoff belongs to the turn loop (`agent-sessions/session` backoff).

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use serde_json::Value;

use crate::agents::ResolvedProfile;
use crate::session::{Message, Reasoning};

/// Class of an inference error, which tells the turn loop what to do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorClass {
    /// The failure may go away: rate limit, server error, network failure, timeout. The turn loop may retry.
    Transient,
    /// The failure will not go away by itself: bad request, authentication, unknown resource, other client error.
    Permanent,
    /// The request is larger than the model context. The turn loop may compact and retry.
    ContextOverflow,
}

/// Usage reported by the provider for one response: prompt tokens and completion tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReportedUsage {
    prompt: u64,
    completion: u64,
}

impl ReportedUsage {
    /// Builds the usage: the prompt tokens and the completion tokens.
    #[must_use]
    pub const fn new(prompt: u64, completion: u64) -> Self {
        Self { prompt, completion }
    }

    /// Returns the prompt tokens.
    #[must_use]
    pub const fn prompt(&self) -> u64 {
        self.prompt
    }

    /// Returns the completion tokens.
    #[must_use]
    pub const fn completion(&self) -> u64 {
        self.completion
    }
}

/// Error of an inference call. Its message names the cause and never carries the content of a request nor a secret.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct InferenceError {
    class: ErrorClass,
    message: String,
}

impl InferenceError {
    /// Builds an error of class `class` with the text `message`.
    #[must_use]
    pub const fn new(class: ErrorClass, message: String) -> Self {
        Self { class, message }
    }

    /// Returns the class of the error.
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        self.class
    }

    /// Returns the message of the error.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Request of one inference call: resolved profile, system prompt, messages, tool definitions as JSON, and the reasoning
/// to replay.
///
/// The reasoning is a list parallel to `messages`: entry `i` is the reasoning to send back with message `i`, or nothing.
/// It is owned, so that the request can cross tasks and be recorded by a test provider.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InferenceRequest {
    profile: ResolvedProfile,
    system_prompt: String,
    messages: Vec<Message>,
    tools: Vec<Value>,
    reasoning: Vec<Option<Reasoning>>,
}

impl InferenceRequest {
    /// Builds a request from its parts.
    #[must_use]
    pub const fn new(
        profile: ResolvedProfile,
        system_prompt: String,
        messages: Vec<Message>,
        tools: Vec<Value>,
        reasoning: Vec<Option<Reasoning>>,
    ) -> Self {
        Self {
            profile,
            system_prompt,
            messages,
            tools,
            reasoning,
        }
    }

    /// Returns the resolved profile, which carries the sampling parameters.
    #[must_use]
    pub const fn profile(&self) -> &ResolvedProfile {
        &self.profile
    }

    /// Returns the system prompt.
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Returns the messages, in order.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns the tool definitions, as JSON.
    #[must_use]
    pub fn tools(&self) -> &[Value] {
        &self.tools
    }

    /// Returns the reasoning to replay, parallel to the messages.
    #[must_use]
    pub fn reasoning(&self) -> &[Option<Reasoning>] {
        &self.reasoning
    }
}

/// Response of one inference call: the message of the assistant and the usage the provider reports, if it does.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InferenceResponse {
    message: Message,
    usage: Option<ReportedUsage>,
}

impl InferenceResponse {
    /// Builds a response from the assistant message and the reported usage.
    #[must_use]
    pub const fn new(message: Message, usage: Option<ReportedUsage>) -> Self {
        Self { message, usage }
    }

    /// Returns the message of the assistant.
    #[must_use]
    pub const fn message(&self) -> &Message {
        &self.message
    }

    /// Returns the usage the provider reports, if it does.
    #[must_use]
    pub const fn usage(&self) -> Option<ReportedUsage> {
        self.usage
    }
}

/// Inference provider, implemented by an adapter or by a test provider. It is `Send + Sync` and used through a generic
/// parameter only, never as a trait object.
pub trait InferenceProvider: Send + Sync {
    /// Sends `request` and returns the response.
    ///
    /// # Errors
    ///
    /// Returns an [`InferenceError`] carrying its [`ErrorClass`] when the call fails.
    fn complete(
        &self,
        request: InferenceRequest,
    ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send;
}

/// Classifies an HTTP status: `Some` class for a failure, `None` for a success.
///
/// `context_overflow` tells whether the response body signals a context overflow. Statuses 429 and 5xx are transient;
/// status 400 with the overflow signal is a context overflow; the other 4xx are permanent. A status outside those ranges
/// is also permanent.
#[must_use]
pub const fn classify_status(status: u16, context_overflow: bool) -> Option<ErrorClass> {
    match status {
        200..=299 => None,
        429 | 500..=599 => Some(ErrorClass::Transient),
        400 if context_overflow => Some(ErrorClass::ContextOverflow),
        _ => Some(ErrorClass::Permanent),
    }
}

/// Scripted provider: each call consumes the next prepared result, and every request is recorded before answering.
/// It is shared between tasks through its internal lock.
#[derive(Debug)]
#[non_exhaustive]
pub struct ScriptedProvider {
    state: Mutex<ScriptState>,
}

/// State of a scripted provider: the results still to give and the requests received.
#[derive(Debug)]
struct ScriptState {
    script: VecDeque<Result<InferenceResponse, InferenceError>>,
    requests: Vec<InferenceRequest>,
}

impl ScriptedProvider {
    /// Builds a provider that gives `script` in order: each call takes the next result.
    #[must_use]
    pub fn new(script: Vec<Result<InferenceResponse, InferenceError>>) -> Self {
        Self {
            state: Mutex::new(ScriptState {
                script: VecDeque::from(script),
                requests: Vec::new(),
            }),
        }
    }

    /// Returns the requests received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<InferenceRequest> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requests
            .clone()
    }

    /// Records `request`, then takes the next prepared result. An exhausted script is a permanent error.
    fn next(&self, request: InferenceRequest) -> Result<InferenceResponse, InferenceError> {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.requests.push(request);
            state.script.pop_front()
        };
        next.unwrap_or_else(|| {
            Err(InferenceError::new(
                ErrorClass::Permanent,
                "script exhausted".to_owned(),
            ))
        })
    }
}

impl InferenceProvider for ScriptedProvider {
    fn complete(
        &self,
        request: InferenceRequest,
    ) -> impl Future<Output = Result<InferenceResponse, InferenceError>> + Send {
        std::future::ready(self.next(request))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        ErrorClass, InferenceError, InferenceProvider as _, InferenceRequest, InferenceResponse,
        ReportedUsage, ScriptedProvider, classify_status,
    };
    use crate::agents::{ModelProfile, ResolvedProfile};
    use crate::session::{Message, Reasoning};

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

    /// Completes one request tagged with `tag` on a shared provider.
    async fn complete_tagged(
        provider: Arc<ScriptedProvider>,
        tag: usize,
    ) -> Result<InferenceResponse, InferenceError> {
        provider.complete(request(&tag.to_string())).await
    }

    /// A request whose system prompt names `tag`, so that requests can be told apart.
    fn request(tag: &str) -> InferenceRequest {
        InferenceRequest::new(
            profile(),
            format!("system {tag}"),
            vec![Message::user(format!("hello {tag}"))],
            Vec::new(),
            vec![None],
        )
    }

    fn answer(text: &str) -> InferenceResponse {
        InferenceResponse::new(
            Message::assistant(text.to_owned(), None, Vec::new()),
            Some(ReportedUsage::new(10, 3)),
        )
    }

    fn transient(text: &str) -> InferenceError {
        InferenceError::new(ErrorClass::Transient, text.to_owned())
    }

    #[test]
    fn status_classification() {
        for status in [429, 500, 503] {
            assert_eq!(
                classify_status(status, false),
                Some(ErrorClass::Transient),
                "status {status} must be transient"
            );
        }
        for status in [400, 401, 403, 404, 418] {
            assert_eq!(
                classify_status(status, false),
                Some(ErrorClass::Permanent),
                "status {status} must be permanent"
            );
        }
        assert_eq!(classify_status(200, false), None, "a success is not an error");
        for status in [201, 204, 299] {
            assert_eq!(
                classify_status(status, false),
                None,
                "every 2xx status is a success, status {status} included"
            );
        }
    }

    #[test]
    fn context_overflow_is_its_own_class() {
        assert_eq!(
            classify_status(400, true),
            Some(ErrorClass::ContextOverflow),
            "a 400 whose body signals a context overflow must be ContextOverflow"
        );
    }

    #[tokio::test]
    async fn scripted_provider_replays_its_script() {
        let provider = ScriptedProvider::new(vec![
            Err(transient("first failure")),
            Err(transient("second failure")),
            Ok(answer("the answer")),
        ]);
        let first = provider.complete(request("a")).await;
        assert!(
            matches!(&first, Err(error) if error.class() == ErrorClass::Transient && error.message() == "first failure"),
            "the first call must return the first scripted error, got {first:?}"
        );
        let second = provider.complete(request("b")).await;
        assert!(
            matches!(&second, Err(error) if error.message() == "second failure"),
            "the second call must return the second scripted error, got {second:?}"
        );
        let third = provider.complete(request("c")).await;
        assert_eq!(
            third.map(|response| response.message().clone()).ok(),
            Some(Message::assistant("the answer", None, Vec::new())),
            "the third call must return the scripted response"
        );
    }

    #[tokio::test]
    async fn scripted_provider_records_requests() {
        let provider = ScriptedProvider::new(vec![Ok(answer("one")), Ok(answer("two"))]);
        let first = request("first");
        let second = request("second");
        let _first_answer = provider
            .complete(first.clone())
            .await
            .expect("the first scripted response must be given");
        let _second_answer = provider
            .complete(second.clone())
            .await
            .expect("the second scripted response must be given");
        assert_eq!(
            provider.requests(),
            vec![first, second],
            "requests must be recorded in the order they were received"
        );
    }

    #[tokio::test]
    async fn exhausted_script_is_a_permanent_error() {
        let provider = ScriptedProvider::new(Vec::new());
        let result = provider.complete(request("none")).await;
        assert!(
            matches!(&result, Err(error) if error.class() == ErrorClass::Permanent && error.message() == "script exhausted"),
            "an exhausted script must fail Permanent with the message `script exhausted`, got {result:?}"
        );
    }

    #[tokio::test]
    async fn shared_between_tasks() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(answer("r1")),
            Ok(answer("r2")),
            Ok(answer("r3")),
            Ok(answer("r4")),
        ]));
        let mut handles = Vec::new();
        for index in 0..4 {
            handles.push(tokio::spawn(complete_tagged(Arc::clone(&provider), index)));
        }
        let mut texts = Vec::new();
        for handle in handles {
            let result = handle.await.expect("a task must not panic");
            let response = result.expect("each task must get a scripted response");
            texts.push(response.message().clone());
        }
        assert_eq!(
            provider.requests().len(),
            4,
            "four requests must be recorded, one per task"
        );
        texts.sort_by_key(|message| format!("{message:?}"));
        texts.dedup();
        assert_eq!(
            texts.len(),
            4,
            "no scripted result may be used twice: four distinct responses expected"
        );
    }

    #[tokio::test]
    async fn poisoned_lock_is_recovered() {
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(answer("after the poison"))]));
        let poisoner = Arc::clone(&provider);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner
                .state
                .lock()
                .expect("the lock is not poisoned before the test poisons it");
            panic!("poisoning the script lock for the test");
        })
        .join();
        assert!(
            joined.is_err(),
            "the poisoning thread must panic while holding the lock"
        );
        let result = provider.complete(request("after")).await;
        assert!(
            matches!(&result, Ok(response) if response.message() == &Message::assistant("after the poison", None, Vec::new())),
            "a poisoned lock must be recovered and the script still answered, got {result:?}"
        );
        assert_eq!(
            provider.requests().len(),
            1,
            "the request received after the poisoning must be recorded"
        );
    }

    #[test]
    fn requests_and_responses_clone_and_compare() {
        let built = request("same");
        let copy = built.clone();
        assert_eq!(copy, built, "a cloned request must equal the original");
        assert_ne!(built, request("other"), "requests with other content must differ");
        let reasoned = InferenceResponse::new(
            Message::assistant("text", Some(Reasoning::new("vllm-strix", "thought")), Vec::new()),
            None,
        );
        let reasoned_copy = reasoned.clone();
        assert_eq!(
            reasoned_copy, reasoned,
            "a cloned response must equal the original"
        );
        assert_ne!(
            reasoned,
            answer("text"),
            "a response without the reasoning must differ"
        );
    }
}
