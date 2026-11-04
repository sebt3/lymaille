//! Rhai module of an MCP role, generated from `tools/list`, in two modes: the real module (each function calls the tool) and
//! the shell descriptor (same names, parameters and documentation, built from a listing-only connector so that no call is
//! possible). Arguments are checked against the JSON Schema of the tool before any call, a lost connection is retried with a
//! short backoff, and every call leaves a record (`agent-sessions/modules#FR-6`, `FR-9`, `FR-10`, `FR-11`, `AC-3` to `AC-7`).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jsonschema::Validator;
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Dynamic, EvalAltResult, FuncRegistration, Map, Module, Position};
use serde_json::Value;
use vynil_core::rt::block_on;

use super::{McpConnector, McpError, McpLister, McpTool, ModulesError, ModulesResult, name_tools};
use crate::clock::{Clock, Timestamp};
use crate::scratchpad::{FunctionDescriptor, ModuleDescriptor};
use crate::session::{Event, EventKind, EventSink, McpInput, McpRecord, Redactor, SessionId};

/// Retry policy of a lost connection: the number of attempts and the delay before the first one. The delay doubles at each
/// attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct McpRetryPolicy {
    attempts: u32,
    first_delay: Duration,
}

impl McpRetryPolicy {
    /// Builds a policy of `attempts` attempts, the first one after `first_delay`, then twice as long each time.
    #[must_use]
    pub const fn new(attempts: u32, first_delay: Duration) -> Self {
        Self {
            attempts,
            first_delay,
        }
    }

    /// Returns the delay before each attempt, in order: 1 s, 2 s and 4 s for the default policy. A delay that would overflow
    /// saturates.
    #[must_use]
    pub fn delays(&self) -> Vec<Duration> {
        let mut delay = self.first_delay;
        (0..self.attempts)
            .map(|_attempt| {
                let current = delay;
                delay = delay.saturating_mul(2);
                current
            })
            .collect()
    }
}

impl Default for McpRetryPolicy {
    /// Three attempts, after 1 s, 2 s and 4 s.
    fn default() -> Self {
        Self::new(3, Duration::from_secs(1))
    }
}

/// What an MCP module needs from its host: the redactor, the event channel, the clock, the session and the retry policy.
#[non_exhaustive]
pub struct McpEnv {
    redactor: Arc<Redactor>,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    session: SessionId,
    retry: McpRetryPolicy,
}

impl std::fmt::Debug for McpEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEnv")
            .field("session", &self.session)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl McpEnv {
    /// Builds the environment of an MCP module.
    #[must_use]
    pub fn new(
        redactor: Arc<Redactor>,
        sink: Arc<dyn EventSink>,
        clock: Arc<dyn Clock>,
        session: SessionId,
        retry: McpRetryPolicy,
    ) -> Self {
        Self {
            redactor,
            sink,
            clock,
            session,
            retry,
        }
    }
}

/// Builds the real Rhai module of the MCP role `role`: one function per tool of the server, taking one map.
///
/// The functions are synchronous and call the connector through `vynil_core::rt::block_on`.
///
/// # Errors
///
/// Returns [`ModulesError::InvalidMcp`] when the role is not a Rhai identifier, when the server cannot be listed, when two
/// tools map to the same function or have an unusable name, or when an input schema is not a compilable JSON Schema.
pub async fn mcp_module<C>(role: &str, connector: Arc<C>, env: McpEnv) -> ModulesResult<Module>
where
    C: McpConnector + 'static,
{
    let tools = list_named(role, &*connector).await?;
    let shared = Arc::new(Shared {
        role: role.to_owned(),
        connector,
        env,
        needs_reconnect: AtomicBool::new(false),
    });
    let mut module = Module::new();
    for (function, tool) in tools {
        let validator = jsonschema::validator_for(tool.input_schema()).map_err(|error| {
            refused(
                role,
                &format!(
                    "the input schema of tool `{}` is not usable: {}",
                    tool.name(),
                    schema_failure(&error)
                ),
            )
        })?;
        let call = Arc::new(ToolCall {
            shared: Arc::clone(&shared),
            function: function.clone(),
            original: tool.name().to_owned(),
            validator,
        });
        let comments: Vec<String> = documentation(role, &function, &tool)
            .lines()
            .map(|line| format!("/// {line}"))
            .collect();
        let _hash = FuncRegistration::new(function.as_str())
            .with_params_info(["arguments: Map", "Dynamic"])
            .with_comments(comments)
            .set_into_module(
                &mut module,
                move |arguments: Map| -> Result<Dynamic, Box<EvalAltResult>> { call.invoke(arguments) },
            );
    }
    Ok(module)
}

/// Builds the descriptor of the shell of the MCP role `role`.
///
/// The shell has the function names, the parameter and the documentation of the real module. Turn the descriptor into a
/// module with [`shell_module`](crate::scratchpad::shell_module). It only needs a listing connector, so no call to the
/// server is possible.
///
/// # Errors
///
/// Returns [`ModulesError::InvalidMcp`] when the role is not a Rhai identifier, when the server cannot be listed (the message
/// names the role), or when two tools map to the same function or have an unusable name.
pub async fn mcp_shell_descriptor<L>(role: &str, lister: &L) -> ModulesResult<ModuleDescriptor>
where
    L: McpLister,
{
    let tools = list_named(role, lister).await?;
    let mut descriptor = ModuleDescriptor::new(role)
        .ok_or_else(|| refused(role, "the role is not a valid Rhai identifier"))?;
    for (function, tool) in tools {
        descriptor = descriptor.with_function(FunctionDescriptor::new(
            &function,
            &["arguments"],
            Some(&documentation(role, &function, &tool)),
        ));
    }
    Ok(descriptor)
}

/// The refusal of the MCP module of `role`.
fn refused(role: &str, reason: &str) -> ModulesError {
    ModulesError::InvalidMcp {
        role: role.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Lists the tools of the server and pairs each with its function name. The role must be a Rhai identifier.
async fn list_named<L: McpLister>(role: &str, lister: &L) -> ModulesResult<Vec<(String, McpTool)>> {
    if ModuleDescriptor::new(role).is_none() {
        return Err(refused(role, "the role is not a valid Rhai identifier"));
    }
    let tools = lister
        .list_tools()
        .await
        .map_err(|_error| refused(role, "the tools of the server cannot be listed"))?;
    let labels: Vec<&str> = tools.iter().map(McpTool::name).collect();
    let pairs = name_tools(role, &labels)?;
    Ok(pairs
        .into_iter()
        .map(|(function, _original)| function)
        .zip(tools)
        .collect())
}

/// The documentation of a function: the description of the tool, its original MCP name, its usage line, the names of its
/// optional fields and its input schema (`agent-sessions/modules#FR-12`).
fn documentation(role: &str, function: &str, tool: &McpTool) -> String {
    format!(
        "{}\nMCP tool: {}\n{}\nInput schema: {}",
        tool.description().replace('\n', " "),
        tool.name(),
        usage_block(role, function, tool.input_schema()),
        tool.input_schema()
    )
}

/// Fields named by the usage line or the optional fields line, at most.
const MAX_USAGE_FIELDS: usize = 12;

/// Characters of the usage line, at most.
const MAX_USAGE_LINE: usize = 400;

/// The usage line of `role::function` and, when the schema has optional properties, the `Optional fields: ` line that
/// follows it. Neither a default, nor an enumeration, nor a description of the schema enters these lines
/// (`agent-sessions#CON-3`).
fn usage_block(role: &str, function: &str, schema: &Value) -> String {
    let required = required_fields(schema);
    let entries: Vec<String> = required.iter().map(|field| usage_entry(field, schema)).collect();
    let mut shown = entries.len().min(MAX_USAGE_FIELDS);
    let mut cut = entries.len() > shown;
    while shown > 0 && usage_line(role, function, &entries, shown, cut).chars().count() > MAX_USAGE_LINE {
        shown = shown.saturating_sub(1);
        cut = true;
    }
    let mut block = usage_line(role, function, &entries, shown, cut);
    let (optional, more) = optional_fields(schema, &required);
    if !optional.is_empty() {
        let joined = optional.join(", ");
        let names = if more {
            format!("{joined}, \u{2026}")
        } else {
            joined
        };
        let _written = write!(block, "\nOptional fields: {names}");
    }
    block
}

/// One usage line: `Usage: {role}::{function}(#{ fields })`, the prefix read by the introspection
/// (../scratchpad/introspect.rs), for the first `shown` entries. When fields were cut, the line closes after the last
/// whole one with `, …`.
fn usage_line(role: &str, function: &str, entries: &[String], shown: usize, cut: bool) -> String {
    let mut body = entries
        .iter()
        .take(shown)
        .map(String::as_str)
        .collect::<Vec<&str>>()
        .join(", ");
    if cut {
        body = if body.is_empty() {
            "\u{2026}".to_owned()
        } else {
            format!("{body}, \u{2026}")
        };
    }
    let map = if body.is_empty() {
        "#{}".to_owned()
    } else {
        format!("#{{ {body} }}")
    };
    format!("Usage: {role}::{function}({map})")
}

/// The required properties of a schema, in the order of its `required` array; non-string entries are left out.
fn required_fields(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The names of the properties that are not required, sorted, twelve at most; plus whether names were left out,
/// which the caller signals with the same `, \u{2026}` as the usage line.
fn optional_fields(schema: &Value, required: &[&str]) -> (Vec<String>, bool) {
    let mut optional: Vec<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .keys()
                .filter(|key| !required.iter().any(|field| field == *key))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    optional.sort();
    let more = optional.len() > MAX_USAGE_FIELDS;
    optional.truncate(MAX_USAGE_FIELDS);
    (optional, more)
}

/// One `field: example` entry: the name quoted when it is not a Rhai identifier, the example given by the declared type.
fn usage_entry(field: &str, schema: &Value) -> String {
    let name = if rhai::is_valid_function_name(field) {
        field.to_owned()
    } else {
        format!("\"{field}\"")
    };
    let example = match schema
        .get("properties")
        .and_then(|properties| properties.get(field))
        .and_then(|property| property.get("type"))
        .and_then(Value::as_str)
    {
        Some("string") => "\"\u{2026}\"",
        Some("integer") => "0",
        Some("number") => "0.0",
        Some("boolean") => "false",
        Some("array") => "[]",
        Some("object") => "#{}",
        _ => "()",
    };
    format!("{name}: {example}")
}

/// Why a schema does not compile, without any value of an instance: its kind and the place in the schema.
fn schema_failure(error: &jsonschema::ValidationError<'_>) -> String {
    format!("rule `{}` at `{}`", error.kind().keyword(), error.schema_path())
}

/// What the functions of one role share: the connector, the host environment and the flag of a lost connection.
struct Shared<C> {
    role: String,
    connector: Arc<C>,
    env: McpEnv,
    needs_reconnect: AtomicBool,
}

/// One tool of the module: its function name, its original MCP name and the compiled schema of its input.
struct ToolCall<C> {
    shared: Arc<Shared<C>>,
    function: String,
    original: String,
    validator: Validator,
}

/// How an attempt to call the tool ended badly.
enum CallFailure {
    /// The server never answered, after this many attempts.
    Unreachable(u32),
    /// The server answered with an error; the text is the server's own, not yet masked.
    Failed(String),
}

impl<C: McpConnector> ToolCall<C> {
    /// The body of the Rhai function: validates, calls, records.
    fn invoke(&self, arguments: Map) -> Result<Dynamic, Box<EvalAltResult>> {
        let shared = &self.shared;
        let started = shared.env.clock.now();
        let Ok(json) = from_dynamic::<Value>(&Dynamic::from(arguments)) else {
            self.record(&Value::Null, "invalid", started);
            return Err(Box::new(self.runtime_error("the arguments are not a JSON value")));
        };
        if let Some(problems) = self.problems(&json) {
            self.record(&json, "invalid", started);
            return Err(Box::new(
                self.runtime_error(&format!("invalid arguments: {problems}")),
            ));
        }
        let outcome = match block_on(self.call(&json)) {
            Ok(outcome) => outcome,
            Err(error) => return Err(vynil_core::rhai_err(error)),
        };
        match outcome {
            Ok(value) => {
                self.record(&json, "ok", started);
                to_dynamic(value).map_err(|_error| {
                    Box::new(self.runtime_error("the answer of the tool cannot be converted"))
                })
            }
            Err(CallFailure::Failed(text)) => {
                self.record(&json, "error", started);
                let masked = shared.env.redactor.redact(&text);
                Err(Box::new(
                    self.runtime_error(&format!("the tool failed: {masked}")),
                ))
            }
            Err(CallFailure::Unreachable(attempts)) => {
                self.record(&json, "unreachable", started);
                Err(Box::new(self.runtime_error(&format!(
                    "the MCP server `{}` is unreachable after {attempts} attempts",
                    shared.role
                ))))
            }
        }
    }

    /// The violations of the schema by `arguments`, as field paths and rule names (never a value), or nothing when valid.
    fn problems(&self, arguments: &Value) -> Option<String> {
        let found: Vec<String> = self
            .validator
            .iter_errors(arguments)
            .map(|error| {
                let kind = error.kind();
                let mut path = error.instance_path().to_string();
                if let jsonschema::error::ValidationErrorKind::Required { property } = kind
                    && let Some(name) = property.as_str()
                {
                    path = format!("{path}/{name}");
                }
                let place = if path.is_empty() { "(root)" } else { path.as_str() };
                format!("field `{place}` breaks the rule `{}`", kind.keyword())
            })
            .collect();
        if found.is_empty() {
            None
        } else {
            Some(found.join("; "))
        }
    }

    /// Calls the tool: reconnects first when the last call lost the server, retries an unreachable server per the policy.
    /// An error of the tool is never retried.
    async fn call(&self, arguments: &Value) -> Result<Value, CallFailure> {
        let shared = &self.shared;
        if shared.needs_reconnect.load(Ordering::Acquire) {
            shared.connector.reconnect().await;
            shared.needs_reconnect.store(false, Ordering::Release);
        }
        match self.attempt(arguments).await {
            Err(McpError::Unreachable(_)) => {}
            other => return other.map_err(failure_of),
        }
        let delays = shared.env.retry.delays();
        let mut attempts: u32 = 0;
        for delay in delays {
            attempts = attempts.saturating_add(1);
            self.emit(EventKind::McpReconnect {
                role: shared.role.clone(),
                attempt: attempts,
                delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            });
            tokio::time::sleep(delay).await;
            shared.connector.reconnect().await;
            match self.attempt(arguments).await {
                Err(McpError::Unreachable(_)) => {}
                other => return other.map_err(failure_of),
            }
        }
        shared.needs_reconnect.store(true, Ordering::Release);
        Err(CallFailure::Unreachable(attempts))
    }

    /// One call to the tool by its original name.
    async fn attempt(&self, arguments: &Value) -> Result<Value, McpError> {
        self.shared.connector.call_tool(&self.original, arguments).await
    }

    /// Emits the record of a call, dated and timed by the injected clock.
    fn record(&self, arguments: &Value, status: &str, started: Timestamp) {
        let shared = &self.shared;
        let ended = shared.env.clock.now();
        let duration = Duration::from_millis(ended.as_millis().saturating_sub(started.as_millis()));
        let input = McpInput::new(&shared.role, &self.function, arguments, status, duration);
        let record = McpRecord::new(&shared.env.redactor, input);
        shared.env.sink.emit(Event::new(
            shared.env.session.clone(),
            ended,
            EventKind::McpCall(record),
        ));
    }

    /// Emits an event of this role.
    fn emit(&self, kind: EventKind) {
        let env = &self.shared.env;
        env.sink
            .emit(Event::new(env.session.clone(), env.clock.now(), kind));
    }

    /// A runtime error of the script that names the role and the function.
    fn runtime_error(&self, message: &str) -> EvalAltResult {
        EvalAltResult::ErrorRuntime(
            Dynamic::from(format!("{}::{}: {message}", self.shared.role, self.function)),
            Position::NONE,
        )
    }
}

/// The failure of a call that is not an unreachable server.
fn failure_of(error: McpError) -> CallFailure {
    match error {
        McpError::Failed(text) | McpError::Unreachable(text) => CallFailure::Failed(text),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use rhai::{Array, Dynamic, Engine as RhaiEngine, EvalAltResult, Map, Module};
    use serde_json::{Value, json};
    use vynil_core::cancel::{CancelToken, scope};

    use super::{McpEnv, McpRetryPolicy, mcp_module, mcp_shell_descriptor};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{McpConnector, McpError, McpLister, McpTool, ModulesError, ScriptedConnector};
    use crate::scratchpad::{ScratchpadError, shell_module};
    use crate::session::{EventKind, EventSink, McpRecord, RecordingSink, Redactor, SessionId};

    /// A clock that moves forward by 5 ms each time it is read.
    struct StepClock(AtomicU64);

    impl Clock for StepClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(self.0.fetch_add(5, Ordering::Relaxed))
        }
    }

    fn loki_tool() -> McpTool {
        McpTool::new(
            "loki.query",
            "Runs a Loki query.",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "LogQL expression"}},
                "required": ["query"]
            }),
        )
    }

    fn pods_tool() -> McpTool {
        McpTool::new("get-pods", "Lists the pods.", json!({"type": "object"}))
    }

    fn two_tools() -> ScriptedConnector {
        ScriptedConnector::new(vec![loki_tool(), pods_tool()])
    }

    /// Delays small enough to run for real, with the shape of the default policy.
    fn quick() -> McpRetryPolicy {
        McpRetryPolicy::new(3, Duration::from_millis(10))
    }

    fn env_with(
        sink: &Arc<RecordingSink>,
        redactor: Redactor,
        clock: Arc<dyn Clock>,
        retry: McpRetryPolicy,
    ) -> McpEnv {
        let recording: Arc<RecordingSink> = Arc::clone(sink);
        let dyn_sink: Arc<dyn EventSink> = recording;
        McpEnv::new(
            Arc::new(redactor),
            dyn_sink,
            clock,
            SessionId::new("s-1").expect("a non-empty identifier"),
            retry,
        )
    }

    fn env(sink: &Arc<RecordingSink>, retry: McpRetryPolicy) -> McpEnv {
        env_with(
            sink,
            Redactor::new(),
            Arc::new(ManualClock::new(Timestamp::from_millis(1_000))),
            retry,
        )
    }

    async fn engine_with<C: McpConnector + 'static>(connector: &Arc<C>, env: McpEnv) -> RhaiEngine {
        let module = mcp_module("grafana", Arc::clone(connector), env)
            .await
            .expect("the module of a reachable server builds");
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("grafana", Arc::new(module));
        engine
    }

    fn error_text<T>(result: Result<T, Box<EvalAltResult>>) -> String {
        match result {
            Ok(_) => panic!("the script must fail"),
            Err(error) => error.to_string(),
        }
    }

    fn no_reconnects() -> Vec<(String, u32, u64)> {
        Vec::new()
    }

    fn calls_of(sink: &RecordingSink) -> Vec<McpRecord> {
        sink.events()
            .into_iter()
            .filter_map(|event| {
                if let EventKind::McpCall(record) = event.kind {
                    Some(record)
                } else {
                    None
                }
            })
            .collect()
    }

    fn reconnects_of(sink: &RecordingSink) -> Vec<(String, u32, u64)> {
        sink.events()
            .into_iter()
            .filter_map(|event| {
                if let EventKind::McpReconnect {
                    role,
                    attempt,
                    delay_ms,
                } = event.kind
                {
                    Some((role, attempt, delay_ms))
                } else {
                    None
                }
            })
            .collect()
    }

    /// The documentation lines of each function of a module, by function name, as an engine reports them.
    fn docs(module: &Module) -> BTreeMap<String, Vec<String>> {
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("probe", Arc::new(module.clone()));
        let json: Value = serde_json::from_str(
            &engine
                .gen_fn_metadata_to_json(false)
                .expect("the metadata of an engine serializes"),
        )
        .expect("the metadata is JSON");
        let functions = json["modules"]["probe"]["functions"]
            .as_array()
            .expect("a list of functions");
        functions
            .iter()
            .map(|function| {
                let lines = function["docComments"]
                    .as_array()
                    .map(|lines| {
                        lines
                            .iter()
                            .map(|line| line.as_str().unwrap_or_default().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                (function["name"].as_str().unwrap_or_default().to_owned(), lines)
            })
            .collect()
    }

    fn joined(docs: &BTreeMap<String, Vec<String>>, name: &str) -> String {
        docs.get(name).map(|lines| lines.join("\n")).unwrap_or_default()
    }

    // Scenario: un module de deux fonctions documentées
    #[tokio::test(flavor = "multi_thread")]
    async fn module_has_two_documented_functions() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let module = mcp_module("grafana", connector, env(&sink, quick()))
            .await
            .expect("the module builds");
        let docs = docs(&module);
        assert_eq!(docs.len(), 2, "one function per tool: {docs:?}");
        assert!(joined(&docs, "loki_query").contains("Runs a Loki query."));
        assert!(joined(&docs, "get_pods").contains("Lists the pods."));
    }

    // Scenario: nommage des fonctions
    #[tokio::test(flavor = "multi_thread")]
    async fn functions_are_named_after_the_tools_and_document_the_original_name() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        succeeds(
            &engine,
            r#"grafana::loki_query(#{query: "up"})"#,
            "grafana::loki_query exists",
        );
        succeeds(&engine, "grafana::get_pods(#{})", "grafana::get_pods exists");
        let module = mcp_module("grafana", connector, env(&sink, quick()))
            .await
            .expect("the module builds");
        let docs = docs(&module);
        assert!(joined(&docs, "loki_query").contains("loki.query"));
        assert!(joined(&docs, "get_pods").contains("get-pods"));
    }

    // Scenario: collision de noms au chargement
    #[tokio::test(flavor = "multi_thread")]
    async fn colliding_tool_names_fail_the_generation() {
        let connector = Arc::new(ScriptedConnector::new(vec![
            McpTool::new("a.b", "first", json!({"type": "object"})),
            McpTool::new("a_b", "second", json!({"type": "object"})),
        ]));
        let sink = Arc::new(RecordingSink::new());
        let error = mcp_module("grafana", connector, env(&sink, quick()))
            .await
            .expect_err("two tools map to one function");
        assert!(matches!(error, ModulesError::InvalidMcp { .. }));
        let text = error.to_string();
        assert!(text.contains("a.b") && text.contains("a_b"), "{text}");
    }

    /// The text of the refusal of a module for `role` over `connector`, which must be an `InvalidMcp`.
    async fn refusal_text(role: &str, connector: ScriptedConnector) -> String {
        let sink = Arc::new(RecordingSink::new());
        let error = mcp_module(role, Arc::new(connector), env(&sink, quick()))
            .await
            .expect_err("the module must be refused");
        assert!(matches!(error, ModulesError::InvalidMcp { .. }), "{error}");
        error.to_string()
    }

    /// Runs a script that must succeed.
    fn succeeds(engine: &RhaiEngine, script: &str, reason: &str) {
        let outcome = engine.eval::<Dynamic>(script);
        assert!(outcome.is_ok(), "{reason}: {outcome:?}");
    }

    // Raises: role name, schema and listing refusals
    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_role_schema_and_listing_are_refused() {
        let _role = refusal_text("bad-role", two_tools()).await;
        for schema in [
            json!({"type": 12}),
            json!({"$ref": "http://example.invalid/schema.json"}),
        ] {
            let text = refusal_text(
                "grafana",
                ScriptedConnector::new(vec![McpTool::new("t", "d", schema)]),
            )
            .await;
            assert!(text.contains("grafana"), "{text}");
        }
        let text = refusal_text("grafana", two_tools().listing_unreachable()).await;
        assert!(text.contains("grafana"), "{text}");
    }

    // Scenario: argument invalide refusé avant l'appel
    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_argument_is_refused_before_any_call() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let text = error_text(engine.eval::<Dynamic>("grafana::loki_query(#{query: 42})"));
        assert!(
            text.contains("field `/query` breaks the rule `type`"),
            "the exact field path and the rule are named: {text}"
        );
        assert!(text.contains("grafana") && text.contains("loki_query"), "{text}");
        assert!(text.contains("type"), "the rule is named: {text}");
        assert!(!text.contains("42"), "the value must not appear: {text}");
        assert_eq!(connector.calls(), 0, "no call reaches the connector");
        let records = calls_of(&sink);
        assert_eq!(records.len(), 1, "one record");
        assert_eq!(records[0].status, "invalid");
        assert_eq!(reconnects_of(&sink), no_reconnects());
    }

    // Scenario: argument invalide refusé avant l'appel (champ manquant)
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_required_field_names_the_field() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let text = error_text(engine.eval::<Dynamic>("grafana::loki_query(#{})"));
        assert!(
            text.contains("field `/query` breaks the rule `required`"),
            "the missing property is part of the path: {text}"
        );
        assert_eq!(connector.calls(), 0);
    }

    // Scenario: la documentation reprend le schéma
    #[tokio::test(flavor = "multi_thread")]
    async fn documentation_carries_the_schema() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let module = mcp_module("grafana", connector, env(&sink, quick()))
            .await
            .expect("the module builds");
        let doc = joined(&docs(&module), "loki_query");
        assert!(doc.contains("query") && doc.contains("LogQL expression"), "{doc}");
        assert!(doc.contains("\"properties\""), "the serialized schema: {doc}");
    }

    /// The documentation of `function` in `module`, with the `///` markers stripped, as the introspection reads it.
    fn doc_text(module: &Module, function: &str) -> String {
        joined(&docs(module), function)
            .lines()
            .map(|line| line.trim_start_matches('/').trim().to_owned())
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Builds the module of `role` over a single `tool` and returns the documentation of its function, markers stripped.
    async fn doc_of_tool(role: &str, function: &str, tool: McpTool) -> String {
        let connector = Arc::new(ScriptedConnector::new(vec![tool]));
        let sink = Arc::new(RecordingSink::new());
        let module = mcp_module(role, connector, env(&sink, quick()))
            .await
            .expect("the module builds");
        doc_text(&module, function)
    }

    /// The first line of `doc` that starts with the `Usage: ` prefix, as introspection reads it.
    fn usage_line_of(doc: &str) -> String {
        doc.lines()
            .find(|line| line.starts_with("Usage: "))
            .unwrap_or_else(|| panic!("the documentation must carry a usage line: {doc}"))
            .to_owned()
    }

    /// The number of `field: example` entries a usage line names.
    fn named_fields(line: &str) -> usize {
        line.split(", ").filter(|entry| entry.contains(": ")).count()
    }

    /// A field name of exactly `width` characters and a valid Rhai identifier, to land a line on an exact bound.
    fn padded_name(index: usize, width: usize) -> String {
        let head = format!("f{index:02}");
        format!(
            "{}{}",
            head,
            "0".repeat(width.saturating_sub(head.chars().count()))
        )
    }

    /// The tool `probe` whose required string fields are `names`, in that order.
    fn probe_with_fields(names: &[String]) -> McpTool {
        let mut properties = serde_json::Map::new();
        for name in names {
            let _inserted = properties.insert(name.clone(), json!({"type": "string"}));
        }
        McpTool::new(
            "probe",
            "Bounded probe.",
            json!({"type": "object", "properties": properties, "required": names}),
        )
    }

    /// The tool of the role `sandbox`: required string `path`, optional integer `limit`.
    fn read_file_tool() -> McpTool {
        McpTool::new(
            "read_file",
            "Reads a file.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}, "limit": {"type": "integer"}},
                "required": ["path"]
            }),
        )
    }

    // Scenario: la documentation porte la ligne d'usage
    #[tokio::test(flavor = "multi_thread")]
    async fn documentation_carries_the_usage_line() {
        let doc = doc_of_tool("sandbox", "read_file", read_file_tool()).await;
        let lines: Vec<&str> = doc.lines().collect();
        let at = lines
            .iter()
            .position(|line| *line == "Usage: sandbox::read_file(#{ path: \"\u{2026}\" })")
            .unwrap_or_else(|| panic!("the exact usage line must appear: {doc}"));
        assert_eq!(
            lines.get(at + 1).copied(),
            Some("Optional fields: limit"),
            "the optional fields line follows the usage line: {doc}"
        );
    }

    // Scenario: les exemples suivent le type de chaque champ obligatoire
    #[tokio::test(flavor = "multi_thread")]
    async fn usage_examples_follow_the_type_of_each_required_field() {
        let tool = McpTool::new(
            "probe",
            "Typed probe.",
            json!({
                "type": "object",
                "properties": {
                    "s": {"type": "string"},
                    "i": {"type": "integer"},
                    "n": {"type": "number"},
                    "b": {"type": "boolean"},
                    "a": {"type": "array"},
                    "o": {"type": "object"},
                    "u": {"description": "no type"}
                },
                "required": ["s", "i", "n", "b", "a", "o", "u"]
            }),
        );
        let doc = doc_of_tool("grafana", "probe", tool).await;
        assert_eq!(
            usage_line_of(&doc),
            "Usage: grafana::probe(#{ s: \"\u{2026}\", i: 0, n: 0.0, b: false, a: [], o: #{}, u: () })"
        );
    }

    // Scenario: un nom de champ qui n'est pas un identifiant est entre guillemets
    #[tokio::test(flavor = "multi_thread")]
    async fn non_identifier_field_name_is_quoted_in_the_usage_line() {
        let tool = McpTool::new(
            "probe",
            "Dashed field.",
            json!({
                "type": "object",
                "properties": {"my-field": {"type": "string"}},
                "required": ["my-field"]
            }),
        );
        let doc = doc_of_tool("grafana", "probe", tool).await;
        assert_eq!(
            usage_line_of(&doc),
            "Usage: grafana::probe(#{ \"my-field\": \"\u{2026}\" })"
        );
    }

    // Scenario: sans champ obligatoire
    #[tokio::test(flavor = "multi_thread")]
    async fn tool_without_required_field_has_an_empty_map_usage() {
        let tool = McpTool::new("tool", "Takes nothing.", json!({"type": "object"}));
        let doc = doc_of_tool("sandbox", "tool", tool).await;
        assert_eq!(usage_line_of(&doc), "Usage: sandbox::tool(#{})");
    }

    /// The default value the schema of one field leaks, and the schema property of one field of the bounded tool: one
    /// carries a default, another an enumeration (`agent-sessions#CON-3` must keep both out of the usage line).
    const LEAKY_DEFAULT: &str = "d\u{e9}faut-secret-x";

    /// The schema property of field number `index` of the bounded tool: a string, one with a leaking default, one with a
    /// leaking enumeration.
    fn leaky_property(index: u32) -> Value {
        match index {
            3 => json!({"type": "string", "default": LEAKY_DEFAULT}),
            5 => json!({"type": "string", "enum": ["valeur-enum-x"]}),
            _ => json!({"type": "string"}),
        }
    }

    // Scenario: la ligne d'usage est bornée et ne divulgue ni défaut ni énumération
    #[tokio::test(flavor = "multi_thread")]
    async fn usage_line_is_bounded_and_leaks_neither_default_nor_enum() {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();
        for index in 0..30 {
            let name = format!("champ_obligatoire_de_sandbox_tres_long_{index:02}");
            let _inserted = properties.insert(name.clone(), leaky_property(index));
            required.push(name);
        }
        let tool = McpTool::new(
            "probe",
            "Many fields.",
            json!({"type": "object", "properties": properties, "required": required}),
        );
        let doc = doc_of_tool("grafana", "probe", tool).await;
        let line = usage_line_of(&doc);
        assert!(line.starts_with("Usage: "), "{line}");
        assert!(
            line.chars().count() <= 400,
            "the line holds at most 400 characters: {}",
            line.chars().count()
        );
        let named = line.split(", ").filter(|entry| entry.contains(": ")).count();
        assert!(named <= 12, "at most twelve fields are named: {named}");
        assert!(named < 30, "the line was cut: {named} fields");
        assert!(line.ends_with(", \u{2026} })"), "the cut is marked: {line}");
        assert!(!line.contains(LEAKY_DEFAULT), "{line}");
        assert!(!line.contains("valeur-enum-x"), "{line}");
    }

    // Scenario: la ligne d'usage est bornée (une ligne qui tient exactement à 400 caracteres n'est jamais coupee)
    #[tokio::test(flavor = "multi_thread")]
    async fn usage_line_of_exactly_400_characters_is_not_cut() {
        let filler = padded_name(12, 257);
        let mut names: Vec<String> = (1..=11).map(|index| format!("n{index:02}")).collect();
        names.push(filler.clone());
        let doc = doc_of_tool("grafana", "probe", probe_with_fields(&names)).await;
        let line = usage_line_of(&doc);
        // Fixed shape ("Usage: grafana::probe(#" and " })") plus 12 names, 12 * 5 examples (": " and "\"\u{2026}\"")
        // and 11 separators: 28 + 33 + 257 + 12 * 5 + 11 * 2 = 400 characters exactly.
        assert_eq!(
            line.chars().count(),
            400,
            "the line must land on the bound: {line}"
        );
        assert_eq!(
            named_fields(&line),
            12,
            "a line that fits shows every field: {line}"
        );
        assert!(
            !line.contains(", \u{2026}"),
            "nothing is omitted, nothing is signaled: {line}"
        );
        assert!(line.contains(&filler), "the twelfth field is named: {line}");
    }

    // Scenario: la ligne d'usage est bornée (une ligne qui deborde se coupe apres le dernier champ entier qui tient)
    #[tokio::test(flavor = "multi_thread")]
    async fn usage_line_over_400_characters_cuts_after_the_last_whole_field() {
        let filler = padded_name(12, 258);
        let mut names: Vec<String> = (1..=11).map(|index| format!("n{index:02}")).collect();
        names.push(filler.clone());
        let doc = doc_of_tool("grafana", "probe", probe_with_fields(&names)).await;
        let line = usage_line_of(&doc);
        // One character more than the bound above: the whole line would take 401 characters, the eleven short
        // fields plus ", \u{2026}" take 139. Maximality: adding the filler back would overflow past 400.
        assert!(
            line.ends_with(", \u{2026} })"),
            "the omission is signaled: {line}"
        );
        assert_eq!(
            named_fields(&line),
            11,
            "the cut keeps the most fields that still fit: {line}"
        );
        assert!(
            !line.contains(&filler),
            "the next field would not have fitted: {line}"
        );
        assert!(
            line.chars().count() <= 400,
            "{} characters, at most 400",
            line.chars().count()
        );
    }

    // Scenario: la ligne d'usage est bornée (plafond de douze champs obligatoires)
    #[tokio::test(flavor = "multi_thread")]
    async fn usage_line_names_twelve_required_fields_at_most() {
        for total in [13usize, 14] {
            let names: Vec<String> = (1..=total).map(|index| format!("q{index:02}")).collect();
            let doc = doc_of_tool("grafana", "probe", probe_with_fields(&names)).await;
            let line = usage_line_of(&doc);
            assert_eq!(
                named_fields(&line),
                12,
                "twelve fields at most, whether there are 13 or 14: {line}"
            );
            assert!(
                line.ends_with(", \u{2026} })"),
                "the omission is signaled: {line}"
            );
            assert!(!line.contains("q13"), "the thirteenth field is left out: {line}");
            assert!(
                line.chars().count() < 400,
                "twelve short names fit well under 400: the cap cut, not the bound: {line}"
            );
        }
    }

    // Scenario: la documentation porte la ligne d'usage (plafond de douze noms facultatifs, meme signal)
    #[tokio::test(flavor = "multi_thread")]
    async fn optional_fields_line_names_twelve_at_most() {
        let mut properties = serde_json::Map::new();
        let _first_inserted = properties.insert("req".to_owned(), json!({"type": "string"}));
        for index in 1..=13 {
            let _inserted = properties.insert(format!("opt_{index:02}"), json!({"type": "integer"}));
        }
        let tool = McpTool::new(
            "probe",
            "Optional probe.",
            json!({"type": "object", "properties": properties, "required": ["req"]}),
        );
        let doc = doc_of_tool("grafana", "probe", tool).await;
        let line = doc
            .lines()
            .find(|line| line.starts_with("Optional fields: "))
            .unwrap_or_else(|| panic!("the optional fields line must appear: {doc}"));
        let shown: Vec<String> = (1..=12).map(|index| format!("opt_{index:02}")).collect();
        assert_eq!(
            line,
            format!("Optional fields: {}, \u{2026}", shown.join(", ")),
            "alphabetical, twelve at most, the omission signaled"
        );
        assert!(!line.contains("opt_13"), "{line}");
    }

    // Scenario: la documentation porte la ligne d'usage (douze noms facultatifs exactement, ligne complete sans signal)
    #[tokio::test(flavor = "multi_thread")]
    async fn optional_fields_line_of_exactly_twelve_names_is_complete() {
        let mut properties = serde_json::Map::new();
        let _req_inserted = properties.insert("req".to_owned(), json!({"type": "string"}));
        let expected: Vec<String> = (1..=12).map(|index| format!("opt_{index:02}")).collect();
        for name in &expected {
            let _inserted = properties.insert(name.clone(), json!({"type": "integer"}));
        }
        let tool = McpTool::new(
            "probe",
            "Optional probe.",
            json!({"type": "object", "properties": properties, "required": ["req"]}),
        );
        let doc = doc_of_tool("grafana", "probe", tool).await;
        let line = doc
            .lines()
            .find(|line| line.starts_with("Optional fields: "))
            .unwrap_or_else(|| panic!("the optional fields line must appear: {doc}"));
        assert_eq!(
            line,
            format!("Optional fields: {}", expected.join(", ")),
            "twelve names are none left out: alphabetical, whole, no signal"
        );
        assert!(
            !line.contains('\u{2026}'),
            "nothing is omitted, nothing is signaled: {line}"
        );
    }

    // Scenario: la coquille porte la même ligne d'usage
    #[tokio::test(flavor = "multi_thread")]
    async fn shell_carries_the_same_usage_line() {
        let connector = Arc::new(ScriptedConnector::new(vec![read_file_tool()]));
        let sink = Arc::new(RecordingSink::new());
        let real = mcp_module("sandbox", Arc::clone(&connector), env(&sink, quick()))
            .await
            .expect("the module builds");
        let descriptor = mcp_shell_descriptor("sandbox", &*connector)
            .await
            .expect("the descriptor builds");
        let shell = shell_module(&descriptor);
        let real_doc = doc_text(&real, "read_file");
        let shell_doc = doc_text(&shell, "read_file");
        assert_eq!(
            usage_line_of(&shell_doc),
            usage_line_of(&real_doc),
            "the shell declares the same call"
        );
        assert!(
            shell_doc.lines().any(|line| line == "Optional fields: limit"),
            "the shell names the optional fields too: {shell_doc}"
        );
    }

    // Scenario: appel réussi
    #[tokio::test(flavor = "multi_thread")]
    async fn successful_call_returns_the_value_and_records_ok() {
        let connector = Arc::new(two_tools().with_result("loki.query", json!({"rows": [1, 2]})));
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let result: Map = engine
            .eval(r#"grafana::loki_query(#{query: "up"})"#)
            .expect("the call succeeds");
        let rows = result["rows"].clone().into_array().expect("rows is an array");
        assert_eq!(rows.len(), 2);
        assert_eq!(connector.received(), vec![json!({"query": "up"})]);
        let records = calls_of(&sink);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, "ok");
        assert_eq!(records[0].role, "grafana");
        assert_eq!(records[0].function, "loki_query");
        assert_eq!(records[0].arguments, json!({"query": "up"}));
    }

    // Must: the duration is measured by the injected clock
    #[tokio::test(flavor = "multi_thread")]
    async fn duration_comes_from_the_injected_clock() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let clock: Arc<dyn Clock> = Arc::new(StepClock(AtomicU64::new(100)));
        let engine = engine_with(&connector, env_with(&sink, Redactor::new(), clock, quick())).await;
        succeeds(&engine, "grafana::get_pods(#{})", "the call succeeds");
        assert_eq!(calls_of(&sink)[0].duration_ms, 5);
    }

    // Scenario: reconnexion puis erreur explicite
    #[tokio::test(flavor = "multi_thread")]
    async fn unreachable_server_is_retried_then_reported() {
        let connector = Arc::new(two_tools().fail_unreachable(100));
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let text = error_text(engine.eval::<Dynamic>(r#"grafana::loki_query(#{query: "up"})"#));
        assert!(text.contains("grafana") && text.contains('3'), "{text}");
        assert_eq!(
            reconnects_of(&sink),
            vec![
                ("grafana".to_owned(), 1, 10),
                ("grafana".to_owned(), 2, 20),
                ("grafana".to_owned(), 3, 40)
            ]
        );
        assert_eq!(connector.reconnects(), 3);
        assert_eq!(connector.calls(), 4, "the first call and one per attempt");
        assert_eq!(calls_of(&sink)[0].status, "unreachable");
    }

    // Scenario: reconnexion puis erreur explicite (politique par défaut)
    #[test]
    fn default_policy_delays_are_one_two_four_seconds() {
        let policy = McpRetryPolicy::default();
        assert_eq!(
            policy.delays(),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
        assert_eq!(
            McpRetryPolicy::new(2, Duration::from_millis(10)).delays(),
            vec![Duration::from_millis(10), Duration::from_millis(20)]
        );
        assert_eq!(
            McpRetryPolicy::new(0, Duration::from_secs(1)).delays(),
            Vec::<Duration>::new()
        );
        assert_eq!(
            McpRetryPolicy::new(100, Duration::from_secs(1)).delays().len(),
            100
        );
    }

    // Must: no attempt left, the error says so
    #[tokio::test(flavor = "multi_thread")]
    async fn zero_attempts_fail_at_once() {
        let connector = Arc::new(two_tools().fail_unreachable(1));
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, McpRetryPolicy::new(0, Duration::ZERO))).await;
        let text = error_text(engine.eval::<Dynamic>("grafana::get_pods(#{})"));
        assert!(text.contains("grafana") && text.contains('0'), "{text}");
        assert_eq!(reconnects_of(&sink), no_reconnects());
    }

    // Scenario: reconnexion paresseuse à l'appel suivant
    #[tokio::test(flavor = "multi_thread")]
    async fn next_call_reconnects_first() {
        let connector = Arc::new(two_tools().fail_unreachable(4));
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let _first = error_text(engine.eval::<Dynamic>("grafana::get_pods(#{})"));
        assert_eq!(connector.reconnects(), 3);
        succeeds(&engine, "grafana::get_pods(#{})", "the server answers again");
        assert_eq!(connector.reconnects(), 4, "reconnected before the call");
        assert_eq!(
            reconnects_of(&sink).len(),
            3,
            "no retry event for a lazy reconnection"
        );
        succeeds(&engine, "grafana::get_pods(#{})", "a healthy connection");
        assert_eq!(connector.reconnects(), 4, "no reconnection once healthy");
    }

    /// A connector that records the order of its operations and plays a script of outcomes (then answers `{}`).
    struct OrderedConnector {
        order: Mutex<Vec<&'static str>>,
        script: Mutex<VecDeque<Result<Value, McpError>>>,
        tools: Vec<McpTool>,
    }

    impl OrderedConnector {
        fn new(script: Vec<Result<Value, McpError>>) -> Self {
            Self {
                order: Mutex::new(Vec::new()),
                script: Mutex::new(script.into()),
                tools: vec![pods_tool()],
            }
        }

        fn order(&self) -> Vec<&'static str> {
            self.order.lock().expect("the order is readable").clone()
        }
    }

    impl McpLister for OrderedConnector {
        fn list_tools(&self) -> impl Future<Output = Result<Vec<McpTool>, McpError>> + Send {
            std::future::ready(Ok(self.tools.clone()))
        }
    }

    impl McpConnector for OrderedConnector {
        fn call_tool(
            &self,
            _name: &str,
            _arguments: &Value,
        ) -> impl Future<Output = Result<Value, McpError>> + Send {
            self.order
                .lock()
                .expect("the order is writable")
                .push("call_tool");
            let next = self
                .script
                .lock()
                .expect("the script is readable")
                .pop_front()
                .unwrap_or_else(|| Ok(json!({})));
            std::future::ready(next)
        }

        fn reconnect(&self) -> impl Future<Output = ()> + Send {
            self.order
                .lock()
                .expect("the order is writable")
                .push("reconnect");
            std::future::ready(())
        }
    }

    fn lost(text: &str) -> Result<Value, McpError> {
        Err(McpError::Unreachable(text.to_owned()))
    }

    // Scenario: reconnexion paresseuse à l'appel suivant (ordre des opérations)
    #[tokio::test(flavor = "multi_thread")]
    async fn lazy_reconnection_comes_before_the_next_first_call() {
        let connector = Arc::new(OrderedConnector::new(vec![
            lost("a"),
            lost("b"),
            lost("c"),
            lost("d"),
        ]));
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let _first = error_text(engine.eval::<Dynamic>("grafana::get_pods(#{})"));
        let before = connector.order().len();
        succeeds(&engine, "grafana::get_pods(#{})", "the server answers again");
        let order = connector.order();
        assert_eq!(
            order.get(..before),
            Some(
                &[
                    "call_tool",
                    "reconnect",
                    "call_tool",
                    "reconnect",
                    "call_tool",
                    "reconnect",
                    "call_tool"
                ][..]
            ),
            "the exhausted call: first call, then one reconnection and one call per attempt"
        );
        assert_eq!(
            order.get(before..),
            Some(&["reconnect", "call_tool"][..]),
            "the next call reconnects before its first call"
        );
    }

    // Scenario: Unreachable puis Failed pendant les tentatives
    #[tokio::test(flavor = "multi_thread")]
    async fn failure_during_the_attempts_is_masked_and_ends_them() {
        let connector = Arc::new(OrderedConnector::new(vec![
            lost("gone"),
            Err(McpError::Failed("bad token tok-123".to_owned())),
        ]));
        let sink = Arc::new(RecordingSink::new());
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123").expect("a secret");
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(0)));
        let engine = engine_with(&connector, env_with(&sink, redactor, clock, quick())).await;
        let text = error_text(engine.eval::<Dynamic>("grafana::get_pods(#{})"));
        assert!(
            text.contains("the tool failed"),
            "the error is the tool's: {text}"
        );
        assert!(text.contains("bad token") && !text.contains("tok-123"), "{text}");
        assert!(!text.contains("unreachable"), "{text}");
        assert_eq!(reconnects_of(&sink).len(), 1, "one attempt only");
        assert_eq!(connector.order(), vec!["call_tool", "reconnect", "call_tool"]);
        assert_eq!(calls_of(&sink)[0].status, "error");
    }

    // Scenario: un échec transitoire est rattrapé
    #[tokio::test(flavor = "multi_thread")]
    async fn transient_failure_is_absorbed() {
        let connector = Arc::new(two_tools().fail_unreachable(1));
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        succeeds(&engine, "grafana::get_pods(#{})", "the second attempt succeeds");
        assert_eq!(reconnects_of(&sink).len(), 1);
        assert_eq!(calls_of(&sink)[0].status, "ok");
    }

    /// A connector whose tool always fails with a text that holds a secret.
    struct LeakyConnector {
        calls: AtomicUsize,
        tools: Vec<McpTool>,
    }

    impl McpLister for LeakyConnector {
        fn list_tools(&self) -> impl Future<Output = Result<Vec<McpTool>, McpError>> + Send {
            std::future::ready(Ok(self.tools.clone()))
        }
    }

    impl McpConnector for LeakyConnector {
        fn call_tool(
            &self,
            _name: &str,
            _arguments: &Value,
        ) -> impl Future<Output = Result<Value, McpError>> + Send {
            let _previous = self.calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Err(McpError::Failed("bad token tok-123".to_owned())))
        }

        fn reconnect(&self) -> impl Future<Output = ()> + Send {
            std::future::ready(())
        }
    }

    // Scenario: erreur de tool non retentée
    #[tokio::test(flavor = "multi_thread")]
    async fn tool_error_is_masked_and_not_retried() {
        let connector = Arc::new(LeakyConnector {
            calls: AtomicUsize::new(0),
            tools: vec![pods_tool()],
        });
        let sink = Arc::new(RecordingSink::new());
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123").expect("a secret");
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(0)));
        let engine = engine_with(&connector, env_with(&sink, redactor, clock, quick())).await;
        let text = error_text(engine.eval::<Dynamic>("grafana::get_pods(#{})"));
        assert!(!text.contains("tok-123"), "the secret is masked: {text}");
        assert!(text.contains("bad token"), "the rest of the text is kept: {text}");
        assert_eq!(reconnects_of(&sink), no_reconnects());
        assert_eq!(connector.calls.load(Ordering::Relaxed), 1);
        assert_eq!(calls_of(&sink)[0].status, "error");
    }

    // Scenario: arguments masqués dans l'enregistrement
    #[tokio::test(flavor = "multi_thread")]
    async fn arguments_are_masked_in_the_record() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123").expect("a secret");
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(0)));
        let engine = engine_with(&connector, env_with(&sink, redactor, clock, quick())).await;
        succeeds(
            &engine,
            r#"grafana::get_pods(#{auth: "tok-123"})"#,
            "the call succeeds",
        );
        let text = format!("{:?}", sink.events());
        assert!(!text.contains("tok-123"), "{text}");
        assert_eq!(connector.received(), vec![json!({"auth": "tok-123"})]);
    }

    // Scenario: coquille sans appel possible
    #[tokio::test(flavor = "multi_thread")]
    async fn shell_documents_like_the_real_module_and_refuses_calls() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let real = mcp_module("grafana", Arc::clone(&connector), env(&sink, quick()))
            .await
            .expect("the module builds");
        let descriptor = mcp_shell_descriptor("grafana", &*connector)
            .await
            .expect("the descriptor builds");
        let shell = shell_module(&descriptor);
        assert_eq!(docs(&shell), docs(&real), "same functions, same documentation");
        assert_eq!(docs(&shell).len(), 2);
        let mut engine = RhaiEngine::new();
        let _registered = engine.register_static_module("grafana", Arc::new(shell));
        let error = engine
            .eval::<Dynamic>(r#"grafana::loki_query(#{query: "up"})"#)
            .expect_err("a shell never calls");
        assert!(
            matches!(
                ScratchpadError::from_rhai(&error),
                ScratchpadError::InspectionOnly { .. }
            ),
            "{error}"
        );
        assert_eq!(connector.calls(), 0);
    }

    // Scenario: serveur injoignable à l'ouverture d'une cible
    #[tokio::test(flavor = "multi_thread")]
    async fn unreachable_listing_fails_the_descriptor() {
        let lister = two_tools().listing_unreachable();
        let error = mcp_shell_descriptor("grafana", &lister)
            .await
            .expect_err("the listing fails");
        assert!(
            matches!(error, ModulesError::InvalidMcp { .. }) && error.to_string().contains("grafana"),
            "{error}"
        );
        let colliding = ScriptedConnector::new(vec![
            McpTool::new("a.b", "x", json!({})),
            McpTool::new("a_b", "y", json!({})),
        ]);
        let _collision = mcp_shell_descriptor("grafana", &colliding)
            .await
            .expect_err("a collision is refused");
        let _role = mcp_shell_descriptor("bad-role", &two_tools())
            .await
            .expect_err("a bad role is refused");
    }

    // Scenario: runtime current_thread
    #[tokio::test]
    async fn current_thread_runtime_renders_the_explicit_error() {
        let connector = Arc::new(two_tools());
        let sink = Arc::new(RecordingSink::new());
        let engine = engine_with(&connector, env(&sink, quick())).await;
        let text = error_text(engine.eval::<Dynamic>("grafana::get_pods(#{})"));
        assert!(text.contains("multi-thread"), "{text}");
        assert_eq!(connector.calls(), 0);
    }

    // Must: the wait of the backoff is abandoned when the run is cancelled
    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_abandons_the_backoff_wait() {
        let connector = Arc::new(two_tools().fail_unreachable(100));
        let sink = Arc::new(RecordingSink::new());
        let slow = McpRetryPolicy::new(3, Duration::from_secs(60));
        let engine = Arc::new(engine_with(&connector, env(&sink, slow)).await);
        let token = CancelToken::new();
        let worker_token = token.clone();
        let worker = tokio::task::spawn_blocking(move || {
            scope(&worker_token, || engine.eval::<Array>("[grafana::get_pods(#{})]"))
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        token.cancel();
        let joined = tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("the cancelled wait ends well before the 60 s delay")
            .expect("the worker does not panic");
        assert!(joined.is_err(), "the call is abandoned");
        assert_eq!(reconnects_of(&sink).len(), 1, "only the first wait began");
        assert_eq!(connector.reconnects(), 0, "the abandoned attempt did not go on");
    }
}
