//! Inference adapter over `rig` for OpenAI-compatible servers (vLLM, llama.cpp, the `smart` gateway): implements
//! [`InferenceProvider`], decodes the reasoning the server returns and replays it, classifies errors without leaking the
//! address, the key or the body, and detects the models a server announces (`agent-sessions#CON-2`,
//! `agent-sessions/agents#FR-6`, `FR-7`, `agent-sessions/context-window#FR-9`, `agent-sessions#NFR-1`).
//!
//! Host warning: `rig` logs the whole request body at the `trace` level of `tracing`. The host must not enable it.

use std::fmt;
use std::sync::Arc;

use rig_core::completion::{CompletionRequest, Message as RigMessage, ToolDefinition};
use rig_core::error::{ErrorDetail, ErrorReport, MalformedToolInput, transient_transport};
use rig_core::http_client::{self, HttpClientExt as _, Method, NoBody, Request};
use rig_core::message::{AssistantContent, CallId, Issuer, Reasoning as RigReasoning, ToolName};
use rig_core::providers::openai::OpenAI;
use rig_core::providers::openai::wire::{LLAMACPP, OpenAIConfig};
use rig_core::streaming::{Item, Relayed, StreamEvent};
use serde_json::{Map, Value};

use super::{
    DetectedModel, ErrorClass, InferenceError, InferenceProvider, InferenceRequest, InferenceResponse,
    ReportedUsage, ResolvedProfile, classify_status,
};
use crate::session::{Message, Reasoning, ToolCall};

/// Name of the `rig` dialect used for every server. It is the emitter `rig` seals the replayed reasoning with: with any
/// other name `rig` silently drops the field.
const DIALECT: &str = "llamacpp";

/// Keys of the free table of a profile that `rig` would let overwrite the core of the request.
const RESERVED_KEYS: [&str; 7] = [
    "model",
    "messages",
    "tools",
    "tool_choice",
    "stream",
    "stream_options",
    "max_tokens",
];

/// Fields of a model entry that announce the context size, in order of preference.
const CONTEXT_KEYS: [&str; 4] = [
    "max_model_len",
    "context_length",
    "context_window",
    "max_context_length",
];

/// Fields of a model entry that announce the maximum output size, in order of preference.
const OUTPUT_KEYS: [&str; 2] = ["max_output_tokens", "max_completion_tokens"];

/// Fragments of a 400 or 413 body that signal a context overflow, compared in lower case.
const OVERFLOW_MARKERS: [&str; 4] = [
    "context_length_exceeded",
    "maximum context length",
    "exceeds the available context size",
    "reduce the length",
];

/// Address and key of an OpenAI-compatible server, already resolved by the host (plain text, never a reference).
///
/// The base address carries the version segment (`/v1`): a chat request goes to `POST <base>/chat/completions`. The key
/// is optional (a local server without authentication). The `Debug` output shows neither.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RigConfig {
    base_url: String,
    api_key: Option<String>,
}

impl RigConfig {
    /// Builds the configuration from the base address and the optional API key. An empty key is no key.
    #[must_use]
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            api_key: api_key.filter(|key| !key.is_empty()),
        }
    }

    /// Builds the `rig` client of the configuration. No network call happens here.
    fn client(&self) -> OpenAI {
        OpenAIConfig::with_key(&LLAMACPP, self.api_key.clone().unwrap_or_default())
            .with_base_url(self.base_url.clone())
            .client()
    }
}

impl fmt::Debug for RigConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RigConfig").finish_non_exhaustive()
    }
}

/// Inference provider for OpenAI-compatible servers, built on `rig` (chat completions API).
///
/// It sends no retry: the backoff belongs to the turn loop. The `Debug` output shows nothing of the configuration.
#[non_exhaustive]
pub struct RigProvider {
    client: Arc<OpenAI>,
}

impl RigProvider {
    /// Builds the provider and its `rig` client from `config`, without any network call.
    ///
    /// The host must have installed a rustls crypto provider beforehand: `reqwest` (`rustls-no-provider`) panics when
    /// it builds its client without one, even for an `http://` address.
    #[must_use]
    pub fn new(config: &RigConfig) -> Self {
        Self {
            client: Arc::new(config.client()),
        }
    }

    /// Runs one request through the stream of `rig`, hands each text fragment to `fragments`, and builds the response.
    async fn run<F: FnMut(&str) + Send>(
        &self,
        request: InferenceRequest,
        fragments: &mut F,
    ) -> Result<InferenceResponse, InferenceError> {
        let rig_request = build_request(&request)?;
        let model = self.client.chat(request.profile().model());
        let mut relay = model
            .stream(rig_request)
            .map_err(|error| classify_report(&ErrorReport::from(&error)))?
            .into_relay();
        let mut answer = Answer::default();
        let mut ended = false;
        while let Some(item) = std::future::poll_fn(|cx| relay.as_mut().poll_next(cx)).await {
            match item {
                Ok(Relayed::Item(Item::Event(event))) => answer.absorb(event, fragments),
                Ok(Relayed::Done(response)) => {
                    answer.usage = reported_usage(&response.usage);
                    ended = true;
                }
                Ok(Relayed::Item(_)) => {}
                Err(report) => match &report.detail {
                    Some(ErrorDetail::MalformedToolInput(bad)) => {
                        answer.push_malformed(bad);
                        ended = true;
                        break;
                    }
                    _ => return Err(classify_report(&report)),
                },
            }
        }
        if !ended {
            return Err(InferenceError::new(
                ErrorClass::Transient,
                "the response stream ended before its final event".to_owned(),
            ));
        }
        Ok(answer.into_response(request.profile().model()))
    }
}

impl fmt::Debug for RigProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RigProvider").finish_non_exhaustive()
    }
}

impl InferenceProvider for RigProvider {
    async fn complete(&self, request: InferenceRequest) -> Result<InferenceResponse, InferenceError> {
        self.run(request, &mut |_| {}).await
    }

    async fn stream<F>(
        &self,
        request: InferenceRequest,
        mut fragments: F,
    ) -> Result<InferenceResponse, InferenceError>
    where
        F: FnMut(&str) + Send,
    {
        self.run(request, &mut fragments).await
    }
}

/// What the stream delivered so far.
#[derive(Default)]
struct Answer {
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    usage: Option<ReportedUsage>,
}

impl Answer {
    /// Takes one event of the stream: text goes to `fragments` and to the text, reasoning only to the reasoning, a closed
    /// tool call to the calls.
    fn absorb<F: FnMut(&str)>(&mut self, event: StreamEvent, fragments: &mut F) {
        match event {
            StreamEvent::Text { text, .. } => {
                if !text.is_empty() {
                    fragments(&text);
                    self.text.push_str(&text);
                }
            }
            StreamEvent::Reasoning { text, .. } => self.reasoning.push_str(&text),
            StreamEvent::End {
                content: AssistantContent::ToolCall(call),
                ..
            } => self.calls.push(ToolCall::new(
                call.id.wire().into_owned(),
                call.function.name.as_str(),
                call.function.arguments,
            )),
            StreamEvent::Start { .. } | StreamEvent::Arguments { .. } | StreamEvent::End { .. } => {}
        }
    }

    /// Records a call whose arguments are not JSON: the arguments are the raw text as a JSON string, so the turn loop
    /// answers the model with an error instead of losing the call.
    fn push_malformed(&mut self, bad: &MalformedToolInput) {
        self.calls.push(ToolCall::new(
            bad.id.wire().into_owned(),
            bad.name.as_str(),
            Value::String(bad.raw.clone()),
        ));
    }

    /// Builds the response; the reasoning carries `model` as emitter.
    fn into_response(self, model: &str) -> InferenceResponse {
        let reasoning = (!self.reasoning.is_empty()).then(|| Reasoning::new(model, self.reasoning));
        InferenceResponse::new(Message::assistant(self.text, reasoning, self.calls), self.usage)
    }
}

/// The usage the server reported, if it reported any counter.
fn reported_usage(usage: &rig_core::completion::Usage) -> Option<ReportedUsage> {
    usage.is_reported().then(|| {
        ReportedUsage::new(
            usage.input_tokens.unwrap_or_default(),
            usage.output_tokens.unwrap_or_default(),
        )
    })
}

/// Builds the error of class `class` with `text`.
fn failure(class: ErrorClass, text: &str) -> InferenceError {
    InferenceError::new(class, text.to_owned())
}

/// Converts the request of the turn loop into a `rig` request.
fn build_request(request: &InferenceRequest) -> Result<CompletionRequest, InferenceError> {
    let profile = request.profile();
    let mut history = Vec::new();
    if !request.system_prompt().is_empty() {
        history.push(RigMessage::system(request.system_prompt()));
    }
    for (index, message) in request.messages().iter().enumerate() {
        let reasoning = request.reasoning().get(index).and_then(Option::as_ref);
        if let Some(converted) = convert_message(message, reasoning, profile.model())? {
            history.push(converted);
        }
    }
    let mut rig_request = CompletionRequest::new(RigMessage::user(String::new()));
    rig_request.chat_history = history;
    rig_request.tools = request
        .tools()
        .iter()
        .map(tool_definition)
        .collect::<Result<_, _>>()?;
    rig_request.max_tokens = Some(profile.max_response());
    rig_request.temperature = profile
        .sampling()
        .and_then(crate::agents::SamplingParams::temperature);
    rig_request.additional_params = extra_params(profile);
    Ok(rig_request)
}

/// Converts one message of the history. A boundary, or an assistant message with nothing to send, gives nothing.
fn convert_message(
    message: &Message,
    reasoning: Option<&Reasoning>,
    model: &str,
) -> Result<Option<RigMessage>, InferenceError> {
    Ok(match message {
        Message::System { content } => Some(RigMessage::system(content.as_str())),
        Message::User { content } => Some(RigMessage::user(content.as_str())),
        Message::Assistant {
            content, tool_calls, ..
        } => convert_assistant(content, tool_calls, reasoning, model)?,
        Message::ToolResult {
            call_id,
            tool,
            content,
            ..
        } => Some(RigMessage::tool_result(
            CallId::from_wire(call_id.as_str()),
            tool_name(tool)?,
            content.as_str(),
        )),
        Message::Boundary { .. } => None,
    })
}

/// Converts an assistant message. The reasoning is replayed only when `model` emitted it, sealed with the dialect name.
fn convert_assistant(
    content: &str,
    tool_calls: &[ToolCall],
    reasoning: Option<&Reasoning>,
    model: &str,
) -> Result<Option<RigMessage>, InferenceError> {
    let mut parts = Vec::new();
    if let Some(replayed) = reasoning.filter(|r| r.emitter() == model && !r.text().is_empty()) {
        parts.push(AssistantContent::Reasoning(
            RigReasoning::new(replayed.text()).sealed(Issuer::from(DIALECT)),
        ));
    }
    if !content.is_empty() {
        parts.push(AssistantContent::text(content));
    }
    for call in tool_calls {
        parts.push(AssistantContent::tool_call(
            call.id(),
            tool_name(call.name())?,
            call.arguments().clone(),
        ));
    }
    Ok((!parts.is_empty()).then_some(RigMessage::Assistant {
        id: None,
        content: parts,
    }))
}

/// Builds a `rig` tool name; an empty name is refused.
fn tool_name(name: &str) -> Result<ToolName, InferenceError> {
    ToolName::new(name)
        .map_err(|_empty| failure(ErrorClass::Permanent, "a tool name of the request is empty"))
}

/// Converts a tool definition (`name`, `description`, `parameters`).
fn tool_definition(definition: &Value) -> Result<ToolDefinition, InferenceError> {
    let name = definition
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| failure(ErrorClass::Permanent, "a tool definition has no name"))?;
    Ok(ToolDefinition {
        name: name.to_owned(),
        description: definition
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        parameters: definition
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
    })
}

/// The free parameters of the request: the typed sampling parameters and the free table of the profile, without the keys
/// that would overwrite the core of the request. A typed parameter wins over the same key of the table.
fn extra_params(profile: &ResolvedProfile) -> Option<Value> {
    let sampling = profile.sampling()?;
    let mut params = Map::new();
    for (key, value) in sampling.extra() {
        if !RESERVED_KEYS.contains(&key.as_str()) {
            let _previous = params.insert(key.clone(), value.clone());
        }
    }
    if let Some(top_p) = sampling.top_p() {
        let _previous = params.insert("top_p".to_owned(), Value::from(top_p));
    }
    if let Some(top_k) = sampling.top_k() {
        let _previous = params.insert("top_k".to_owned(), Value::from(top_k));
    }
    if sampling.temperature().is_some() {
        let _previous = params.remove("temperature");
    }
    (!params.is_empty()).then_some(Value::Object(params))
}

/// Tells whether an error body signals a context overflow.
fn signals_overflow(body: &str) -> bool {
    let lower = body.to_lowercase();
    OVERFLOW_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Classifies an HTTP failure by its status; the body only serves to detect a context overflow. The message names the
/// status or the class, never the address, the key or the body.
fn classify_http(status: u16, body: Option<&str>) -> InferenceError {
    let overflow = matches!(status, 400 | 413) && body.is_some_and(signals_overflow);
    let class = if overflow && status == 413 {
        ErrorClass::ContextOverflow
    } else {
        classify_status(status, overflow).unwrap_or(ErrorClass::Permanent)
    };
    let text = match class {
        ErrorClass::ContextOverflow => {
            format!("the request exceeds the model context (HTTP status {status})")
        }
        ErrorClass::Transient | ErrorClass::Permanent => {
            format!("the server answered with HTTP status {status}")
        }
    };
    InferenceError::new(class, text)
}

/// Classifies an error report of `rig`: by status when it has one, else by its retryable flag. A response that cannot be
/// decoded is permanent, with a message that names the usual cause.
fn classify_report(report: &ErrorReport) -> InferenceError {
    if let Some(status) = report.http_status {
        return classify_http(status, report.provider_response_body());
    }
    if matches!(
        report.kind,
        rig_core::ErrorKind::Json | rig_core::ErrorKind::Response
    ) {
        failure(
            ErrorClass::Permanent,
            "the server response could not be decoded: it does not follow the OpenAI chat completions format \
             (an empty `usage` object is a known cause)",
        )
    } else if report.retryable {
        failure(ErrorClass::Transient, "the connection to the server failed")
    } else {
        failure(ErrorClass::Permanent, "the request to the server failed")
    }
}

/// Classifies a failure of the HTTP client of `rig`.
fn classify_transport(error: &http_client::Error) -> InferenceError {
    if let Some(status) = error.non_success_status() {
        return classify_http(status.as_u16(), error.non_success_body());
    }
    if transient_transport(error) {
        failure(ErrorClass::Transient, "the connection to the server failed")
    } else {
        failure(ErrorClass::Permanent, "the request to the server failed")
    }
}

/// Asks the server which models it announces, with `GET <base>/models`.
///
/// For each model it reads the context size from `max_model_len` (vLLM), `context_length`, `context_window` or
/// `max_context_length`, and the maximum output size from `max_output_tokens` or `max_completion_tokens`. A field the
/// server does not announce stays absent.
///
/// The entries come in the order of the server, each with the model identifier.
///
/// # Errors
///
/// Returns an [`InferenceError`]: transient for a connection failure, a rate limit or a server error, permanent for the
/// other statuses and for an answer that is not a model list. The message never carries the address nor the key.
///
/// # Panics
///
/// Panics if the host has not installed a rustls crypto provider: `reqwest` (`rustls-no-provider`) refuses to build its
/// client without one, even for an `http://` address.
pub async fn detect_models(config: &RigConfig) -> Result<Vec<(String, DetectedModel)>, InferenceError> {
    let transport = config.client().chat("models").transport;
    let address = format!("{}/models", config.base_url.trim_end_matches('/'));
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(address)
        .header("accept", "application/json");
    if let Some(key) = &config.api_key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let request = builder
        .body(NoBody)
        .map_err(|_invalid| failure(ErrorClass::Permanent, "the server address is not valid"))?;
    let response = transport
        .send::<NoBody, Vec<u8>>(request)
        .await
        .map_err(|error| classify_transport(&error))?;
    let body = response
        .into_body()
        .await
        .map_err(|error| classify_transport(&error))?;
    parse_models(&body)
        .ok_or_else(|| failure(ErrorClass::Permanent, "the server answer is not a list of models"))
}

/// Reads the model list of a `/models` answer.
fn parse_models(body: &[u8]) -> Option<Vec<(String, DetectedModel)>> {
    let document: Value = serde_json::from_slice(body).ok()?;
    let entries = document.get("data")?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let id = entry.get("id")?.as_str()?;
                let context = first_number(entry, &CONTEXT_KEYS);
                let output = first_number(entry, &OUTPUT_KEYS).or_else(|| {
                    entry
                        .get("top_provider")
                        .and_then(|provider| provider.get("max_completion_tokens"))
                        .and_then(Value::as_u64)
                });
                Some((id.to_owned(), DetectedModel::new(context, output, None)))
            })
            .collect(),
    )
}

/// The first of `keys` that `entry` holds as a number.
fn first_number(entry: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| entry.get(*key).and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use super::{RigConfig, RigProvider, detect_models};
    use crate::agents::{
        DetectedModel, ErrorClass, InferenceError, InferenceProvider as _, InferenceRequest, ModelProfile,
        ResolvedProfile, SamplingParams,
    };
    use crate::session::{Message, Reasoning};

    /// What the fake server answers to every request.
    enum Reply {
        Http {
            status: u16,
            content_type: &'static str,
            body: String,
        },
        /// Closes the connection without answering.
        Drop,
    }

    /// A request the fake server received.
    #[derive(Debug, Clone)]
    struct Seen {
        request_line: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    impl Seen {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).expect("the request body is JSON")
        }
    }

    /// A fake OpenAI-compatible server on a local port.
    struct Fake {
        host: String,
        base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Fake {
        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }

        fn config(&self, key: Option<&str>) -> RigConfig {
            RigConfig::new(self.base.clone(), key.map(str::to_owned))
        }
    }

    /// Installs the TLS provider that the host owns in production: `reqwest` refuses to build a client without one. An
    /// error means one is already installed.
    fn install_tls_provider() {
        let _installed = rustls::crypto::ring::default_provider().install_default();
    }

    /// Starts a fake server that gives `reply` to every request and records them.
    async fn fake(reply: Reply) -> Fake {
        install_tls_provider();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port can be bound");
        let addr = listener.local_addr().expect("a bound listener has an address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let reply = Arc::new(reply);
        let _acceptor = tokio::spawn(async move {
            while let Ok((stream, _peer)) = listener.accept().await {
                let _connection = tokio::spawn(serve(stream, Arc::clone(&reply), Arc::clone(&recorded)));
            }
        });
        Fake {
            host: addr.to_string(),
            base: format!("http://{addr}/v1"),
            seen,
        }
    }

    /// Reads one HTTP request, records it, and answers.
    async fn serve(mut stream: TcpStream, reply: Arc<Reply>, seen: Arc<Mutex<Vec<Seen>>>) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_owned();
        let mut headers = BTreeMap::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let _previous = headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut body = buffer[head_end.saturating_add(4)..].to_vec();
        while body.len() < length {
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        seen.lock().unwrap_or_else(PoisonError::into_inner).push(Seen {
            request_line,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        match &*reply {
            Reply::Drop => {}
            Reply::Http {
                status,
                content_type,
                body: payload,
            } => {
                let answer = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _written = stream.write_all(answer.as_bytes()).await;
                let _closed = stream.shutdown().await;
            }
        }
    }

    /// A server-sent events body: the chunks, then the end marker.
    fn sse(chunks: &[Value]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.extend(format!("data: {chunk}\n\n").chars());
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn stream_reply(chunks: &[Value]) -> Reply {
        Reply::Http {
            status: 200,
            content_type: "text/event-stream",
            body: sse(chunks),
        }
    }

    fn json_reply(status: u16, body: &Value) -> Reply {
        Reply::Http {
            status,
            content_type: "application/json",
            body: body.to_string(),
        }
    }

    fn delta(delta: &Value) -> Value {
        json!({"id": "c1", "choices": [{"index": 0, "delta": delta}]})
    }

    fn usage_chunk() -> Value {
        json!({"id": "c1", "choices": [], "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}})
    }

    fn profile(sampling: Option<SamplingParams>) -> ResolvedProfile {
        let mut model = ModelProfile::new("p".to_owned(), "prov".to_owned(), "qwen-test".to_owned())
            .with_context_size(32_768);
        if let Some(sampling) = sampling {
            model = model.with_sampling(sampling);
        }
        model
            .resolve(None)
            .expect("a profile with a context size resolves")
    }

    fn simple_request() -> InferenceRequest {
        InferenceRequest::new(
            profile(None),
            "system".to_owned(),
            vec![Message::user("salut")],
            Vec::new(),
            vec![None],
        )
    }

    async fn complete(
        fake: &Fake,
        request: InferenceRequest,
    ) -> Result<crate::agents::InferenceResponse, InferenceError> {
        RigProvider::new(&fake.config(None)).complete(request).await
    }

    fn assistant(
        response: &crate::agents::InferenceResponse,
    ) -> (String, Option<Reasoning>, Vec<crate::session::ToolCall>) {
        let Message::Assistant {
            content,
            reasoning,
            tool_calls,
        } = response.message()
        else {
            panic!("an assistant message was expected");
        };
        (content.clone(), reasoning.clone(), tool_calls.clone())
    }

    // Scenario: une réponse simple est décodée
    #[tokio::test]
    async fn simple_answer_is_decoded() {
        let server = fake(stream_reply(&[
            delta(&json!({"role": "assistant", "reasoning": "je reflechis"})),
            delta(&json!({"content": "Bonjour"})),
            usage_chunk(),
        ]))
        .await;
        let response = complete(&server, simple_request())
            .await
            .expect("the answer is decoded");
        let (text, reasoning, calls) = assistant(&response);
        assert_eq!(text, "Bonjour", "the reasoning must not be in the text");
        let reasoning = reasoning.expect("the reasoning is decoded");
        assert_eq!(reasoning.text(), "je reflechis", "the reasoning text");
        assert_eq!(
            reasoning.emitter(),
            "qwen-test",
            "the emitter is the model of the profile"
        );
        assert!(calls.is_empty(), "no tool call");
        let usage = response.usage().expect("the usage is reported");
        assert_eq!(
            (usage.prompt(), usage.completion()),
            (11, 7),
            "the reported usage"
        );
        let seen = server.seen();
        assert_eq!(
            seen.first().map(|request| request.request_line.as_str()),
            Some("POST /v1/chat/completions HTTP/1.1"),
            "the request goes to <base>/chat/completions"
        );
        assert!(
            seen.iter()
                .all(|request| !request.headers.contains_key("authorization")),
            "no key, no Authorization header"
        );
    }

    // Scenario: une réponse simple est décodée (la clé, quand il y en a une, part en Bearer)
    #[tokio::test]
    async fn key_is_sent_as_bearer() {
        let server = fake(stream_reply(&[delta(&json!({"content": "ok"}))])).await;
        let provider = RigProvider::new(&server.config(Some("sk-secret")));
        let _response = provider
            .complete(simple_request())
            .await
            .expect("the answer is decoded");
        let seen = server.seen();
        assert_eq!(
            seen.first()
                .and_then(|request| request.headers.get("authorization"))
                .map(String::as_str),
            Some("Bearer sk-secret"),
            "the key is sent as a bearer token"
        );
    }

    // Scenario: la passerelle renvoie reasoning_content
    #[tokio::test]
    async fn gateway_reasoning_content_is_decoded() {
        let server = fake(stream_reply(&[
            delta(&json!({"role": "assistant", "reasoning_content": "je reflechis"})),
            delta(&json!({"content": "Bonjour"})),
        ]))
        .await;
        let response = complete(&server, simple_request())
            .await
            .expect("the answer is decoded");
        let (text, reasoning, _calls) = assistant(&response);
        assert_eq!(text, "Bonjour", "the reasoning must not be in the text");
        assert_eq!(
            reasoning.map(|r| r.text().to_owned()).as_deref(),
            Some("je reflechis"),
            "reasoning_content is decoded like reasoning"
        );
        assert!(response.usage().is_none(), "no usage reported, none invented");
    }

    fn history_request(reasoning: Option<Reasoning>) -> InferenceRequest {
        InferenceRequest::new(
            profile(None),
            String::new(),
            vec![
                Message::user("q1"),
                Message::assistant("r1", None, Vec::new()),
                Message::user("q2"),
            ],
            Vec::new(),
            vec![None, reasoning, None],
        )
    }

    // Scenario: le rejeu du raisonnement suit le mode du profil
    #[tokio::test]
    async fn replay_follows_the_profile_mode() {
        let server = fake(stream_reply(&[delta(&json!({"content": "ok"}))])).await;
        let replayed = Reasoning::new("qwen-test", "ma pensee");
        let _first = complete(&server, history_request(Some(replayed)))
            .await
            .expect("the replay request succeeds");
        let _second = complete(&server, history_request(None))
            .await
            .expect("the request without replay succeeds");
        let seen = server.seen();
        let first = seen.first().expect("the first request was recorded").json();
        let second = seen.get(1).expect("the second request was recorded").json();
        assert_eq!(
            first["messages"][1]["reasoning_content"],
            json!("ma pensee"),
            "the replayed reasoning is sent under reasoning_content: {first}"
        );
        assert!(
            second["messages"][1].get("reasoning_content").is_none(),
            "without replay the reasoning is absent: {second}"
        );
    }

    // Scenario: le rejeu du raisonnement suit le mode du profil (uniquement pour le modèle qui l'a émis)
    #[tokio::test]
    async fn reasoning_of_another_model_is_not_replayed() {
        let server = fake(stream_reply(&[delta(&json!({"content": "ok"}))])).await;
        let _response = complete(
            &server,
            history_request(Some(Reasoning::new("other-model", "pensee etrangere"))),
        )
        .await
        .expect("the request succeeds");
        let body = server
            .seen()
            .first()
            .expect("a request was recorded")
            .body
            .clone();
        assert!(
            !body.contains("pensee etrangere"),
            "the reasoning of another model must not be sent: {body}"
        );
    }

    // Must: la réponse maximale du profil est envoyée comme max_tokens, et l'emporte sur la table libre
    #[tokio::test]
    async fn profile_max_response_is_sent_as_max_tokens() {
        let sampling = SamplingParams::default().with_extra("max_tokens".to_owned(), json!(99));
        let request = InferenceRequest::new(
            profile(Some(sampling)),
            String::new(),
            vec![Message::user("q")],
            Vec::new(),
            vec![None],
        );
        let expected = request.profile().max_response();
        let server = fake(stream_reply(&[delta(&json!({"content": "ok"}))])).await;
        let _response = complete(&server, request).await.expect("the request succeeds");
        let body = server.seen().first().expect("a request was recorded").json();
        assert_eq!(
            body["max_tokens"],
            json!(expected),
            "the profile value wins: {body}"
        );
    }

    // Scenario: la table libre du profil est fusionnée sans écraser le noyau
    #[tokio::test]
    async fn free_table_is_merged_without_overwriting_the_core() {
        let sampling = SamplingParams::default()
            .with_temperature(0.3)
            .with_top_p(0.9)
            .with_top_k(20)
            .with_extra(
                "chat_template_kwargs".to_owned(),
                json!({"preserve_thinking": true}),
            )
            .with_extra(
                "messages".to_owned(),
                json!([{"role": "user", "content": "PIRATE"}]),
            )
            .with_extra("model".to_owned(), json!("PIRATE"))
            .with_extra("tools".to_owned(), json!([]))
            .with_extra("tool_choice".to_owned(), json!("none"))
            .with_extra("stream".to_owned(), json!(false))
            .with_extra("stream_options".to_owned(), json!({"include_usage": false}))
            .with_extra("temperature".to_owned(), json!(0.99));
        let request = InferenceRequest::new(
            profile(Some(sampling)),
            String::new(),
            vec![Message::user("vrai prompt")],
            Vec::new(),
            vec![None],
        );
        let server = fake(stream_reply(&[delta(&json!({"content": "ok"}))])).await;
        let _response = complete(&server, request).await.expect("the request succeeds");
        let body = server.seen().first().expect("a request was recorded").json();
        assert_eq!(
            body["chat_template_kwargs"],
            json!({"preserve_thinking": true}),
            "chat_template_kwargs is flat in the body: {body}"
        );
        assert_eq!(
            body["model"],
            json!("qwen-test"),
            "the model is the profile's: {body}"
        );
        assert_eq!(
            body["messages"][0]["content"],
            json!("vrai prompt"),
            "the messages are the request's: {body}"
        );
        assert_eq!(body["stream"], json!(true), "the request streams: {body}");
        assert_eq!(
            body["stream_options"]["include_usage"],
            json!(true),
            "usage asked: {body}"
        );
        assert!(
            body.get("tool_choice").is_none(),
            "tool_choice from the table is dropped: {body}"
        );
        assert_eq!(
            body["temperature"],
            json!(0.3),
            "the typed temperature wins: {body}"
        );
        assert_eq!(body["top_p"], json!(0.9), "top_p is sent: {body}");
        assert_eq!(body["top_k"], json!(20), "top_k is sent: {body}");
    }

    // Scenario: les appels d'outil sont décodés
    #[tokio::test]
    async fn tool_calls_are_decoded() {
        let server = fake(stream_reply(&[
            delta(&json!({"content": "voyons"})),
            delta(
                &json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                "function": {"name": "rhai_pad", "arguments": "{\"script\":"}}]}),
            ),
            delta(&json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"1\"}"}}]})),
            delta(
                &json!({"tool_calls": [{"index": 1, "id": "call_2", "type": "function",
                "function": {"name": "skill", "arguments": "{pas json"}}]}),
            ),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
            usage_chunk(),
        ]))
        .await;
        let response = complete(&server, simple_request())
            .await
            .expect("an invalid argument must not lose the response");
        let (text, _reasoning, calls) = assistant(&response);
        assert_eq!(text, "voyons", "the text before the calls is kept");
        assert_eq!(calls.len(), 2, "both calls are rendered: {calls:?}");
        let first = calls.first().expect("the first call");
        assert_eq!(
            (first.id(), first.name()),
            ("call_1", "rhai_pad"),
            "the first call identity"
        );
        assert_eq!(
            first.arguments(),
            &json!({"script": "1"}),
            "the first call is decoded"
        );
        let second = calls.get(1).expect("the second call");
        assert_eq!(
            (second.id(), second.name()),
            ("call_2", "skill"),
            "the faulty call identity"
        );
        assert_eq!(
            second.arguments(),
            &json!("{pas json"),
            "the faulty call carries its raw text as a string, which no tool accepts"
        );
    }

    // Scenario: le flux transmet les fragments dans l'ordre
    #[tokio::test]
    async fn stream_forwards_fragments_in_order() {
        let server = fake(stream_reply(&[
            delta(&json!({"reasoning": "pen"})),
            delta(&json!({"content": "Bon"})),
            delta(&json!({"reasoning_content": "see"})),
            delta(&json!({"content": "jour"})),
            usage_chunk(),
        ]))
        .await;
        let mut received = Vec::new();
        let response = RigProvider::new(&server.config(None))
            .stream(simple_request(), |fragment| received.push(fragment.to_owned()))
            .await
            .expect("the stream succeeds");
        assert_eq!(received, ["Bon", "jour"], "only the text fragments, in order");
        let (text, reasoning, _calls) = assistant(&response);
        assert_eq!(text, "Bonjour", "the final response holds the whole text");
        assert_eq!(
            reasoning.map(|r| r.text().to_owned()).as_deref(),
            Some("pensee"),
            "the reasoning is in the final response only"
        );
        assert_eq!(
            response.usage().map(|usage| (usage.prompt(), usage.completion())),
            Some((11, 7)),
            "the final usage"
        );
    }

    /// Asserts that neither the message nor the `Debug` output of `error` shows one of `secrets`.
    fn assert_no_leak(error: &InferenceError, secrets: &[&str]) {
        for secret in secrets {
            assert!(
                !error.message().contains(secret) && !format!("{error:?}").contains(secret),
                "the error leaks `{secret}`: {error:?}"
            );
        }
    }

    /// Asserts that `result` is an error of class `class` whose message leaks none of `secrets`.
    fn assert_failure(
        result: Result<crate::agents::InferenceResponse, InferenceError>,
        class: ErrorClass,
        secrets: &[&str],
        what: &str,
    ) {
        let error = result.expect_err(what);
        assert_eq!(
            error.class(),
            class,
            "{what}: class, message was {:?}",
            error.message()
        );
        assert_no_leak(&error, secrets);
    }

    // Scenario: les erreurs sont classées sans fuite
    #[tokio::test]
    async fn errors_are_classified_without_leak() {
        let overflow = json!({"error": {"message": "This model's maximum context length is 8192 tokens s3cr3t-body",
            "type": "BadRequestError", "code": "context_length_exceeded"}});
        let cases = [
            (
                json_reply(429, &json!({"error": "s3cr3t-body"})),
                ErrorClass::Transient,
                "429",
            ),
            (
                json_reply(500, &json!({"error": "s3cr3t-body"})),
                ErrorClass::Transient,
                "500",
            ),
            (
                json_reply(401, &json!({"error": "s3cr3t-body"})),
                ErrorClass::Permanent,
                "401",
            ),
            (
                json_reply(400, &json!({"error": "s3cr3t-body"})),
                ErrorClass::Permanent,
                "plain 400",
            ),
            (
                json_reply(400, &overflow),
                ErrorClass::ContextOverflow,
                "400 overflow",
            ),
            (
                json_reply(413, &overflow),
                ErrorClass::ContextOverflow,
                "413 overflow",
            ),
            (Reply::Drop, ErrorClass::Transient, "cut connection"),
        ];
        for (reply, class, what) in cases {
            let server = fake(reply).await;
            let provider = RigProvider::new(&server.config(Some("sk-secret")));
            let result = provider.complete(simple_request()).await;
            assert_failure(result, class, &["sk-secret", &server.host, "s3cr3t-body"], what);
        }
    }

    // Scenario: les erreurs sont classées sans fuite (flux coupé avant l'événement final)
    #[tokio::test]
    async fn stream_ended_without_final_event_is_transient() {
        let body = format!("data: {}\n\n", delta(&json!({"content": "Bon"})));
        let server = fake(Reply::Http {
            status: 200,
            content_type: "text/event-stream",
            body,
        })
        .await;
        let result = RigProvider::new(&server.config(Some("sk-secret")))
            .complete(simple_request())
            .await;
        assert_failure(
            result,
            ErrorClass::Transient,
            &["sk-secret", &server.host],
            "a stream cut before its final event",
        );
    }

    // Scenario: les erreurs sont classées sans fuite (connexion refusée)
    #[tokio::test]
    async fn refused_connection_is_transient_without_leak() {
        install_tls_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a local port");
        let addr = listener.local_addr().expect("an address");
        drop(listener);
        let config = RigConfig::new(format!("http://{addr}/v1"), Some("sk-secret".to_owned()));
        let result = RigProvider::new(&config).complete(simple_request()).await;
        assert_failure(
            result,
            ErrorClass::Transient,
            &["sk-secret", &addr.to_string()],
            "refused connection",
        );
    }

    // Scenario: les erreurs sont classées sans fuite (corps illisible)
    #[tokio::test]
    async fn unreadable_body_is_permanent_without_leak() {
        let not_sse = Reply::Http {
            status: 200,
            content_type: "text/event-stream",
            body: "data: {not json s3cr3t-body\n\n".to_owned(),
        };
        let server = fake(not_sse).await;
        let result = complete(&server, simple_request()).await;
        assert_failure(
            result,
            ErrorClass::Permanent,
            &[&server.host, "s3cr3t-body"],
            "unreadable body",
        );
    }

    // Scenario: les erreurs sont classées sans fuite (usage vide)
    #[tokio::test]
    async fn empty_usage_object_is_a_clear_permanent_error() {
        let server = fake(stream_reply(&[
            delta(&json!({"content": "ok"})),
            json!({"id": "c1", "choices": [], "usage": {}}),
        ]))
        .await;
        let error = complete(&server, simple_request())
            .await
            .expect_err("an empty usage object cannot be decoded");
        assert_eq!(
            error.class(),
            ErrorClass::Permanent,
            "an empty usage is permanent"
        );
        assert!(
            error.message().contains("usage"),
            "the message names the usual cause: {:?}",
            error.message()
        );
    }

    // Scenario: la détection reporte la taille de contexte du serveur
    #[tokio::test]
    async fn detection_reports_the_server_context_size() {
        let models = json!({"object": "list", "data": [
            {"id": "vllm-model", "object": "model", "max_model_len": 262_144},
            {"id": "sans-info", "object": "model"},
            {"id": "gateway", "object": "model", "context_length": 8192, "max_completion_tokens": 2048},
            {"id": "window", "context_window": 4096},
            {"id": "maxctx", "max_context_length": 1024}]});
        let server = fake(json_reply(200, &models)).await;
        let detected = detect_models(&server.config(Some("sk-secret")))
            .await
            .expect("the models are detected");
        assert_eq!(
            detected,
            vec![
                (
                    "vllm-model".to_owned(),
                    DetectedModel::new(Some(262_144), None, None)
                ),
                ("sans-info".to_owned(), DetectedModel::new(None, None, None)),
                (
                    "gateway".to_owned(),
                    DetectedModel::new(Some(8192), Some(2048), None)
                ),
                ("window".to_owned(), DetectedModel::new(Some(4096), None, None)),
                ("maxctx".to_owned(), DetectedModel::new(Some(1024), None, None)),
            ],
            "one entry per announced model"
        );
        let seen = server.seen();
        let request = seen.first().expect("a request was recorded");
        assert_eq!(
            request.request_line, "GET /v1/models HTTP/1.1",
            "the model list request"
        );
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer sk-secret"),
            "the key is sent"
        );
    }

    // Scenario: la détection reporte la taille de contexte du serveur (échecs)
    #[tokio::test]
    async fn detection_failures_are_classified_without_leak() {
        let cases = [
            (
                json_reply(503, &json!({"e": "s3cr3t-body"})),
                ErrorClass::Transient,
            ),
            (
                json_reply(401, &json!({"e": "s3cr3t-body"})),
                ErrorClass::Permanent,
            ),
            (
                json_reply(200, &json!({"unexpected": "s3cr3t-body"})),
                ErrorClass::Permanent,
            ),
            // The status and the body reach the classification through the error of the HTTP client (`non_success_*`).
            (
                json_reply(
                    413,
                    &json!({"error": {"message": "maximum context length s3cr3t-body", "code": "context_length_exceeded"}}),
                ),
                ErrorClass::ContextOverflow,
            ),
            (Reply::Drop, ErrorClass::Transient),
        ];
        for (reply, class) in cases {
            let server = fake(reply).await;
            let error = detect_models(&server.config(Some("sk-secret")))
                .await
                .expect_err("the detection fails");
            assert_eq!(error.class(), class, "class of {:?}", error.message());
            assert_no_leak(&error, &["sk-secret", server.host.as_str(), "s3cr3t-body"]);
        }
    }

    // Must: Debug opaque
    #[test]
    fn debug_shows_neither_address_nor_key() {
        install_tls_provider();
        let config = RigConfig::new("http://secret-host/v1".to_owned(), Some("sk-secret".to_owned()));
        let provider = RigProvider::new(&config);
        for shown in [format!("{config:?}"), format!("{provider:?}")] {
            assert!(
                !shown.contains("secret"),
                "the Debug output leaks the configuration: {shown}"
            );
        }
    }
}
