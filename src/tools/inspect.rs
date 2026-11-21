//! The `rhai_prod_inspect` tool of the configuration agent: it resolves the current inspection target into an inspection
//! pad and runs one script in it, read-only (`agent-sessions/scratchpad#FR-13`, `AC-13`). It lives in `tools` because it
//! reads the stores of `toolsets`, the listers of `modules` and builds a pad of `scratchpad`. See `src/tools/inspect.sdd`.

use std::collections::BTreeSet;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::agents::AgentStore;
use crate::clock::Clock;
use crate::scratchpad::{
    Capabilities, InspectionPad, InspectionTarget, ModuleDescriptor, Presentation, RunEnv, RunStatus,
    Scratchpad, ScratchpadEngine, descriptors_from_capabilities, descriptors_from_catalog,
    rhai_prod_inspect_definition,
};
use crate::session::{CancelHandle, InspectionTargetRef, NativeTool, Redactor, SessionId, SessionStore};
use crate::toolsets::{
    ConfigStores, ContextStore, InspectionSlot, Toolset, ToolsetStore, assemble, select_toolsets,
};

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
///
/// It also carries the raw capabilities of the host, never executed: they feed the assembly of the toolsets and are
/// projected as shells.
#[non_exhaustive]
pub struct InspectEnv {
    base: ScratchpadEngine,
    redactor: Redactor,
    clock: Arc<dyn Clock>,
    capabilities: Capabilities,
}

impl InspectEnv {
    /// Groups the base engine (limits), the redactor, the clock and the capabilities of the session.
    #[must_use]
    pub fn new(
        base: ScratchpadEngine,
        redactor: Redactor,
        clock: Arc<dyn Clock>,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            base,
            redactor,
            clock,
            capabilities,
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
        let engine = ScratchpadEngine::builder(*self.env.base.limits(), Presentation::new())
            .build()
            .map_err(|_error| unresolved("the engine of the session does not build"))?;
        let restored = Scratchpad::restore(Arc::new(engine), state.variables(), state.functions());
        let snapshot = restored.into_pad().snapshot();
        InspectionPad::new(
            &self.env.base,
            &Presentation::new(),
            &[],
            InspectionTarget::Session(snapshot),
        )
        .map_err(|_error| unresolved("the inspection pad does not build"))
    }

    /// The pad of an agent definition read with a context: the presentation of its active toolsets, the shells of their modules
    /// (read in the catalog of a real engine), the shells of the MCP roles of the context (described by the host) and the
    /// shells of the raw capabilities (read in their Rhai metadata; a capability that would shadow a projected module yields).
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
        let selection = select_toolsets(&stored, &definition);
        for name in selection.names() {
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
        let presentation =
            assemble(&toolsets, &self.env.capabilities, &resolved, &limits).into_presentation();
        let real = ScratchpadEngine::builder(limits, presentation.clone())
            .build()
            .map_err(|_error| unresolved("the engine of the target does not build"))?;
        let names: Vec<&str> = toolsets.iter().map(Toolset::name).collect();
        let mut modules = descriptors_from_catalog(real.catalog(), &names);
        let roles = self.host.role_descriptors(resolved.roles()).await?;
        modules.extend(roles);
        let projected: BTreeSet<String> = modules
            .iter()
            .map(|descriptor| descriptor.name().to_owned())
            .collect();
        modules.extend(
            descriptors_from_capabilities(&self.env.capabilities)
                .into_iter()
                .filter(|descriptor| !projected.contains(descriptor.name())),
        );
        InspectionPad::new(
            &self.env.base,
            &presentation,
            &modules,
            InspectionTarget::Definition,
        )
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
        Capabilities, FunctionDescriptor, ModuleDescriptor, Presentation, ScratchpadEngine, ScratchpadLimits,
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
        fixture_backed_by(redactor, Capabilities::compiled())
    }

    /// The same fixture over the capabilities the host injected in the registry.
    fn fixture_backed_by(redactor: Redactor, capabilities: Capabilities) -> Fixture {
        let stores = ConfigStores::new(
            InMemoryCatalog::new(),
            InMemoryToolsets::new(10_000),
            InMemoryToolsets::new(10_000),
            InMemoryToolsets::new(10_000),
        );
        let slot = Arc::new(InspectionSlot::new());
        let host = Arc::new(Host::new());
        let sessions = Arc::new(InMemoryStore::new());
        let base = ScratchpadEngine::builder(ScratchpadLimits::new(), Presentation::new())
            .build()
            .expect("the base engine builds");
        let env = InspectEnv::new(
            base,
            redactor,
            Arc::new(ManualClock::new(Timestamp::from_millis(0))),
            capabilities,
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
        let agent = AgentDefinition::new("cfg".to_owned(), AgentKind::Work, "You configure.".to_owned());
        stores
            .agents()
            .put_agent(&agent)
            .await
            .expect("the agent is stored");
        let context = ToolsetContext::new("prod".to_owned(), ContextLevel::Shared)
            .expect("a context")
            .with_role("grafana".to_owned(), "ref".to_owned())
            .expect("the role is bound")
            .with_toolset("ops".to_owned())
            .expect("the toolset is listed");
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

    // Scenario (amendement proposé à `agent-sessions#AC-6bis`, tools/inspect.sdd tâche #3) : une fonction d'une
    // capacité brute appelée dans `rhai_prod_inspect` rend l'erreur d'inspection seule, sans lire l'environnement.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_raw_capability_call_returns_the_inspection_only_error() {
        let fixture = fixture();
        target_definition(&fixture).await;
        let listed = call(&fixture, r#""host" in modules().map(|m| m.name)"#).await;
        assert_eq!(
            listed,
            Ok("true".to_owned()),
            "`host` is projected as a compiled capability"
        );
        let result = call(&fixture, r#"host::get_env("LYMAILLE_ABSENT")"#).await;
        let Err(failure) = result else {
            panic!("the call must fail: {result:?}");
        };
        assert!(
            failure.contains("host::get_env") && failure.contains("not executed"),
            "the error names the function and says it is not executed: {failure}"
        );
    }

    // tools/inspect.sdd tâche #3 : la cible `Definition` assemble par `assemble(.., &Capabilities, ..)` avec les
    // capacités brutes de l'hôte, pas avec `Capabilities::compiled()`. Un toolset dont la dépendance déclarée n'est
    // que dans le module injecté par l'hôte s'assemble et se projette ; mué en `compiled()`, la dépendance manque,
    // le toolset est omis sans signalement et le module disparaît de `modules()`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_toolset_depending_on_an_injected_capability_is_assembled_and_projected() {
        let mut module = rhai::Module::new();
        let _registered =
            rhai::FuncRegistration::new("ping").set_into_module(&mut module, || "pong".to_owned());
        let capabilities = Capabilities::compiled()
            .with_module("injected", rhai::Shared::new(module))
            .expect("a fresh name");
        let fixture = fixture_backed_by(Redactor::new(), capabilities);
        let toolset = Toolset::new(
            "solo".to_owned(),
            "Solo work".to_owned(),
            Semver::parse("1.0.0").expect("a version"),
            "import \"injected\" as i;\nfn greet() { i::ping() }".to_owned(),
        )
        .expect("the toolset builds")
        .with_dependency("injected".to_owned())
        .expect("the dependency is declared");
        let stores = &fixture.tool.stores;
        stores
            .toolsets()
            .put_draft(&toolset)
            .await
            .expect("the draft is stored");
        stores
            .toolsets()
            .activate("solo")
            .expect("the toolset is activated");
        let agent = AgentDefinition::new("cfg".to_owned(), AgentKind::Work, "You configure.".to_owned());
        stores
            .agents()
            .put_agent(&agent)
            .await
            .expect("the agent is stored");
        let context = ToolsetContext::new("prod".to_owned(), ContextLevel::Shared)
            .expect("a context")
            .with_toolset("solo".to_owned())
            .expect("the toolset is listed");
        stores
            .contexts()
            .put_context(&context)
            .await
            .expect("the context is stored");
        fixture.slot.set(Some(InspectionTargetRef::Definition {
            agent: "cfg".to_owned(),
            context: "prod".to_owned(),
        }));
        let listed = call(&fixture, r#""solo" in modules().map(|m| m.name)"#).await;
        assert_eq!(
            listed,
            Ok("true".to_owned()),
            "the toolset assembled against the injected capability is projected"
        );
        let functions = call(&fixture, r#"functions("solo")[0].path"#).await;
        assert_eq!(
            functions,
            Ok("solo::greet".to_owned()),
            "the function of the assembled toolset is listed"
        );
        let called = call(&fixture, "solo::greet()").await;
        let Err(failure) = called else {
            panic!("the call must fail: {called:?}");
        };
        assert!(
            failure.contains("solo::greet") && failure.contains("not executed"),
            "the projected function is a shell and refuses the call: {failure}"
        );
    }

    // Same amendment, the other face: an MCP role the host also registered as a raw capability keeps its
    // descriptor (the capability yields the name), and its calls stay the inspection-only error.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_role_described_by_the_host_wins_over_a_capability_of_the_same_name() {
        let mut module = rhai::Module::new();
        let _registered =
            rhai::FuncRegistration::new("direct").set_into_module(&mut module, || "ran".to_owned());
        let capabilities = Capabilities::compiled()
            .with_module("grafana", rhai::Shared::new(module))
            .expect("a fresh name");
        let fixture = fixture_backed_by(Redactor::new(), capabilities);
        target_definition(&fixture).await;
        let listed = call(&fixture, r#""host" in modules().map(|m| m.name)"#).await;
        assert_eq!(
            listed,
            Ok("true".to_owned()),
            "the other capabilities are still there"
        );
        let result = call(&fixture, r#"grafana::query("x")"#).await;
        let Err(failure) = result else {
            panic!("the call must fail: {result:?}");
        };
        assert!(
            failure.contains("grafana::query") && failure.contains("not executed"),
            "the role descriptor of the host keeps the name against the capability: {failure}"
        );
        let direct = call(&fixture, "grafana::direct()").await;
        let Err(direct) = direct else {
            panic!("a function of the module behind the role is not reachable: {direct:?}");
        };
        assert!(
            direct.contains("direct") && !direct.contains("ran"),
            "the raw capability module named `grafana` is not projected: {direct}"
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

    // Not a Scenario (`agent-sessions/toolsets#FR-25`, `AC-20`): the pad of a sub-agent's definition presents the
    // intersection of the context list and its restriction: no toolset outside the list, even active in the catalogue.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sub_agent_target_sees_only_the_intersection() {
        let fixture = fixture();
        let stores = &fixture.tool.stores;
        for name in ["ops", "ghost"] {
            let toolset = Toolset::new(
                name.to_owned(),
                "Operations".to_owned(),
                Semver::parse("1.0.0").expect("a version"),
                OPS.to_owned(),
            )
            .expect("the toolset builds");
            stores
                .toolsets()
                .put_draft(&toolset)
                .await
                .expect("the draft is stored");
            stores
                .toolsets()
                .activate(name)
                .expect("the toolset is activated");
        }
        let agent = AgentDefinition::new("sub".to_owned(), AgentKind::Sub, "You delegate.".to_owned())
            .with_toolset_restriction(vec!["ops".to_owned(), "ghost".to_owned()]);
        stores
            .agents()
            .put_agent(&agent)
            .await
            .expect("the agent is stored");
        let context = ToolsetContext::new("prod".to_owned(), ContextLevel::Shared)
            .expect("a context")
            .with_toolset("ops".to_owned())
            .expect("the toolset is listed");
        stores
            .contexts()
            .put_context(&context)
            .await
            .expect("the context is stored");
        fixture.slot.set(Some(InspectionTargetRef::Definition {
            agent: "sub".to_owned(),
            context: "prod".to_owned(),
        }));
        let listed = call(&fixture, "toolsets()").await.expect("toolsets runs");
        assert!(
            listed.contains("ops") && !listed.contains("ghost"),
            "the restriction never extends the list of the context: {listed}"
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
