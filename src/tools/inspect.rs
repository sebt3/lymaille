//! The `rhai_prod_inspect` tool of the configuration agent: it resolves the current inspection target into an inspection
//! pad and runs one script in it, read-only (`agent-sessions/scratchpad#FR-13`, `AC-13`). It lives in `tools` because it
//! reads the stores of `toolsets`, the listers of `modules` and builds a pad of `scratchpad`. See `src/tools/inspect.sdd`.

use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::agents::AgentStore;
use crate::clock::Clock;
use crate::scratchpad::{
    Grants, InspectionPad, InspectionTarget, ModuleDescriptor, RunEnv, RunStatus, Scratchpad,
    ScratchpadEngine, descriptors_from_catalog, rhai_prod_inspect_definition,
};
use crate::session::{CancelHandle, InspectionTargetRef, NativeTool, Redactor, SessionId, SessionStore};
use crate::toolsets::{ConfigStores, ContextStore, InspectionSlot, Toolset, ToolsetStore, assemble};

/// The host side of the inspection: what only the host can tell about the target.
///
/// The host only describes, it never calls: the descriptors it gives come from a listing of the tools of a server, and no
/// tool of any server is called.
pub trait InspectionHost: Send + Sync {
    /// The descriptors of the shell modules of the MCP roles `roles`, the roles bound by the context of the target. A role
    /// the host does not know as an MCP role gives no descriptor.
    ///
    /// # Errors
    ///
    /// Returns the text of the failure when a role cannot be listed. The text carries no secret.
    fn role_descriptors(
        &self,
        roles: &[String],
    ) -> impl Future<Output = Result<Vec<ModuleDescriptor>, String>> + Send;
}

/// What the inspection tool shares with the configuration session: the base engine that gives the limits, the redactor
/// that masks the output and the clock that dates the runs.
#[non_exhaustive]
pub struct InspectEnv {
    base: ScratchpadEngine,
    redactor: Redactor,
    clock: Arc<dyn Clock>,
}

impl InspectEnv {
    /// Groups the base engine (limits), the redactor and the clock of the session.
    #[must_use]
    pub fn new(base: ScratchpadEngine, redactor: Redactor, clock: Arc<dyn Clock>) -> Self {
        Self {
            base,
            redactor,
            clock,
        }
    }
}

impl fmt::Debug for InspectEnv {
    /// Opaque: the redactor holds the secrets to mask.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InspectEnv").finish_non_exhaustive()
    }
}

/// The native tool `rhai_prod_inspect`.
#[non_exhaustive]
pub struct InspectTool<A, T, Y, C, X, H> {
    stores: ConfigStores<A, T, Y, C>,
    sessions: X,
    slot: Arc<InspectionSlot>,
    host: H,
    env: InspectEnv,
}

impl<A, T, Y, C, X, H> fmt::Debug for InspectTool<A, T, Y, C, X, H> {
    /// Opaque: the stores and the environment may hold secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InspectTool").finish_non_exhaustive()
    }
}

impl<A, T, Y, C, X, H> InspectTool<A, T, Y, C, X, H> {
    /// Builds the tool over the stores, the session store, the slot shared with the `inspect` module, the host and the
    /// environment of the session.
    #[must_use]
    pub const fn new(
        stores: ConfigStores<A, T, Y, C>,
        sessions: X,
        slot: Arc<InspectionSlot>,
        host: H,
        env: InspectEnv,
    ) -> Self {
        Self {
            stores,
            sessions,
            slot,
            host,
            env,
        }
    }
}

impl<A, T, Y, C, X, H> InspectTool<A, T, Y, C, X, H>
where
    A: AgentStore,
    Y: Send + Sync,
    T: ToolsetStore,
    C: ContextStore,
    X: SessionStore,
    H: InspectionHost,
{
    /// Resolves the current target into an inspection pad. A target that does not resolve is the text of the reason.
    async fn pad_of(&self, target: &InspectionTargetRef) -> Result<InspectionPad, String> {
        match target {
            InspectionTargetRef::Definition { agent, context } => self.definition_pad(agent, context).await,
            InspectionTargetRef::Session(id) => self.session_pad(id).await,
        }
    }

    /// The pad of a past session: its variables as constants, its function sources listed.
    async fn session_pad(&self, id: &SessionId) -> Result<InspectionPad, String> {
        let state = self
            .sessions
            .load(id)
            .await
            .map_err(|_error| unresolved("the session store failed"))?
            .ok_or_else(|| unresolved(&format!("unknown session `{id}`")))?;
        let engine = ScratchpadEngine::builder(*self.env.base.limits(), Grants::new())
            .build()
            .map_err(|_error| unresolved("the engine of the session does not build"))?;
        let restored = Scratchpad::restore(Arc::new(engine), state.variables(), state.functions());
        let snapshot = restored.into_pad().snapshot();
        InspectionPad::new(
            &self.env.base,
            &Grants::new(),
            &[],
            InspectionTarget::Session(snapshot),
        )
        .map_err(|_error| unresolved("the inspection pad does not build"))
    }

    /// The pad of an agent definition read with a context: the grants of its active toolsets, the shells of their modules
    /// (read in the catalog of a real engine) and the shells of the MCP roles of the context (described by the host).
    async fn definition_pad(&self, agent: &str, context: &str) -> Result<InspectionPad, String> {
        let definition = self
            .stores
            .agents()
            .get_agent(agent)
            .await
            .map_err(|_error| unresolved("the agent store failed"))?
            .ok_or_else(|| unresolved(&format!("unknown agent `{agent}`")))?;
        let stored = self
            .stores
            .contexts()
            .get_context(context)
            .await
            .map_err(|_error| unresolved("the context store failed"))?
            .ok_or_else(|| unresolved(&format!("unknown toolset context `{context}`")))?;
        let mut toolsets = Vec::new();
        for name in definition.toolsets() {
            let found = self
                .stores
                .toolsets()
                .get_active(name)
                .await
                .map_err(|_error| unresolved("the toolset store failed"))?;
            toolsets.extend(found);
        }
        let resolved = stored
            .resolve(defaults_of(&toolsets))
            .map_err(|error| unresolved(&error.to_string()))?;
        let limits = *self.env.base.limits();
        let grants = assemble(&toolsets, &[], &BTreeMap::new(), &resolved, &limits).into_grants();
        let real = ScratchpadEngine::builder(limits, grants.clone())
            .build()
            .map_err(|_error| unresolved("the engine of the target does not build"))?;
        let names: Vec<&str> = toolsets.iter().map(Toolset::name).collect();
        let mut modules = descriptors_from_catalog(real.catalog(), &names);
        let roles = self.host.role_descriptors(resolved.roles()).await?;
        modules.extend(roles);
        InspectionPad::new(&self.env.base, &grants, &modules, InspectionTarget::Definition)
            .map_err(|_error| unresolved("the inspection pad does not build"))
    }

    /// Runs the script in the pad of the current target.
    async fn execute(&self, arguments: &Value, cancel: &CancelHandle) -> Result<String, String> {
        let script = arguments
            .get("script")
            .and_then(Value::as_str)
            .ok_or_else(|| "the argument `script` (a string) is missing".to_owned())?;
        let target = self.slot.get().ok_or_else(|| {
            "No inspection target: open one with `inspect::open_context(agent, context)` or `inspect::open_session(id)` first."
                .to_owned()
        })?;
        let pad = self.pad_of(&target).await?;
        let env = RunEnv::new("inspection", &self.env.redactor, &*self.env.clock);
        let outcome = pad.run(script, cancel, &env).await;
        let text = outcome.text().to_owned();
        match outcome.status() {
            RunStatus::Ok => Ok(text),
            RunStatus::Failed(_) | RunStatus::Cancelled => Err(text),
        }
    }
}

/// The default values declared by the toolsets, as `(toolset, parameter, value)`.
fn defaults_of(toolsets: &[Toolset]) -> Vec<(String, String, Value)> {
    toolsets
        .iter()
        .flat_map(|toolset| {
            toolset.params().values().filter_map(move |param| {
                param
                    .default()
                    .map(|value| (toolset.name().to_owned(), param.name().to_owned(), value.clone()))
            })
        })
        .collect()
}

/// The text of a target that does not resolve. The reason holds no value of a secret: names and fixed words only.
fn unresolved(reason: &str) -> String {
    format!("The inspection target cannot be resolved: {reason}.")
}

impl<A, T, Y, C, X, H> NativeTool for InspectTool<A, T, Y, C, X, H>
where
    A: AgentStore,
    T: ToolsetStore,
    Y: Send + Sync,
    C: ContextStore,
    X: SessionStore,
    H: InspectionHost,
{
    fn name(&self) -> &'static str {
        "rhai_prod_inspect"
    }

    fn definition(&self) -> Value {
        rhai_prod_inspect_definition()
    }

    fn call<'call>(
        &'call self,
        arguments: &'call Value,
        cancel: &'call CancelHandle,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'call>> {
        Box::pin(self.execute(arguments, cancel))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use serde_json::{Value, json};
    use vynil_core::Semver;

    use super::{InspectEnv, InspectTool, InspectionHost};
    use crate::agents::{AgentDefinition, AgentKind, AgentStore as _, InMemoryCatalog};
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        FunctionDescriptor, Grants, ModuleDescriptor, ScratchpadEngine, ScratchpadLimits,
    };
    use crate::session::{
        AgentRef, CancelHandle, InMemoryStore, InspectionTargetRef, NativeTool, Redactor, SessionId,
        SessionState, SessionStore as _, TaggedValue,
    };
    use crate::toolsets::{
        ConfigStores, ContextLevel, ContextStore as _, InMemoryToolsets, InspectionSlot, Toolset,
        ToolsetContext, ToolsetStore as _,
    };

    /// A host that describes the MCP role `grafana` with one function `query`, and records every request it receives. It has
    /// no way to call a tool: it only describes.
    struct Host {
        requests: Mutex<Vec<Vec<String>>>,
    }

    impl Host {
        fn new() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl InspectionHost for Arc<Host> {
        fn role_descriptors(
            &self,
            roles: &[String],
        ) -> impl Future<Output = Result<Vec<ModuleDescriptor>, String>> + Send {
            self.requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(roles.to_vec());
            let described = roles
                .iter()
                .filter(|role| role.as_str() == "grafana")
                .filter_map(|role| ModuleDescriptor::new(role))
                .map(|module| {
                    module.with_function(FunctionDescriptor::new(
                        "query",
                        &["arguments"],
                        Some("Runs a query"),
                    ))
                })
                .collect();
            std::future::ready(Ok(described))
        }
    }

    type Tool = InspectTool<
        InMemoryCatalog,
        InMemoryToolsets,
        InMemoryToolsets,
        InMemoryToolsets,
        Arc<InMemoryStore>,
        Arc<Host>,
    >;

    /// Everything a test needs around a tool.
    struct Fixture {
        tool: Tool,
        slot: Arc<InspectionSlot>,
        host: Arc<Host>,
        sessions: Arc<InMemoryStore>,
    }

    const OPS: &str = "/// Restarts a pod\nfn restart_pod(pod) { pod }";

    fn fixture_with(redactor: Redactor) -> Fixture {
        let stores = ConfigStores::new(
            InMemoryCatalog::new(),
            InMemoryToolsets::new(10_000),
            InMemoryToolsets::new(10_000),
            InMemoryToolsets::new(10_000),
        );
        let slot = Arc::new(InspectionSlot::new());
        let host = Arc::new(Host::new());
        let sessions = Arc::new(InMemoryStore::new());
        let base = ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .build()
            .expect("the base engine builds");
        let env = InspectEnv::new(
            base,
            redactor,
            Arc::new(ManualClock::new(Timestamp::from_millis(0))),
        );
        let tool = InspectTool::new(
            stores,
            Arc::clone(&sessions),
            Arc::clone(&slot),
            Arc::clone(&host),
            env,
        );
        Fixture {
            tool,
            slot,
            host,
            sessions,
        }
    }

    fn fixture() -> Fixture {
        fixture_with(Redactor::new())
    }

    /// Stores the agent `cfg` (toolset `ops`) and the context `prod` (role `grafana`), and targets them.
    async fn target_definition(fixture: &Fixture) {
        let toolset = Toolset::new(
            "ops".to_owned(),
            "Operations".to_owned(),
            Semver::parse("1.0.0").expect("a version"),
            OPS.to_owned(),
        )
        .expect("the toolset builds");
        let stores = &fixture.tool.stores;
        stores
            .toolsets()
            .put_draft(&toolset)
            .await
            .expect("the draft is stored");
        stores
            .toolsets()
            .activate("ops")
            .expect("the toolset is activated");
        let agent = AgentDefinition::new(
            "cfg".to_owned(),
            AgentKind::Work,
            "You configure.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_toolsets(vec!["ops".to_owned()]);
        stores
            .agents()
            .put_agent(&agent)
            .await
            .expect("the agent is stored");
        let context = ToolsetContext::new("prod".to_owned(), ContextLevel::Shared)
            .expect("a context")
            .with_role("grafana".to_owned(), "ref".to_owned())
            .expect("the role is bound");
        stores
            .contexts()
            .put_context(&context)
            .await
            .expect("the context is stored");
        fixture.slot.set(Some(InspectionTargetRef::Definition {
            agent: "cfg".to_owned(),
            context: "prod".to_owned(),
        }));
    }

    /// Stores a past session `past` whose pad holds `x = 5`, and targets it.
    async fn target_session(fixture: &Fixture, value: TaggedValue) {
        let id = SessionId::new("past").expect("a session id");
        let mut state = SessionState::new(
            id.clone(),
            AgentRef::new("cfg", "fingerprint"),
            "prod",
            BTreeMap::new(),
            Timestamp::from_millis(0),
        );
        state.set_pad(BTreeMap::from([("x".to_owned(), value)]), BTreeMap::new());
        fixture.sessions.save(&state).await.expect("the state is saved");
        fixture.slot.set(Some(InspectionTargetRef::Session(id)));
    }

    /// Calls the tool with `script`: the text of the result, `Ok` or `Err`.
    async fn call(fixture: &Fixture, script: &str) -> Result<String, String> {
        fixture
            .tool
            .call(&json!({"script": script}), &CancelHandle::new())
            .await
    }

    /// The text of a result, whichever way it came.
    fn text(result: Result<String, String>) -> String {
        match result {
            Ok(text) | Err(text) => text,
        }
    }

    // Scenario: un appel MCP rend l'erreur d'inspection seule
    #[tokio::test(flavor = "multi_thread")]
    async fn an_mcp_call_returns_the_inspection_only_error() {
        let fixture = fixture();
        target_definition(&fixture).await;
        let result = call(&fixture, r#"grafana::query("x")"#).await;
        let Err(failure) = result else {
            panic!("the call must fail: {result:?}");
        };
        assert!(
            failure.contains("grafana::query") && failure.contains("not executed"),
            "the error names the function and says it is not executed: {failure}"
        );
        let requests = fixture
            .host
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(
            requests,
            vec![vec!["grafana".to_owned()]],
            "the host received one request for descriptors, of the bound roles"
        );
    }

    // Scenario: les fonctions d'un toolset projeté se listent sans s'exécuter
    #[tokio::test(flavor = "multi_thread")]
    async fn toolset_functions_are_listed_without_running() {
        let fixture = fixture();
        target_definition(&fixture).await;
        let listed = call(
            &fixture,
            r#"let f = functions("ops")[0]; f.path + " | " + f.signature + " | " + f.doc"#,
        )
        .await;
        let listed = listed.expect("the listing runs");
        assert!(
            listed.contains("ops::restart_pod")
                && listed.contains("restart_pod(")
                && listed.contains("Restarts a pod"),
            "the real name, signature and documentation are listed: {listed}"
        );
        let refused = text(call(&fixture, r#"ops::restart_pod("x")"#).await);
        assert!(
            refused.contains("ops::restart_pod") && refused.contains("not executed"),
            "the call is the inspection-only error: {refused}"
        );
    }

    // Scenario: sans cible, un message dit comment en ouvrir une
    #[tokio::test]
    async fn without_a_target_a_message_says_how_to_open_one() {
        let fixture = fixture();
        let shown = text(call(&fixture, "1 + 1").await);
        assert!(
            shown.contains("inspect::open_context") && shown.contains("inspect::open_session"),
            "the message names both ways: {shown}"
        );
    }

    // Scenario: une session passée se lit sans s'écrire
    #[tokio::test(flavor = "multi_thread")]
    async fn a_past_session_is_read_and_not_written() {
        let fixture = fixture();
        target_session(&fixture, TaggedValue::Int(5)).await;
        assert_eq!(
            call(&fixture, "x").await,
            Ok("5".to_owned()),
            "the variable reads its value"
        );
        let write = call(&fixture, "x = 2").await;
        assert!(write.is_err(), "the write fails: {write:?}");
        assert_eq!(
            call(&fixture, "x").await,
            Ok("5".to_owned()),
            "the variable is unchanged"
        );
    }

    // Scenario: une cible inconnue est une erreur de texte
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_target_is_an_error_text() {
        let fixture = fixture();
        fixture.slot.set(Some(InspectionTargetRef::Definition {
            agent: "ghost".to_owned(),
            context: "prod".to_owned(),
        }));
        let shown = call(&fixture, "1").await;
        let Err(shown) = shown else {
            panic!("an unknown agent is an error: {shown:?}");
        };
        assert!(
            shown.contains("cannot be resolved") && shown.contains("ghost"),
            "the text says the target does not resolve: {shown}"
        );
        fixture.slot.set(Some(InspectionTargetRef::Session(
            SessionId::new("none").expect("an id"),
        )));
        let missing = text(call(&fixture, "1").await);
        assert!(
            missing.contains("cannot be resolved"),
            "an unknown session too: {missing}"
        );
    }

    // Scenario: chaque affirmation de la description est vraie
    #[tokio::test(flavor = "multi_thread")]
    async fn introspection_runs_for_real() {
        let fixture = fixture();
        target_definition(&fixture).await;
        for script in [
            "vars()",
            "modules()",
            "functions()",
            "toolsets()",
            "skills()",
            r#"find("restart")"#,
            "rules()",
        ] {
            let result = call(&fixture, script).await;
            assert!(result.is_ok(), "`{script}` runs: {result:?}");
        }
        let modules = call(&fixture, "modules()").await.expect("modules runs");
        assert!(
            modules.contains("ops") && modules.contains("grafana"),
            "the toolset and MCP modules are listed: {modules}"
        );
        let toolsets = call(&fixture, "toolsets()").await.expect("toolsets runs");
        assert!(toolsets.contains("ops"), "the toolset is listed: {toolsets}");
        let found = call(&fixture, r#"find("restart")"#).await.expect("find runs");
        assert!(
            found.contains("restart_pod"),
            "the search finds the function: {found}"
        );
        let mcp = call(
            &fixture,
            r#"let f = functions("grafana")[0]; f.path + " | " + f.signature + " | " + f.doc"#,
        )
        .await;
        let mcp = mcp.expect("the listing runs");
        assert!(
            mcp.contains("grafana::query") && mcp.contains("Runs a query"),
            "the MCP function has its name and documentation: {mcp}"
        );
    }

    // Scenario: chaque affirmation de la description est vraie (rien n'est sauvé, les erreurs sont du texte, rien n'est contacté)
    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_is_saved_and_errors_come_back_as_text() {
        let fixture = fixture();
        target_session(&fixture, TaggedValue::Int(5)).await;
        let before = fixture
            .sessions
            .load(&SessionId::new("past").expect("an id"))
            .await;
        assert!(
            call(&fixture, "let z = 3; z").await.is_ok(),
            "a variable is defined in the run"
        );
        let gone = call(&fixture, "z").await;
        assert!(gone.is_err(), "nothing was kept for the next call: {gone:?}");
        let after = fixture
            .sessions
            .load(&SessionId::new("past").expect("an id"))
            .await;
        assert_eq!(
            before.expect("loads"),
            after.expect("loads"),
            "the stored session did not change"
        );
        let broken = call(&fixture, "1 +").await;
        assert!(
            broken.is_err_and(|shown| !shown.is_empty()),
            "a syntax error is a text, not a panic"
        );
        let missing = fixture.tool.call(&json!({}), &CancelHandle::new()).await;
        assert!(
            missing.is_err_and(|shown| shown.contains("script")),
            "a missing script is named"
        );
    }

    // Must (inspect.sdd) : la sortie est masquée
    #[tokio::test(flavor = "multi_thread")]
    async fn the_output_never_holds_a_declared_secret() {
        let mut redactor = Redactor::new();
        redactor.add_secret("tok-123456").expect("a secret");
        let fixture = fixture_with(redactor);
        target_session(&fixture, TaggedValue::String("Bearer tok-123456".to_owned())).await;
        let shown = text(call(&fixture, "x").await);
        assert!(!shown.contains("tok-123456"), "the secret is masked: {shown}");
    }

    // Must (inspect.sdd) : l'annulation du tour arrête le script
    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_stops_the_script() {
        let fixture = fixture();
        target_session(&fixture, TaggedValue::Int(5)).await;
        let cancel = CancelHandle::new();
        let trigger = cancel.clone();
        let arguments = json!({"script": "loop { }"});
        let (result, ()) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(5), fixture.tool.call(&arguments, &cancel)),
            async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                trigger.cancel();
            }
        );
        let result = result.expect("the cancellation must stop the script within 5 seconds");
        assert!(result.is_err(), "a cancelled script is an error text: {result:?}");
    }

    // Must (inspect.sdd) : le nom et la définition sont ceux du tool
    #[test]
    fn the_tool_answers_its_name_and_its_definition() {
        let fixture = fixture();
        let tool: &dyn NativeTool = &fixture.tool;
        assert_eq!(tool.name(), "rhai_prod_inspect", "the name is the canonical one");
        assert_eq!(
            tool.definition(),
            crate::scratchpad::rhai_prod_inspect_definition()
        );
        assert_eq!(
            tool.definition().get("name"),
            Some(&Value::from("rhai_prod_inspect"))
        );
    }
}
