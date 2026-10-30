//! Boundary with the remote MCP servers: a tool as a server describes it, the connection errors, the connector traits (listing
//! only, then listing and calling), and a scripted connector for tests. No real transport lives here: the host, or a later lot,
//! connects to a server (`agent-sessions/modules#FR-6`, `agent-sessions/modules#FR-11`, `agent-sessions#CON-5`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde_json::Value;

/// Error of an MCP exchange. Its text never carries an argument of a call nor a secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The server no longer answers. The caller may try to reconnect.
    #[error("the MCP server is unreachable: {0}")]
    Unreachable(String),
    /// The server answered with an error: unknown tool, error of the tool, or an unreadable answer. The caller does not
    /// retry.
    #[error("the MCP server failed: {0}")]
    Failed(String),
}

/// A tool as an MCP server describes it: its name, its description and its input schema.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct McpTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl McpTool {
    /// Builds a tool from its name, its description and its input schema.
    #[must_use]
    pub fn new(name: &str, description: &str, input_schema: Value) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema,
        }
    }

    /// Returns the name the server gives the tool.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the description the server gives the tool.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the input schema the server gives the tool.
    #[must_use]
    pub const fn input_schema(&self) -> &Value {
        &self.input_schema
    }
}

/// Connector that can only list the tools of a server. A function that receives only this trait cannot call a tool.
pub trait McpLister: Send + Sync {
    /// Lists the tools of the server.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Unreachable`] when the server does not answer, and [`McpError::Failed`] when its answer is
    /// unreadable.
    fn list_tools(&self) -> impl Future<Output = Result<Vec<McpTool>, McpError>> + Send;
}

/// Connector that can list the tools of a server and call them.
pub trait McpConnector: McpLister {
    /// Calls the tool of the server that carries the original MCP `name`, with `arguments`, and returns the value it gives.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Unreachable`] when the server does not answer, and [`McpError::Failed`] when the server answers
    /// with an error (unknown tool, error of the tool, unreadable answer).
    fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
    ) -> impl Future<Output = Result<Value, McpError>> + Send;

    /// Re-establishes the connection after a loss.
    fn reconnect(&self) -> impl Future<Output = ()> + Send;
}

/// Scripted connector for tests: a fake server with the tools it exposes, the results of its tools, and programmed
/// failures. It performs no I/O and can be shared between threads.
#[derive(Debug)]
#[non_exhaustive]
pub struct ScriptedConnector {
    state: Mutex<ConnectorState>,
}

/// State of a scripted connector.
#[derive(Debug, Default)]
struct ConnectorState {
    tools: Vec<McpTool>,
    results: BTreeMap<String, Value>,
    failing_tools: BTreeSet<String>,
    unreachable_calls: usize,
    listing_unreachable: bool,
    calls: usize,
    reconnects: usize,
    received: Vec<Value>,
}

impl ScriptedConnector {
    /// Builds a connector exposing `tools`, with no programmed result and no programmed failure.
    #[must_use]
    pub fn new(tools: Vec<McpTool>) -> Self {
        Self {
            state: Mutex::new(ConnectorState {
                tools,
                ..ConnectorState::default()
            }),
        }
    }

    /// Programs the value that the tool `name` gives when it is called.
    #[must_use]
    pub fn with_result(mut self, name: &str, value: Value) -> Self {
        let _previous = self
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .results
            .insert(name.to_owned(), value);
        self
    }

    /// Programs the next `count` calls to fail with [`McpError::Unreachable`], whatever the tool.
    #[must_use]
    pub fn fail_unreachable(mut self, count: usize) -> Self {
        self.state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .unreachable_calls = count;
        self
    }

    /// Programs the tool `name` to fail with [`McpError::Failed`] when it is called.
    #[must_use]
    pub fn fail_tool(mut self, name: &str) -> Self {
        let _added = self
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .failing_tools
            .insert(name.to_owned());
        self
    }

    /// Programs the listing to fail with [`McpError::Unreachable`].
    #[must_use]
    pub fn listing_unreachable(mut self) -> Self {
        self.state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .listing_unreachable = true;
        self
    }

    /// Returns the number of calls received, failed ones included.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.lock().calls
    }

    /// Returns the number of reconnections.
    #[must_use]
    pub fn reconnects(&self) -> usize {
        self.lock().reconnects
    }

    /// Returns the arguments received by each call that reached the server, in order.
    #[must_use]
    pub fn received(&self) -> Vec<Value> {
        self.lock().received.clone()
    }

    /// Locks the state. A poisoned lock is recovered: the fake server keeps answering.
    fn lock(&self) -> MutexGuard<'_, ConnectorState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers the listing: the tools, or an unreachable server when the listing is programmed to fail.
    fn listing(&self) -> Result<Vec<McpTool>, McpError> {
        let state = self.lock();
        if state.listing_unreachable {
            return Err(McpError::Unreachable(
                "the listing was programmed to fail".to_owned(),
            ));
        }
        Ok(state.tools.clone())
    }

    /// Answers a call and counts it. A call that reaches the server is recorded with its arguments; a call that finds no
    /// programmed failure gives the programmed result of its tool, or null.
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, McpError> {
        let mut state = self.lock();
        state.calls = state.calls.saturating_add(1);
        if let Some(remaining) = state.unreachable_calls.checked_sub(1) {
            state.unreachable_calls = remaining;
            return Err(McpError::Unreachable(format!(
                "the call to `{name}` was programmed to fail"
            )));
        }
        state.received.push(arguments.clone());
        if !state.tools.iter().any(|tool| tool.name() == name) {
            return Err(McpError::Failed(format!("unknown tool `{name}`")));
        }
        if state.failing_tools.contains(name) {
            return Err(McpError::Failed(format!("tool `{name}` returned an error")));
        }
        Ok(state.results.get(name).cloned().unwrap_or(Value::Null))
    }
}

impl McpLister for ScriptedConnector {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<McpTool>, McpError>> + Send {
        std::future::ready(self.listing())
    }
}

impl McpConnector for ScriptedConnector {
    fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
    ) -> impl Future<Output = Result<Value, McpError>> + Send {
        std::future::ready(self.call(name, arguments))
    }

    // A reconnection counts and never fails: the fake server comes back at once.
    fn reconnect(&self) -> impl Future<Output = ()> + Send {
        {
            let mut state = self.lock();
            state.reconnects = state.reconnects.saturating_add(1);
        }
        std::future::ready(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use serde_json::json;

    use super::{McpConnector as _, McpError, McpLister as _, McpTool, ScriptedConnector};

    fn assert_send_sync<T: Send + Sync>() {}

    fn echo_tool() -> McpTool {
        McpTool::new(
            "echo",
            "Echoes its input.",
            json!({"type": "object", "properties": {"msg": {"type": "string"}}}),
        )
    }

    fn boom_tool() -> McpTool {
        McpTool::new("boom", "Always fails.", json!({"type": "object"}))
    }

    /// A connector exposing `echo` and `boom`.
    fn two_tools() -> ScriptedConnector {
        ScriptedConnector::new(vec![echo_tool(), boom_tool()])
    }

    // Scenario: le connecteur scripté liste ses tools
    #[tokio::test]
    async fn scripted_connector_lists_its_tools() {
        let connector = two_tools();
        let tools = connector
            .list_tools()
            .await
            .expect("the listing of a reachable fake server succeeds");
        assert_eq!(tools.len(), 2, "both tools must be listed, got {tools:?}");
        let first = tools.first().expect("one tool is listed first");
        assert_eq!(first.name(), "echo", "the first listed tool keeps its name");
        assert_eq!(
            first.description(),
            "Echoes its input.",
            "the description is kept"
        );
        assert_eq!(
            first.input_schema(),
            &json!({"type": "object", "properties": {"msg": {"type": "string"}}}),
            "the input schema is kept as given"
        );
        assert_eq!(
            tools.get(1).map(McpTool::name),
            Some("boom"),
            "the second tool is listed too"
        );
    }

    // Scenario: résultat programmé
    #[tokio::test]
    async fn programmed_result_is_returned_and_arguments_are_recorded() {
        let connector = two_tools().with_result("echo", json!({"ok": true}));
        let arguments = json!({"msg": "hi"});
        let value = connector
            .call_tool("echo", &arguments)
            .await
            .expect("a call to a programmed tool succeeds");
        assert_eq!(
            value,
            json!({"ok": true}),
            "the value programmed for the tool is returned"
        );
        assert_eq!(
            connector.received(),
            vec![json!({"msg": "hi"})],
            "the arguments of the call must be recorded"
        );
        assert_eq!(connector.calls(), 1, "the call must be counted");
    }

    #[tokio::test]
    async fn tool_without_programmed_result_returns_null() {
        let connector = two_tools();
        let value = connector
            .call_tool("echo", &json!({}))
            .await
            .expect("a call to a known tool without programmed result succeeds");
        assert_eq!(value, json!(null), "a tool without programmed result gives null");
    }

    // Scenario: échecs programmés dans l'ordre
    #[tokio::test]
    async fn programmed_failures_are_consumed_in_order() {
        let connector = two_tools().with_result("echo", json!("fine")).fail_unreachable(2);
        let first = connector.call_tool("echo", &json!({})).await;
        let second = connector.call_tool("echo", &json!({})).await;
        let third = connector.call_tool("echo", &json!({})).await;
        assert!(
            matches!(first, Err(McpError::Unreachable(_))),
            "the first call must be Unreachable, got {first:?}"
        );
        assert!(
            matches!(second, Err(McpError::Unreachable(_))),
            "the second call must be Unreachable, got {second:?}"
        );
        assert_eq!(
            third.expect("the third call must succeed"),
            json!("fine"),
            "the third call must give the programmed result"
        );
        assert_eq!(connector.calls(), 3, "the three calls must be counted");
    }

    #[tokio::test]
    async fn unreachable_call_is_counted_but_not_received() {
        let connector = two_tools().fail_unreachable(1);
        let _lost = connector.call_tool("echo", &json!({"msg": "lost"})).await;
        assert_eq!(connector.calls(), 1, "a failed call is still counted");
        assert!(
            connector.received().is_empty(),
            "a call that never reached the server is not received"
        );
    }

    #[tokio::test]
    async fn unknown_tool_also_consumes_a_programmed_unreachable() {
        let connector = two_tools().fail_unreachable(1);
        let first = connector.call_tool("absent", &json!({})).await;
        assert!(
            matches!(first, Err(McpError::Unreachable(_))),
            "the programmed unreachable failure comes before the unknown tool check, got {first:?}"
        );
        let second = connector.call_tool("absent", &json!({})).await;
        assert!(
            matches!(second, Err(McpError::Failed(_))),
            "once the programmed failures are consumed, an unknown tool is Failed, got {second:?}"
        );
    }

    // Scenario: erreur de tool
    #[tokio::test]
    async fn tool_error_is_failed_and_not_unreachable() {
        let connector = two_tools().fail_tool("boom");
        let result = connector.call_tool("boom", &json!({})).await;
        assert!(
            matches!(result, Err(McpError::Failed(_))),
            "an error of the tool must be Failed, got {result:?}"
        );
    }

    // Scenario: tool inconnu
    #[tokio::test]
    async fn unknown_tool_is_failed() {
        let connector = two_tools();
        let result = connector.call_tool("absent", &json!({})).await;
        assert!(
            matches!(result, Err(McpError::Failed(_))),
            "a tool the server does not have must be Failed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn listing_can_be_programmed_unreachable() {
        let connector = two_tools().listing_unreachable();
        let result = connector.list_tools().await;
        assert!(
            matches!(result, Err(McpError::Unreachable(_))),
            "a programmed listing failure must be Unreachable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn reconnect_is_counted_and_never_fails() {
        let connector = two_tools().fail_unreachable(1);
        connector.reconnect().await;
        connector.reconnect().await;
        assert_eq!(connector.reconnects(), 2, "each reconnection must be counted");
        let result = connector.call_tool("echo", &json!({})).await;
        assert!(
            matches!(result, Err(McpError::Unreachable(_))),
            "reconnecting does not consume the programmed failures, got {result:?}"
        );
    }

    #[test]
    fn mcp_tool_serializes_its_fields() {
        let value = serde_json::to_value(echo_tool()).expect("a tool serializes");
        assert_eq!(
            value,
            json!({
                "name": "echo",
                "description": "Echoes its input.",
                "input_schema": {"type": "object", "properties": {"msg": {"type": "string"}}}
            }),
            "a tool serializes with its name, description and input schema"
        );
    }

    #[test]
    fn errors_and_connectors_are_send_and_sync() {
        assert_send_sync::<McpError>();
        assert_send_sync::<McpTool>();
        assert_send_sync::<ScriptedConnector>();
    }

    #[test]
    fn error_texts_are_stable() {
        assert_eq!(
            McpError::Unreachable("timeout".to_owned()).to_string(),
            "the MCP server is unreachable: timeout"
        );
        assert_eq!(
            McpError::Failed("unknown tool".to_owned()).to_string(),
            "the MCP server failed: unknown tool"
        );
    }

    /// Calls the tool `echo` ten times through the shared connector.
    async fn call_ten_times(connector: Arc<ScriptedConnector>) {
        for _ in 0..10 {
            let _value = connector.call_tool("echo", &json!({})).await;
        }
    }

    #[tokio::test]
    async fn connector_is_shared_between_threads() {
        let connector = Arc::new(two_tools().with_result("echo", json!(1)));
        let workers: Vec<_> = (0..4)
            .map(|_| tokio::spawn(call_ten_times(Arc::clone(&connector))))
            .collect();
        for worker in workers {
            worker.await.expect("a calling task must not panic");
        }
        assert_eq!(
            connector.calls(),
            40,
            "forty calls from four tasks must all be counted"
        );
    }

    #[tokio::test]
    async fn poisoned_lock_is_recovered() {
        let connector = Arc::new(two_tools().with_result("echo", json!("after")));
        let poisoner = Arc::clone(&connector);
        let joined = thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poisoning the connector lock for the test");
        })
        .join();
        assert!(
            joined.is_err(),
            "the poisoning thread must panic while holding the lock"
        );
        let value = connector
            .call_tool("echo", &json!({}))
            .await
            .expect("a call on a poisoned connector is still answered");
        assert_eq!(
            value,
            json!("after"),
            "the programmed result is still given after the poisoning"
        );
        assert_eq!(
            connector.calls(),
            1,
            "the call after the poisoning must be counted"
        );
    }
}
