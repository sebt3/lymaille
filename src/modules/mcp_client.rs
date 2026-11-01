//! Real MCP connector: reaches a remote server over streamable HTTP through `rmcp`, and implements [`McpConnector`] for a
//! URL and for authentication headers that the host resolves (`agent-sessions/modules#FR-6`, `agent-sessions/modules#FR-9`,
//! `agent-sessions#CON-5`). Everything here exists only with the `mcp-http` Cargo feature, and `rmcp` appears nowhere else.

use std::collections::{BTreeMap, HashMap};
use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use http::{HeaderName, HeaderValue};
use rmcp::ServiceExt as _;
use rmcp::handler::client::ClientHandler;
#[expect(
    deprecated,
    reason = "logging messages are still a server notification that watchers can observe"
)]
use rmcp::model::LoggingMessageNotificationParam;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, CustomNotification, ResourceUpdatedNotificationParam,
    TaskStatusNotificationParams, Tool,
};
use rmcp::service::ClientInitializeError;
use rmcp::service::NotificationContext;
use rmcp::service::{Peer, RoleClient, RunningService, ServiceError};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpClientTransportConfig,
};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

use super::mcp::{McpConnector, McpError, McpLister, McpNotification, McpTool};

/// Authentication headers of a server, as the host resolves them. The values are opaque and never shown.
#[derive(Clone, Default)]
pub struct McpHeaders {
    bearer: Option<String>,
    extra: BTreeMap<String, String>,
}

impl McpHeaders {
    /// Builds an empty set of headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a bearer token, given without the `Bearer` prefix.
    #[must_use]
    pub fn with_bearer(mut self, token: &str) -> Self {
        self.bearer = Some(token.to_owned());
        self
    }

    /// Adds a header with its name and its value.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        let _previous = self.extra.insert(name.to_owned(), value.to_owned());
        self
    }

    /// Converts the custom headers for the transport. An invalid header is refused without showing its value.
    fn custom_headers(&self) -> Result<HashMap<HeaderName, HeaderValue>, McpError> {
        let mut headers = HashMap::new();
        for (name, value) in &self.extra {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_error| McpError::Failed(format!("invalid header name `{name}`")))?;
            let mut header_value = HeaderValue::from_str(value)
                .map_err(|_error| McpError::Failed(format!("invalid value for the header `{name}`")))?;
            header_value.set_sensitive(true);
            let _previous = headers.insert(header_name, header_value);
        }
        Ok(headers)
    }
}

impl fmt::Debug for McpHeaders {
    // Only the names and the presence of a token: a value never shows (`agent-sessions#CON-3`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHeaders")
            .field("bearer", &self.bearer.is_some())
            .field("headers", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Source of the authentication headers, implemented by the host. It is called at each connection and at each
/// reconnection, so that a renewed token is taken into account.
pub trait McpAuth: Send + Sync {
    /// Returns the current headers.
    ///
    /// # Errors
    ///
    /// Returns the [`McpError`] of the host when it cannot resolve the headers; the connector gives it back as is.
    fn headers(&self) -> impl Future<Output = Result<McpHeaders, McpError>> + Send;
}

/// Configuration of a [`RmcpConnector`]: the URL of the server and the time limits.
#[derive(Clone)]
#[non_exhaustive]
pub struct McpClientConfig {
    url: String,
    request_timeout: Duration,
    connect_timeout: Duration,
}

impl McpClientConfig {
    /// Builds a configuration for the server at `url`, with 30 s per request and 10 s to connect.
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
        }
    }

    /// Sets the time limit of each request.
    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Sets the time limit of a connection.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

impl fmt::Debug for McpClientConfig {
    // Not the URL: it may carry a token in its query.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpClientConfig")
            .field("request_timeout", &self.request_timeout)
            .field("connect_timeout", &self.connect_timeout)
            .finish_non_exhaustive()
    }
}

/// Connector to a remote MCP server over streamable HTTP. The connection is lazy: it is made by the first listing or call,
/// and a single session serves the concurrent calls.
pub struct RmcpConnector<A> {
    config: McpClientConfig,
    auth: Arc<A>,
    session: Mutex<Option<RunningService<RoleClient, NotificationRelay>>>,
    relay: mpsc::Sender<McpNotification>,
    notifications: Mutex<mpsc::Receiver<McpNotification>>,
}

/// Number of server notifications kept while the host does not read them; beyond it the newest ones are dropped.
const NOTIFICATION_CAPACITY: usize = 256;

/// Receives the notifications of the server inside `rmcp` and queues them for [`McpConnector::next_notification`].
/// Protocol bookkeeping of the client's own requests (cancellation, progress, subscription acknowledgement) is not an event
/// for the watchers and is not forwarded. Only what the server sent is queued: no request, no header, no URL.
struct NotificationRelay {
    sender: mpsc::Sender<McpNotification>,
}

impl NotificationRelay {
    /// Queues a notification of `method` with `params` (`null` when it has none or when they cannot be read). A full queue
    /// drops it: the handler of `rmcp` must not wait for the host.
    fn forward(&self, context: &NotificationContext<RoleClient>, method: &str, params: Value) {
        let server = context
            .peer
            .peer_info()
            .and_then(|info| info.server_info.as_ref().map(|server| server.name.clone()))
            .unwrap_or_else(|| "unknown".to_owned());
        let _queued = self
            .sender
            .try_send(McpNotification::new(&server, method, params));
    }
}

impl ClientHandler for NotificationRelay {
    #[expect(
        deprecated,
        reason = "logging messages are a server notification that watchers can still observe"
    )]
    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        context: NotificationContext<RoleClient>,
    ) {
        self.forward(
            &context,
            "notifications/message",
            serde_json::to_value(&params).unwrap_or(Value::Null),
        );
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        context: NotificationContext<RoleClient>,
    ) {
        self.forward(
            &context,
            "notifications/resources/updated",
            serde_json::to_value(&params).unwrap_or(Value::Null),
        );
    }

    async fn on_resource_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.forward(&context, "notifications/resources/list_changed", Value::Null);
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.forward(&context, "notifications/tools/list_changed", Value::Null);
    }

    async fn on_prompt_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.forward(&context, "notifications/prompts/list_changed", Value::Null);
    }

    async fn on_task_status(
        &self,
        params: TaskStatusNotificationParams,
        context: NotificationContext<RoleClient>,
    ) {
        self.forward(
            &context,
            "notifications/tasks",
            serde_json::to_value(&params).unwrap_or(Value::Null),
        );
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        context: NotificationContext<RoleClient>,
    ) {
        self.forward(
            &context,
            &notification.method,
            notification.params.unwrap_or(Value::Null),
        );
    }
}

impl<A: McpAuth> RmcpConnector<A> {
    /// Builds a connector for the server of `config`. It does not connect.
    ///
    /// The host MUST have installed a rustls crypto provider (for instance
    /// `rustls::crypto::ring::default_provider().install_default()`) before the first listing or call: the library does not
    /// choose one, and `rmcp` panics when it builds its HTTP client without one.
    #[must_use]
    pub fn new(config: McpClientConfig, auth: Arc<A>) -> Self {
        let (relay, receiver) = mpsc::channel(NOTIFICATION_CAPACITY);
        Self {
            config,
            auth,
            session: Mutex::new(None),
            relay,
            notifications: Mutex::new(receiver),
        }
    }

    /// Returns the peer of the current session, opening the session first when there is none.
    async fn peer(&self) -> Result<Peer<RoleClient>, McpError> {
        let mut session = self.session.lock().await;
        if let Some(open) = session.as_ref()
            && !open.is_closed()
        {
            return Ok(open.peer().clone());
        }
        let headers = self.auth.headers().await?;
        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(self.config.url.as_str())
            .custom_headers(headers.custom_headers()?);
        if let Some(token) = &headers.bearer {
            transport_config = transport_config.auth_header(token.as_str());
        }
        let transport = StreamableHttpClientTransport::from_config(transport_config);
        let opened = timeout(
            self.config.connect_timeout,
            NotificationRelay {
                sender: self.relay.clone(),
            }
            .serve(transport),
        )
        .await
        .map_err(|_elapsed| McpError::Unreachable("the connection timed out".to_owned()))?
        .map_err(|error| initialize_error(&error))?;
        let peer = opened.peer().clone();
        *session = Some(opened);
        drop(session);
        Ok(peer)
    }

    /// Bounds a request by the request time limit.
    async fn bounded<T>(
        &self,
        request: impl Future<Output = Result<T, ServiceError>>,
    ) -> Result<T, McpError> {
        timeout(self.config.request_timeout, request)
            .await
            .map_err(|_elapsed| McpError::Unreachable("the request timed out".to_owned()))?
            .map_err(|error| service_error(&error))
    }
}

impl<A> fmt::Debug for RmcpConnector<A> {
    // Neither the URL nor the headers: they may be secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RmcpConnector")
            .field("request_timeout", &self.config.request_timeout)
            .field("connect_timeout", &self.config.connect_timeout)
            .finish_non_exhaustive()
    }
}

/// Converts a tool of `rmcp`.
fn mcp_tool(tool: &Tool) -> McpTool {
    McpTool::new(
        &tool.name,
        tool.description.as_deref().unwrap_or(""),
        Value::Object((*tool.input_schema).clone()),
    )
}

/// Classifies an error of the transport: by type when `rmcp` lets us name it, else by its text.
fn classify(error: &(dyn StdError + 'static)) -> McpError {
    let mut current = Some(error);
    while let Some(cause) = current {
        if cause.downcast_ref::<AuthRequiredError>().is_some() {
            return McpError::Failed("the server requires authentication (HTTP 401)".to_owned());
        }
        if cause.downcast_ref::<InsufficientScopeError>().is_some() {
            return McpError::Failed("the server refuses the scope of the token (HTTP 403)".to_owned());
        }
        current = cause.source();
    }
    classify_text(&error.to_string())
}

/// The three digits that follow `marker` in `text`, if any.
fn status_after(text: &str, marker: &str) -> Option<String> {
    let (_, rest) = text.split_once(marker)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).take(3).collect();
    (digits.len() == 3).then_some(digits)
}

/// Fallback of [`classify`]: `HttpStatusError` is not reachable through the `source` chain of the transport error (only its
/// text is), and the errors that carry a `reqwest::Error` cannot be named without depending on `reqwest`, so the text is
/// read. The texts are those of `rmcp` 3.8.0 (`unexpected server response: HTTP 403 Forbidden: ...` for a refusal,
/// `... error: HTTP 503` for a 429 or a 5xx); a test per case warns when `rmcp` changes them. Any other text is never
/// rendered: it carries the URL of the server, which may hold a token. The body of the answer is never repeated.
fn classify_text(text: &str) -> McpError {
    if let Some(status) = status_after(text, "unexpected server response: HTTP ")
        && status.starts_with('4')
    {
        return McpError::Failed(format!("the server refused the request (HTTP {status})"));
    }
    McpError::Unreachable(status_after(text, "error: HTTP ").map_or_else(
        || "the transport to the server failed".to_owned(),
        |status| format!("the server answered HTTP {status}"),
    ))
}

/// Converts the error of the handshake.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the error enums of `rmcp` are non-exhaustive: any other error is an unusable answer"
)]
fn initialize_error(error: &ClientInitializeError) -> McpError {
    match error {
        ClientInitializeError::TransportError { error: transport, .. } => classify(transport),
        ClientInitializeError::ConnectionClosed(_) => {
            McpError::Unreachable("the server closed the connection during the handshake".to_owned())
        }
        _ => McpError::Failed("the server answered the handshake with an unusable result".to_owned()),
    }
}

/// Converts the error of a request.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the error enums of `rmcp` are non-exhaustive: any other error is an unusable answer"
)]
fn service_error(error: &ServiceError) -> McpError {
    match error {
        ServiceError::McpError(data) => McpError::Failed(data.message.to_string()),
        ServiceError::TransportSend(transport) => classify(transport),
        ServiceError::TransportClosed | ServiceError::Cancelled { .. } | ServiceError::Timeout { .. } => {
            McpError::Unreachable("the request did not complete".to_owned())
        }
        _ => McpError::Failed("the server answered the request with an unusable result".to_owned()),
    }
}

/// Converts the result of a tool: the error text, else the structured content, else the concatenated texts.
fn tool_value(result: &CallToolResult) -> Result<Value, McpError> {
    let texts: String = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect();
    if result.is_error == Some(true) {
        let text = if texts.is_empty() {
            "the tool returned an error".to_owned()
        } else {
            texts
        };
        return Err(McpError::Failed(text));
    }
    Ok(result.structured_content.clone().unwrap_or(Value::String(texts)))
}

impl<A: McpAuth> McpLister for RmcpConnector<A> {
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let peer = self.peer().await?;
        let tools = self.bounded(peer.list_all_tools()).await?;
        Ok(tools.iter().map(mcp_tool).collect())
    }
}

impl<A: McpAuth> McpConnector for RmcpConnector<A> {
    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<Value, McpError> {
        let mut params = CallToolRequestParams::new(name.to_owned());
        match arguments {
            Value::Null => {}
            Value::Object(map) => params = params.with_arguments(map.clone()),
            Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
                return Err(McpError::Failed(
                    "the arguments of a tool must be a JSON object".to_owned(),
                ));
            }
        }
        let peer = self.peer().await?;
        let result = self.bounded(peer.call_tool(params)).await?;
        tool_value(&result)
    }

    // The session is dropped; the next listing or call asks the host for fresh headers and opens a new one.
    async fn reconnect(&self) {
        drop(self.session.lock().await.take());
    }

    // Waits for the next notification of the server, whichever session delivered it: the queue outlives a reconnection. It
    // does not open the session (the first listing or call does), and it waits for ever when the server sends none.
    async fn next_notification(&self) -> Option<McpNotification> {
        self.notifications.lock().await.recv().await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::timeout;

    use super::{
        McpAuth, McpClientConfig, McpConnector as _, McpError, McpHeaders, McpLister as _, RmcpConnector,
    };

    /// Body of the error answers: it must never show in an error.
    const BODY_SECRET: &str = "{\"error\":\"s3cr3t-body\"}";

    /// What the fake server does with the requests it receives.
    #[derive(Debug, Clone, Copy)]
    enum Mode {
        /// Answers like a healthy MCP server.
        Normal,
        /// Answers every request with this HTTP status, this `WWW-Authenticate` header and a body holding a secret.
        Status(u16, Option<&'static str>),
        /// Never answers the JSON-RPC method of this name.
        Hang(&'static str),
        /// Closes the connection without answering.
        Drop,
        /// Answers `initialize` with a result that is not an initialize result.
        BadInitialize,
        /// Answers `initialize` with HTTP 200 and an empty body.
        EmptyAnswer,
        /// Answers `initialize` with HTTP 200 and a body that is not JSON.
        NotJson,
        /// Answers the first page of `tools/list` with an event stream: two notifications, then the result.
        Notify,
    }

    /// A request the fake server received: its JSON-RPC method (empty when it has none) and its headers, in lower case.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        headers: BTreeMap<String, String>,
    }

    /// Minimal HTTP/1.1 MCP server on a local port: JSON answers only, one request per connection.
    struct FakeServer {
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<Seen>>>,
        /// TCP connections accepted, whether or not a full HTTP request followed.
        connections: Arc<AtomicUsize>,
    }

    impl FakeServer {
        async fn start(mode: Mode) -> Self {
            install_tls_provider();
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("a local port can be bound");
            let addr = listener.local_addr().expect("a bound listener has an address");
            let seen = Arc::new(Mutex::new(Vec::new()));
            let accepted = Arc::clone(&seen);
            let connections = Arc::new(AtomicUsize::new(0));
            let _acceptor = tokio::spawn(accept_loop(listener, mode, accepted, Arc::clone(&connections)));
            Self {
                addr,
                seen,
                connections,
            }
        }

        fn url(&self) -> String {
            format!("http://{}/mcp", self.addr)
        }

        /// The URL of the server with a fake secret in its query.
        fn url_with_secret(&self) -> String {
            format!("http://{}/mcp?token=s3cr3t", self.addr)
        }

        fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }

        fn methods(&self) -> Vec<String> {
            self.seen().into_iter().map(|request| request.method).collect()
        }
    }

    /// Accepts the connections of the fake server, one task each.
    async fn accept_loop(
        listener: TcpListener,
        mode: Mode,
        seen: Arc<Mutex<Vec<Seen>>>,
        connections: Arc<AtomicUsize>,
    ) {
        while let Ok((stream, _peer)) = listener.accept().await {
            let _previous = connections.fetch_add(1, Ordering::SeqCst);
            let _connection = tokio::spawn(serve_connection(stream, mode, Arc::clone(&seen)));
        }
    }

    /// Installs the TLS provider that the host owns in production. An error means one is already installed.
    fn install_tls_provider() {
        let _installed = rustls::crypto::ring::default_provider().install_default();
    }

    type Request = (String, BTreeMap<String, String>, Vec<u8>);

    /// Reads one HTTP request: its method, its headers (names in lower case) and its body.
    async fn read_request(stream: &mut TcpStream) -> Option<Request> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        let head_end = loop {
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position;
            }
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(chunk.get(..read)?);
        };
        let head = String::from_utf8_lossy(buffer.get(..head_end)?).into_owned();
        let mut lines = head.split("\r\n");
        let method = lines.next()?.split(' ').next()?.to_owned();
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
        let mut body = buffer.get(head_end.checked_add(4)?..)?.to_vec();
        while body.len() < length {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            body.extend_from_slice(chunk.get(..read)?);
        }
        Some((method, headers, body))
    }

    /// Writes one HTTP answer and closes the connection.
    async fn respond(stream: &mut TcpStream, status: u16, extra: &[(&str, &str)], body: &str) {
        let mut answer = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in extra {
            answer.extend(format!("{name}: {value}\r\n").chars());
        }
        answer.push_str("\r\n");
        answer.push_str(body);
        let _written = stream.write_all(answer.as_bytes()).await;
        let _closed = stream.shutdown().await;
    }

    /// Writes an event stream answer holding two server notifications, then the JSON-RPC answer, and closes.
    async fn respond_with_notifications(stream: &mut TcpStream, answer: &Value) {
        let message = json!({"jsonrpc": "2.0", "method": "notifications/message",
            "params": {"level": "info", "data": "disk almost full"}});
        let list_changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        let body = format!("data: {message}\n\ndata: {list_changed}\n\ndata: {answer}\n\n");
        let head = format!(
            "HTTP/1.1 200 X\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _written = stream.write_all(format!("{head}{body}").as_bytes()).await;
        let _closed = stream.shutdown().await;
    }

    async fn serve_connection(mut stream: TcpStream, mode: Mode, seen: Arc<Mutex<Vec<Seen>>>) {
        let Some((http_method, headers, body)) = read_request(&mut stream).await else {
            return;
        };
        let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        seen.lock().unwrap_or_else(PoisonError::into_inner).push(Seen {
            method: method.clone(),
            headers,
        });
        if http_method != "POST" {
            respond(&mut stream, 405, &[], "").await;
            return;
        }
        match mode {
            Mode::Drop => return,
            Mode::Status(code, challenge) => {
                let extra: Vec<(&str, &str)> = challenge
                    .map(|value| ("www-authenticate", value))
                    .into_iter()
                    .collect();
                respond(&mut stream, code, &extra, BODY_SECRET).await;
                return;
            }
            Mode::Hang(hung) if hung == method => std::future::pending::<()>().await,
            Mode::EmptyAnswer => {
                respond(&mut stream, 200, &[], "").await;
                return;
            }
            Mode::NotJson => {
                respond(&mut stream, 200, &[], "this is not json").await;
                return;
            }
            Mode::BadInitialize if method == "initialize" => {
                let answer =
                    json!({"jsonrpc": "2.0", "id": message.get("id").cloned(), "result": {"nonsense": 1}});
                respond(&mut stream, 200, &[], &answer.to_string()).await;
                return;
            }
            Mode::Normal | Mode::Hang(_) | Mode::BadInitialize | Mode::Notify => {}
        }
        let Some(id) = message.get("id").cloned() else {
            respond(&mut stream, 202, &[], "").await;
            return;
        };
        let mut answer = json!({"jsonrpc": "2.0", "id": id});
        match rpc_answer(&method, &message) {
            Ok(result) => answer["result"] = result,
            Err(error) => answer["error"] = error,
        }
        if matches!(mode, Mode::Notify)
            && method == "tools/list"
            && message.pointer("/params/cursor").is_none()
        {
            respond_with_notifications(&mut stream, &answer).await;
            return;
        }
        respond(&mut stream, 200, &[], &answer.to_string()).await;
    }

    fn text(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    /// The JSON-RPC result (or error) of the fake server for a method.
    fn rpc_answer(method: &str, message: &Value) -> Result<Value, Value> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "0.0.0"}
            })),
            "tools/list" => {
                if message.pointer("/params/cursor").is_none() {
                    Ok(json!({"tools": [{
                        "name": "echo",
                        "description": "Echoes its input.",
                        "inputSchema": {"type": "object", "properties": {"msg": {"type": "string"}}}
                    }], "nextCursor": "page-2"}))
                } else {
                    Ok(json!({"tools": [{
                        "name": "boom",
                        "description": "Always fails.",
                        "inputSchema": {"type": "object"}
                    }]}))
                }
            }
            "tools/call" => {
                let name = message
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match name {
                    "echo" => {
                        let msg = message
                            .pointer("/params/arguments/msg")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        Ok(json!({"content": [text(msg)]}))
                    }
                    "boom" => Ok(json!({"content": [text("kaboom")], "isError": true})),
                    "structured" => {
                        Ok(json!({"content": [text("ignored")], "structuredContent": {"answer": 42}}))
                    }
                    "multi" => Ok(
                        json!({"content": [text("a"), text("b"), {"type": "image", "data": "AA==", "mimeType": "image/png"}]}),
                    ),
                    _ => Err(json!({"code": -32602, "message": "unknown tool"})),
                }
            }
            _ => Err(json!({"code": -32601, "message": "method not found"})),
        }
    }

    /// Host authentication that always gives the same headers.
    struct StaticAuth(McpHeaders);

    impl McpAuth for StaticAuth {
        fn headers(&self) -> impl Future<Output = Result<McpHeaders, McpError>> + Send {
            std::future::ready(Ok(self.0.clone()))
        }
    }

    /// Host authentication whose token changes at each call: the tokens in order, then the last one again.
    struct RotatingAuth {
        tokens: Vec<&'static str>,
        calls: Mutex<usize>,
    }

    impl McpAuth for RotatingAuth {
        fn headers(&self) -> impl Future<Output = Result<McpHeaders, McpError>> + Send {
            let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
            let token = self
                .tokens
                .get(*calls)
                .or_else(|| self.tokens.last())
                .copied()
                .unwrap_or("");
            *calls = calls.saturating_add(1);
            drop(calls);
            std::future::ready(Ok(McpHeaders::new().with_bearer(token)))
        }
    }

    /// Host authentication that cannot give headers.
    struct FailingAuth;

    impl McpAuth for FailingAuth {
        fn headers(&self) -> impl Future<Output = Result<McpHeaders, McpError>> + Send {
            std::future::ready(Err(McpError::Failed("no token available".to_owned())))
        }
    }

    fn anonymous() -> Arc<StaticAuth> {
        Arc::new(StaticAuth(McpHeaders::new()))
    }

    fn connector_for(server: &FakeServer) -> RmcpConnector<StaticAuth> {
        RmcpConnector::new(McpClientConfig::new(&server.url()), anonymous())
    }

    fn assert_send_sync<T: Send + Sync>() {}

    // Scenario: listage des tools
    #[tokio::test]
    async fn lists_the_tools_with_name_description_and_schema() {
        let server = FakeServer::start(Mode::Normal).await;
        let tools = connector_for(&server)
            .list_tools()
            .await
            .expect("a healthy server lists its tools");
        assert_eq!(tools.len(), 2, "both pages must be followed, got {tools:?}");
        let echo = tools.first().expect("the first page holds a tool");
        assert_eq!(echo.name(), "echo", "the name is kept");
        assert_eq!(echo.description(), "Echoes its input.", "the description is kept");
        assert_eq!(
            echo.input_schema(),
            &json!({"type": "object", "properties": {"msg": {"type": "string"}}}),
            "the input schema is kept"
        );
        assert_eq!(
            tools.get(1).map(super::McpTool::name),
            Some("boom"),
            "the second page is followed"
        );
        assert_eq!(
            server.methods(),
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tools/list"
            ],
            "the handshake precedes the two pages of the listing"
        );
    }

    // Scenario: appel réussi
    #[tokio::test]
    async fn successful_call_carries_the_text() {
        let server = FakeServer::start(Mode::Normal).await;
        let value = connector_for(&server)
            .call_tool("echo", &json!({"msg": "hello"}))
            .await
            .expect("a call to a healthy tool succeeds");
        assert_eq!(value, json!("hello"), "the value carries the text of the tool");
    }

    // Scenario: appel réussi (contenu structuré, contenus textuels, contenus non textuels ignorés)
    #[tokio::test]
    async fn structured_content_wins_and_texts_are_concatenated() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = connector_for(&server);
        let structured = connector
            .call_tool("structured", &json!({}))
            .await
            .expect("the call succeeds");
        assert_eq!(
            structured,
            json!({"answer": 42}),
            "the structured content is returned when present"
        );
        let multi = connector
            .call_tool("multi", &json!(null))
            .await
            .expect("the call succeeds");
        assert_eq!(
            multi,
            json!("ab"),
            "the text contents are concatenated and the image is ignored"
        );
    }

    // Scenario: erreur du tool
    #[tokio::test]
    async fn tool_error_is_failed_with_the_server_text() {
        let server = FakeServer::start(Mode::Normal).await;
        let error = connector_for(&server)
            .call_tool("boom", &json!({}))
            .await
            .expect_err("a tool marked as an error fails");
        assert!(
            matches!(&error, McpError::Failed(text) if text.contains("kaboom")),
            "the error must be Failed and carry the server text, got {error:?}"
        );
    }

    #[tokio::test]
    async fn unknown_tool_and_bad_arguments_are_failed() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = connector_for(&server);
        let unknown = connector.call_tool("absent", &json!({})).await;
        assert!(
            matches!(&unknown, Err(McpError::Failed(text)) if text.contains("unknown tool")),
            "an unknown tool must be Failed with the server text, got {unknown:?}"
        );
        let bad = connector.call_tool("echo", &json!([1])).await;
        assert!(
            matches!(bad, Err(McpError::Failed(_))),
            "arguments that are not an object must be Failed, got {bad:?}"
        );
    }

    // Scenario: serveur injoignable
    #[tokio::test]
    async fn nobody_listening_is_unreachable() {
        install_tls_provider();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port can be bound");
        let address = listener.local_addr().expect("a bound listener has an address");
        drop(listener);
        let connector = RmcpConnector::new(
            McpClientConfig::new(&format!("http://{address}/mcp")),
            anonymous(),
        );
        let result = connector.list_tools().await;
        assert!(
            matches!(result, Err(McpError::Unreachable(_))),
            "a closed port must be Unreachable, got {result:?}"
        );
    }

    // Scenario: délai de requête dépassé
    #[tokio::test]
    async fn request_timeout_is_unreachable() {
        let server = FakeServer::start(Mode::Hang("tools/call")).await;
        let config = McpClientConfig::new(&server.url()).with_request_timeout(Duration::from_millis(300));
        let connector = RmcpConnector::new(config, anonymous());
        let outcome = timeout(
            Duration::from_secs(10),
            connector.call_tool("echo", &json!({"msg": "x"})),
        )
        .await
        .expect("the connector must give up before the test deadline");
        assert!(
            matches!(outcome, Err(McpError::Unreachable(_))),
            "a request without answer must be Unreachable, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn connect_timeout_is_unreachable() {
        let server = FakeServer::start(Mode::Hang("initialize")).await;
        let config = McpClientConfig::new(&server.url()).with_connect_timeout(Duration::from_millis(300));
        let connector = RmcpConnector::new(config, anonymous());
        let outcome = timeout(Duration::from_secs(10), connector.list_tools())
            .await
            .expect("the connector must give up before the test deadline");
        assert!(
            matches!(outcome, Err(McpError::Unreachable(_))),
            "a handshake without answer must be Unreachable, got {outcome:?}"
        );
    }

    /// Lists the tools of a server that answers `mode`, with the token `tok-secret`, and returns the error.
    async fn failure_with(mode: Mode) -> McpError {
        let server = FakeServer::start(mode).await;
        let auth = Arc::new(StaticAuth(McpHeaders::new().with_bearer("tok-secret")));
        let connector = RmcpConnector::new(McpClientConfig::new(&server.url()), auth);
        connector
            .list_tools()
            .await
            .expect_err("the server refuses the listing")
    }

    fn assert_no_secret(error: &McpError) {
        assert!(
            !error.to_string().contains("tok-secret"),
            "the error must not carry the token: {error}"
        );
        assert!(
            !error.to_string().contains("s3cr3t"),
            "the error must not carry a secret of the URL or of the body: {error}"
        );
    }

    // Scenario: authentification refusée
    #[tokio::test]
    async fn unauthorized_with_challenge_is_failed_without_token() {
        let error = failure_with(Mode::Status(401, Some("Bearer realm=\"mcp\""))).await;
        assert!(
            matches!(error, McpError::Failed(_)),
            "a 401 with a challenge must be Failed, got {error:?}"
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn unauthorized_without_challenge_is_failed_without_token() {
        let error = failure_with(Mode::Status(401, None)).await;
        assert!(
            matches!(error, McpError::Failed(_)),
            "a 401 without challenge must be Failed, got {error:?}"
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn forbidden_is_failed_without_token() {
        let error = failure_with(Mode::Status(403, None)).await;
        assert!(
            matches!(error, McpError::Failed(_)),
            "a 403 must be Failed, got {error:?}"
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn server_error_is_unreachable() {
        let error = failure_with(Mode::Status(500, None)).await;
        assert!(
            matches!(error, McpError::Unreachable(_)),
            "a 500 must be Unreachable, got {error:?}"
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn too_many_requests_is_unreachable() {
        let error = failure_with(Mode::Status(429, None)).await;
        assert!(
            matches!(error, McpError::Unreachable(_)),
            "a 429 must be Unreachable, got {error:?}"
        );
    }

    #[tokio::test]
    async fn cut_connection_is_unreachable() {
        let error = failure_with(Mode::Drop).await;
        assert!(
            matches!(error, McpError::Unreachable(_)),
            "a cut connection must be Unreachable, got {error:?}"
        );
        assert_no_secret(&error);
    }

    // Scenario: le jeton part en en-tête
    #[tokio::test]
    async fn token_and_custom_header_are_sent() {
        let server = FakeServer::start(Mode::Normal).await;
        let headers = McpHeaders::new()
            .with_bearer("tok-123")
            .with_header("x-tenant", "acme");
        let connector =
            RmcpConnector::new(McpClientConfig::new(&server.url()), Arc::new(StaticAuth(headers)));
        let _tools = connector.list_tools().await.expect("the listing succeeds");
        let seen = server.seen();
        assert!(!seen.is_empty(), "the server must have received requests");
        for request in &seen {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer tok-123"),
                "every request of `{}` carries the token",
                request.method
            );
            assert_eq!(
                request.headers.get("x-tenant").map(String::as_str),
                Some("acme"),
                "every request of `{}` carries the custom header",
                request.method
            );
        }
    }

    #[tokio::test]
    async fn invalid_header_name_is_failed_without_its_value() {
        let server = FakeServer::start(Mode::Normal).await;
        let headers = McpHeaders::new().with_header("bad name", "v-secret");
        let connector =
            RmcpConnector::new(McpClientConfig::new(&server.url()), Arc::new(StaticAuth(headers)));
        let error = connector
            .list_tools()
            .await
            .expect_err("an invalid header name is refused");
        assert!(
            matches!(&error, McpError::Failed(text) if !text.contains("v-secret")),
            "got {error:?}"
        );
        assert!(server.seen().is_empty(), "nothing is sent with an invalid header");
    }

    #[tokio::test]
    async fn auth_failure_is_returned_as_is() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = RmcpConnector::new(McpClientConfig::new(&server.url()), Arc::new(FailingAuth));
        let error = connector
            .list_tools()
            .await
            .expect_err("no headers, no connection");
        assert!(
            matches!(&error, McpError::Failed(text) if text == "no token available"),
            "got {error:?}"
        );
        assert!(server.seen().is_empty(), "nothing is sent without headers");
    }

    // Scenario: reconnexion et nouveau jeton
    #[tokio::test]
    async fn reconnect_takes_the_renewed_token() {
        let server = FakeServer::start(Mode::Normal).await;
        let auth = Arc::new(RotatingAuth {
            tokens: vec!["old", "new"],
            calls: Mutex::new(0),
        });
        let connector = RmcpConnector::new(McpClientConfig::new(&server.url()), auth);
        let _first = connector.list_tools().await.expect("the first listing succeeds");
        let before = server.seen().len();
        connector.reconnect().await;
        assert_eq!(
            server.seen().len(),
            before,
            "reconnecting does not connect by itself"
        );
        let _second = connector
            .list_tools()
            .await
            .expect("the listing after the reconnection succeeds");
        let seen = server.seen();
        let (old, new) = seen.split_at(before);
        assert!(
            old.iter()
                .all(|r| r.headers.get("authorization").map(String::as_str) == Some("Bearer old")),
            "the first session uses the first token, got {old:?}"
        );
        assert!(
            new.first().is_some_and(|r| r.method == "initialize")
                && new
                    .iter()
                    .all(|r| r.headers.get("authorization").map(String::as_str) == Some("Bearer new")),
            "the new session starts over with the new token, got {new:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_calls_share_one_session() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = connector_for(&server);
        let (a, b, c) = tokio::join!(
            connector.list_tools(),
            connector.list_tools(),
            connector.list_tools()
        );
        assert!(
            a.is_ok() && b.is_ok() && c.is_ok(),
            "all concurrent listings succeed"
        );
        let initializations = server.methods().iter().filter(|m| *m == "initialize").count();
        assert_eq!(initializations, 1, "a single session serves the concurrent calls");
    }

    // Scenario: les en-têtes ne se montrent pas
    #[test]
    fn headers_debug_shows_names_and_never_values() {
        let headers = McpHeaders::new()
            .with_bearer("tok-secret")
            .with_header("x-api-key", "key-secret");
        let text = format!("{headers:?}");
        assert!(text.contains("x-api-key"), "the header name is shown: {text}");
        assert!(
            text.contains("bearer"),
            "the presence of a token is shown: {text}"
        );
        assert!(!text.contains("tok-secret"), "the token is never shown: {text}");
        assert!(!text.contains("key-secret"), "a value is never shown: {text}");
        let bare = format!("{:?}", McpHeaders::new());
        assert!(!bare.contains("true"), "no token, none announced: {bare}");
    }

    // Scenario: aucune connexion à la construction
    #[tokio::test]
    async fn construction_does_not_connect() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = connector_for(&server);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            server.seen().is_empty(),
            "constructing a connector must not reach the server"
        );
        drop(connector);
        let lost = RmcpConnector::new(McpClientConfig::new("http://127.0.0.1:1/mcp"), anonymous());
        assert!(
            format!("{lost:?}").contains("RmcpConnector"),
            "an unreachable URL is accepted at construction"
        );
    }

    #[tokio::test]
    async fn construction_opens_no_tcp_connection() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = connector_for(&server);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(server.connections(), 0, "no TCP connection at construction");
        let _tools = connector.list_tools().await.expect("the listing succeeds");
        assert!(server.connections() > 0, "the listing connects");
    }

    /// Lists the tools through a URL that carries a secret, against a server in `mode`.
    async fn failure_with_secret_url(mode: Mode) -> McpError {
        let server = FakeServer::start(mode).await;
        let config = McpClientConfig::new(&server.url_with_secret())
            .with_request_timeout(Duration::from_millis(300))
            .with_connect_timeout(Duration::from_millis(300));
        RmcpConnector::new(config, anonymous())
            .list_tools()
            .await
            .expect_err("the server fails")
    }

    #[tokio::test]
    async fn url_secret_never_shows_in_transport_errors() {
        for (label, mode) in [
            ("cut connection", Mode::Drop),
            ("5xx", Mode::Status(503, None)),
            ("timeout", Mode::Hang("initialize")),
            ("401", Mode::Status(401, None)),
            ("403", Mode::Status(403, None)),
            ("invalid initialize result", Mode::BadInitialize),
            ("empty answer", Mode::EmptyAnswer),
            ("answer that is not JSON", Mode::NotJson),
        ] {
            let error = failure_with_secret_url(mode).await;
            assert_no_secret(&error);
            assert!(
                !error.to_string().contains("127.0.0.1"),
                "{label}: the URL shows: {error}"
            );
        }
        install_tls_provider();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port can be bound");
        let address = listener.local_addr().expect("a bound listener has an address");
        drop(listener);
        let refused = RmcpConnector::new(
            McpClientConfig::new(&format!("http://{address}/mcp?token=s3cr3t")),
            anonymous(),
        )
        .list_tools()
        .await
        .expect_err("nobody listens");
        assert!(matches!(refused, McpError::Unreachable(_)), "got {refused:?}");
        assert_no_secret(&refused);
        assert!(
            !refused.to_string().contains("127.0.0.1"),
            "the URL shows: {refused}"
        );
    }

    #[tokio::test]
    async fn invalid_initialize_result_is_failed_with_a_fixed_text() {
        let error = failure_with_secret_url(Mode::BadInitialize).await;
        assert!(
            matches!(&error, McpError::Failed(text) if text == "the server answered the handshake with an unusable result"),
            "got {error:?}"
        );
        assert_no_secret(&error);
    }

    #[test]
    fn config_debug_hides_the_url() {
        let text = format!("{:?}", McpClientConfig::new("http://host/mcp?token=s3cr3t"));
        assert!(!text.contains("s3cr3t"), "the URL secret shows: {text}");
        assert!(!text.contains("host"), "the URL shows: {text}");
        assert!(text.contains("McpClientConfig"), "the type is named: {text}");
    }

    #[tokio::test]
    async fn insufficient_scope_is_failed() {
        let error = failure_with(Mode::Status(
            403,
            Some("Bearer error=\"insufficient_scope\", scope=\"a b\""),
        ))
        .await;
        assert!(
            matches!(&error, McpError::Failed(text) if text.contains("scope")),
            "an insufficient scope must be Failed, got {error:?}"
        );
        assert_no_secret(&error);
    }

    #[tokio::test]
    async fn server_error_codes_are_unreachable_and_never_show_the_body() {
        for code in [500u16, 502, 503] {
            let error = failure_with(Mode::Status(code, None)).await;
            assert!(
                matches!(&error, McpError::Unreachable(text) if text.contains(&code.to_string())),
                "a {code} must be Unreachable and name the status, got {error:?}"
            );
            assert_no_secret(&error);
        }
    }

    #[tokio::test]
    async fn body_never_shows_in_errors() {
        for code in [401u16, 403, 429, 500] {
            let error = failure_with(Mode::Status(code, None)).await;
            assert_no_secret(&error);
        }
    }

    #[test]
    fn connector_and_headers_are_send_and_sync() {
        assert_send_sync::<RmcpConnector<StaticAuth>>();
        assert_send_sync::<McpHeaders>();
        assert_send_sync::<McpClientConfig>();
    }

    // Scenario (mcp_client.sdd #5): notifications du serveur
    #[tokio::test]
    async fn server_notifications_are_delivered_in_order() {
        let server = FakeServer::start(Mode::Notify).await;
        let connector = connector_for(&server);
        let _tools = connector.list_tools().await.expect("the server lists its tools");
        let first = timeout(Duration::from_secs(5), connector.next_notification())
            .await
            .expect("a notification arrives in time")
            .expect("the connector delivers the notification");
        assert_eq!(
            first.server(),
            "fake",
            "the server name is the one of the handshake"
        );
        assert_eq!(first.method(), "notifications/message", "the method is kept");
        assert_eq!(
            first.params(),
            &json!({"level": "info", "data": "disk almost full"}),
            "the params are the server's"
        );
        let second = timeout(Duration::from_secs(5), connector.next_notification())
            .await
            .expect("the second notification arrives in time")
            .expect("the connector delivers the second notification");
        assert_eq!(
            second.method(),
            "notifications/tools/list_changed",
            "the method is kept"
        );
        assert_eq!(
            second.params(),
            &Value::Null,
            "a notification without params gives null"
        );
    }

    #[tokio::test]
    async fn notification_holds_no_request_data_nor_secret() {
        let server = FakeServer::start(Mode::Notify).await;
        let auth = Arc::new(StaticAuth(McpHeaders::new().with_bearer("tok-secret")));
        let connector = RmcpConnector::new(McpClientConfig::new(&server.url_with_secret()), auth);
        let _tools = connector.list_tools().await.expect("the server lists its tools");
        let notification = timeout(Duration::from_secs(5), connector.next_notification())
            .await
            .expect("a notification arrives in time")
            .expect("the connector delivers the notification");
        let shown = format!("{notification:?}");
        for secret in ["tok-secret", "s3cr3t", "127.0.0.1", "Authorization"] {
            assert!(
                !shown.contains(secret),
                "the notification must not show `{secret}`: {shown}"
            );
        }
    }

    #[tokio::test]
    async fn notifications_wait_when_the_server_sent_none() {
        let server = FakeServer::start(Mode::Normal).await;
        let connector = connector_for(&server);
        let _tools = connector.list_tools().await.expect("the server lists its tools");
        let waited = timeout(Duration::from_millis(300), connector.next_notification()).await;
        assert!(
            waited.is_err(),
            "a live connector waits instead of returning, got {waited:?}"
        );
    }
}
