//! Assembly of what the scratchpad receives from the toolsets of an agent: a presented module, a presented toolset and the
//! presented skills for each toolset that builds, and the reason for each one that does not (`agent-sessions/toolsets#FR-2`,
//! `AC-1`, `agent-sessions/scratchpad#FR-3`).
//!
//! The module of a toolset is evaluated on a construction engine that knows the dependencies that toolset declares, taken
//! from the [`Capabilities`] register, and only those. No dependency is presented by that: the import sub-modules are
//! emptied before the module is presented. Nothing here performs I/O.

use rhai::module_resolvers::StaticModuleResolver;
use rhai::{Module, Scope, Shared};

use super::{ResolvedContext, Toolset, ToolsetsError};
use crate::modules::native::NATIVE_MODULE_NAMES;
use crate::scratchpad::{
    Capabilities, Presentation, PresentedModule, PresentedSkill, PresentedToolset, ScratchpadLimits,
    base_engine,
};

/// What the scratchpad receives from the toolsets of an agent: the presentation, and the toolsets that could not be built.
#[derive(Debug)]
#[non_exhaustive]
pub struct Assembled {
    presentation: Presentation,
    failures: Vec<ToolsetsError>,
}

impl Assembled {
    /// The presented modules, toolsets and skills.
    #[must_use]
    pub const fn presentation(&self) -> &Presentation {
        &self.presentation
    }

    /// Consumes the result and gives the presentation: the modules, toolsets and skills the session receives.
    #[must_use]
    pub fn into_presentation(self) -> Presentation {
        self.presentation
    }

    /// The toolsets that were left out, each as an [`ToolsetsError::InvalidToolset`] that names it and gives the reason.
    #[must_use]
    pub fn failures(&self) -> &[ToolsetsError] {
        &self.failures
    }
}

/// Assembles the toolsets of an agent.
///
/// `capabilities` are the raw capabilities of the session (the modules of the compiled features, the MCP modules and the
/// host's own modules, already gathered): a toolset reaches only the ones it declares, while its module is evaluated, and
/// no raw module is ever presented by that. Each toolset is evaluated on an engine built from `base_engine` with `limits`;
/// one that cannot be built is left out and reported, the others are presented.
///
/// The resolved `context` is not read here: `session_context()` is registered on the engine of the session and read when
/// a function of a module is called, not when the module is evaluated.
#[must_use]
pub fn assemble(
    toolsets: &[Toolset],
    capabilities: &Capabilities,
    _context: &ResolvedContext,
    limits: &ScratchpadLimits,
) -> Assembled {
    let mut presentation = Presentation::new();
    let mut failures = Vec::new();
    for toolset in toolsets {
        let outcome =
            build(toolset, capabilities, limits).and_then(|module| present(&presentation, toolset, module));
        match outcome {
            Ok(extended) => presentation = extended,
            Err(failure) => failures.push(failure),
        }
    }
    Assembled {
        presentation,
        failures,
    }
}

/// The refusal of a toolset, with a fixed reason and a detail.
fn failure(toolset: &Toolset, reason: &'static str, detail: String) -> ToolsetsError {
    ToolsetsError::InvalidToolset {
        name: toolset.name().to_owned(),
        reason,
        detail,
    }
}

/// Evaluates the module of a toolset and empties its import sub-modules. A declared dependency that the register does
/// not hold fails the whole toolset: it is never presented by halves.
fn build(
    toolset: &Toolset,
    capabilities: &Capabilities,
    limits: &ScratchpadLimits,
) -> Result<Module, ToolsetsError> {
    let mut resolver = StaticModuleResolver::new();
    for name in toolset.dependencies() {
        let Some(dependency) = capabilities.get(name) else {
            return Err(failure(toolset, "a dependency is not available", name.clone()));
        };
        resolver.insert(name.as_str(), Module::clone(dependency));
    }
    let mut engine = base_engine(limits);
    let _resolver = engine.set_module_resolver(resolver);
    let ast = engine
        .compile(toolset.source())
        .map_err(|error| failure(toolset, "the module does not compile", place(error.position())))?;
    let mut module = Module::eval_ast_as_new(Scope::new(), &ast, &engine)
        .map_err(|error| failure(toolset, "the module does not evaluate", place(error.position())))?;
    let imports: Vec<String> = module
        .iter_sub_modules()
        .map(|(name, _)| name.to_owned())
        .collect();
    for name in imports {
        let _module = module.set_sub_module(name.as_str(), Module::new());
    }
    Ok(module)
}

/// Where Rhai reports an error. The text of the error is never copied: Rhai puts tokens and values in it.
fn place(position: rhai::Position) -> String {
    match (position.line(), position.position()) {
        (Some(line), Some(column)) => format!("line {line}, column {column}"),
        (Some(line), None) => format!("line {line}"),
        _ => "unknown position".to_owned(),
    }
}

/// The presentation extended with a toolset: its module, its description and its skills. All or nothing.
fn present(
    presentation: &Presentation,
    toolset: &Toolset,
    module: Module,
) -> Result<Presentation, ToolsetsError> {
    let taken = || failure(toolset, "a name is already presented", String::new());
    if NATIVE_MODULE_NAMES.contains(&toolset.name()) {
        return Err(failure(toolset, "a name is a native module", String::new()));
    }
    let presented_module = PresentedModule::new(toolset.name(), Shared::new(module))
        .ok_or_else(|| failure(toolset, "the name is not a module name", String::new()))?;
    let presented_toolset = PresentedToolset::new(toolset.name(), toolset.description())
        .ok_or_else(|| failure(toolset, "the description must not be blank", String::new()))?;
    let mut extended = presentation
        .clone()
        .with_module(presented_module)
        .and_then(|next| next.with_toolset(presented_toolset))
        .ok_or_else(taken)?;
    for skill in toolset.skills().values() {
        let presented_skill = PresentedSkill::new(skill.name(), skill.description(), Some(toolset.name()))
            .ok_or_else(|| failure(toolset, "a skill is not valid", String::new()))?;
        extended = extended.with_skill(presented_skill).ok_or_else(taken)?;
    }
    Ok(extended)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rhai::{FuncRegistration, Module, Shared};
    use vynil_core::Semver;

    use super::{Assembled, assemble};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{InMemoryMemoryStore, MemoryLevel, MemoryScopes, memory_module, memory_module_name};
    use crate::scratchpad::{
        Capabilities, PresentedModule, PresentedSkill, PresentedToolset, RunEnv, RunOutcome, RunStatus,
        Scratchpad, ScratchpadEngine, ScratchpadLimits, ScriptErrorClass,
    };
    use crate::session::{CancelHandle, EventSink, RecordingSink, Redactor, SessionId};
    use crate::toolsets::{ResolvedContext, Skill, Toolset, ToolsetContext, ToolsetsError};

    /// A toolset named `name` over the source text `source`.
    fn toolset(name: &str, source: &str) -> Toolset {
        Toolset::new(
            name.to_owned(),
            format!("the toolset {name}"),
            Semver::parse("1.0.0").expect("a sample version must parse"),
            source.to_owned(),
        )
        .expect("a valid toolset builds")
    }

    /// The toolset with one dependency declared.
    fn depending_on(toolset: Toolset, dependency: &str) -> Toolset {
        toolset
            .with_dependency(dependency.to_owned())
            .expect("a distinct dependency is accepted")
    }

    /// The toolset with one skill.
    fn with_skill(toolset: Toolset, skill: &str) -> Toolset {
        let skill = Skill::new(
            skill.to_owned(),
            format!("the skill {skill}"),
            "a body".to_owned(),
        )
        .expect("a valid skill builds");
        toolset.with_skill(skill).expect("a distinct skill is accepted")
    }

    /// A module `helpers` with `double(x)` and `secret()`.
    fn helpers() -> Shared<Module> {
        let mut module = Module::new();
        let _double =
            FuncRegistration::new("double").set_into_module(&mut module, |x: i64| x.saturating_mul(2));
        let _secret = FuncRegistration::new("secret").set_into_module(&mut module, || "token".to_owned());
        Shared::new(module)
    }

    /// A module `grafana` with `query(k)`, as a host module added to the register.
    fn grafana() -> Shared<Module> {
        let mut module = Module::new();
        let _query =
            FuncRegistration::new("query").set_into_module(&mut module, |k: &str| format!("grafana:{k}"));
        Shared::new(module)
    }

    /// The register of a build: the compiled features, nothing added.
    fn capabilities() -> Capabilities {
        Capabilities::compiled()
    }

    /// The register with `helpers` added by the host.
    fn capabilities_with_helpers() -> Capabilities {
        capabilities()
            .with_module("helpers", helpers())
            .expect("a fresh name")
    }

    fn context() -> ResolvedContext {
        ToolsetContext::new("ctx".to_owned(), crate::toolsets::ContextLevel::Session)
            .expect("a valid context")
            .resolve(Vec::<(String, String, serde_json::Value)>::new())
            .expect("an empty context resolves")
    }

    fn run_assemble(toolsets: &[Toolset], capabilities: &Capabilities) -> Assembled {
        assemble(toolsets, capabilities, &context(), &ScratchpadLimits::new())
    }

    /// A pad over the presentation of an assembly.
    fn pad(assembled: Assembled) -> Scratchpad {
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), assembled.into_presentation())
            .build()
            .expect("the engine builds");
        Scratchpad::new(Arc::new(engine))
    }

    async fn run(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    fn module_names(assembled: &Assembled) -> Vec<&str> {
        assembled
            .presentation()
            .modules()
            .iter()
            .map(PresentedModule::name)
            .collect()
    }

    fn failed_names(assembled: &Assembled) -> Vec<&str> {
        assembled
            .failures()
            .iter()
            .map(|failure| match failure {
                ToolsetsError::InvalidToolset { name, .. } => name.as_str(),
                ToolsetsError::InvalidContextType { .. }
                | ToolsetsError::InvalidSkill { .. }
                | ToolsetsError::InvalidContext { .. }
                | ToolsetsError::SkillTooLarge { .. }
                | ToolsetsError::Template { .. }
                | ToolsetsError::Incompatible { .. }
                | ToolsetsError::Forbidden { .. }
                | ToolsetsError::Store(_)
                | ToolsetsError::Poisoned => panic!("a failure must be InvalidToolset, got {failure:?}"),
            })
            .collect()
    }

    const OPS: &str =
        "import \"helpers\" as h;\nfn twice(x) { h::double(x) }\nfn token_len() { h::secret().len() }";

    /// A toolset whose only dependency is `k8s`, absent from the register of this crate (no such feature).
    fn k8s_toolset(name: &str) -> Toolset {
        depending_on(
            toolset(name, "import \"k8s\" as k;\nfn pods() { k::list() }"),
            "k8s",
        )
    }

    // Scenario: la dépendance fonctionne mais reste cachée
    #[tokio::test(flavor = "multi_thread")]
    async fn dependency_works_but_stays_hidden() {
        let toolsets = [depending_on(toolset("ops", OPS), "helpers")];
        let assembled = run_assemble(&toolsets, &capabilities_with_helpers());
        assert!(assembled.failures().is_empty(), "got {:?}", assembled.failures());
        assert_eq!(module_names(&assembled), ["ops"], "helpers is not presented");
        let pad = pad(assembled);

        let called = run(&pad, "ops::twice(4)").await;
        assert_eq!(called.status(), RunStatus::Ok, "got {}", called.text());
        assert_eq!(called.text(), "8");
        let direct = run(&pad, "helpers::double(4)").await;
        assert_eq!(direct.status(), RunStatus::Failed(ScriptErrorClass::NotFound));
        let through = run(&pad, "ops::double(4)").await;
        assert_eq!(through.status(), RunStatus::Failed(ScriptErrorClass::NotFound));
        let alias = run(&pad, "ops::h::secret()").await;
        assert!(
            matches!(alias.status(), RunStatus::Failed(_)),
            "got {}",
            alias.text()
        );
    }

    // Scenario: une capacité de l'hôte reste cachée aussi
    #[tokio::test(flavor = "multi_thread")]
    async fn host_capability_stays_hidden_too() {
        let dash = depending_on(
            toolset("dash", "import \"grafana\" as g;\nfn ask(k) { g::query(k) }"),
            "grafana",
        );
        let assembled = run_assemble(
            &[dash],
            &capabilities()
                .with_module("grafana", grafana())
                .expect("a fresh name"),
        );
        assert!(assembled.failures().is_empty(), "got {:?}", assembled.failures());
        assert_eq!(module_names(&assembled), ["dash"], "grafana is not presented");
        let pad = pad(assembled);

        let through = run(&pad, "dash::ask(\"up\")").await;
        assert_eq!(through.status(), RunStatus::Ok, "got {}", through.text());
        assert_eq!(through.text(), "grafana:up");
        let direct = run(&pad, "grafana::query(\"up\")").await;
        assert_eq!(
            direct.status(),
            RunStatus::Failed(ScriptErrorClass::NotFound),
            "a host capability stays out of reach: {}",
            direct.text()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dependency_is_absent_from_introspection() {
        let toolsets = [depending_on(toolset("ops", OPS), "helpers")];
        let pad = pad(run_assemble(&toolsets, &capabilities_with_helpers()));
        for script in [
            "modules()",
            "functions()",
            "functions(\"ops\")",
            "find(\"helpers\")",
            "find(\"double\")",
            "find(\"secret\")",
            "toolsets()",
        ] {
            let outcome = run(&pad, script).await;
            let text = outcome.text();
            assert!(
                !text.contains("helpers"),
                "`{script}` leaks the dependency: {text}"
            );
            assert!(!text.contains("double"), "`{script}` leaks its function: {text}");
            assert!(!text.contains("secret"), "`{script}` leaks its function: {text}");
        }
        let listed = run(&pad, "functions()").await;
        assert!(
            listed.text().contains("ops::twice"),
            "the toolset itself is listed: {}",
            listed.text()
        );
    }

    #[test]
    fn import_sub_modules_are_emptied() {
        let toolsets = [depending_on(toolset("ops", OPS), "helpers")];
        let assembled = run_assemble(&toolsets, &capabilities_with_helpers());
        let presented = assembled
            .presentation()
            .modules()
            .first()
            .expect("the module is presented");
        assert!(
            presented
                .module()
                .iter_sub_modules()
                .all(|(_, module)| module.is_empty()),
            "no content of a dependency remains"
        );
        assert!(!presented.module().is_empty(), "the public functions remain");
    }

    #[test]
    fn undeclared_dependency_is_unreachable() {
        let toolsets = [toolset("ops", OPS)];
        let assembled = run_assemble(&toolsets, &capabilities_with_helpers());
        assert_eq!(failed_names(&assembled), ["ops"], "an import not declared fails");
        assert_eq!(module_names(&assembled).len(), 0);
    }

    // Scenario: dépendance indisponible
    #[tokio::test(flavor = "multi_thread")]
    async fn missing_dependency_fails_the_toolset_only() {
        let toolsets = [k8s_toolset("ops"), toolset("plain", "fn one() { 1 }")];
        let assembled = run_assemble(&toolsets, &capabilities());
        assert_eq!(failed_names(&assembled), ["ops"]);
        assert_eq!(module_names(&assembled), ["plain"]);
        let presented: Vec<&str> = assembled
            .presentation()
            .toolsets()
            .iter()
            .map(PresentedToolset::name)
            .collect();
        assert_eq!(presented, ["plain"], "the failed toolset is not presented at all");
        let reason = assembled.failures().first().expect("a failure").to_string();
        assert!(
            reason.contains("k8s"),
            "the reason names the dependency: {reason}"
        );
        // never granted by halves: no function of the failed toolset is reachable
        let pad = pad(assembled);
        let called = run(&pad, "ops::pods()").await;
        assert_eq!(
            called.status(),
            RunStatus::Failed(ScriptErrorClass::NotFound),
            "the failed toolset grants nothing: {}",
            called.text()
        );
        let fine = run(&pad, "plain::one()").await;
        assert_eq!(fine.status(), RunStatus::Ok, "got {}", fine.text());
    }

    // engine.sdd #19 (posed in assemble.rs, slice E): the construction engine resolves `host`, which
    // capabilities.sdd keeps in the register whatever the features compiled.
    #[tokio::test(flavor = "multi_thread")]
    async fn host_dependency_resolves_and_get_env_returns_the_empty_string() {
        let source = "import \"host\" as h;\nfn probe() { h::get_env(\"LYMAILLE_ABSENT\") }";
        let toolsets = [depending_on(toolset("env_probe", source), "host")];
        let assembled = run_assemble(&toolsets, &capabilities());
        assert!(assembled.failures().is_empty(), "got {:?}", assembled.failures());
        assert_eq!(module_names(&assembled), ["env_probe"], "host is not presented");
        let pad = pad(assembled);

        let called = run(&pad, "env_probe::probe()").await;
        assert_eq!(called.status(), RunStatus::Ok, "got {}", called.text());
        assert_eq!(called.text(), "", "an absent variable renders the empty string");
    }

    // Scenario: la fonction d'une capacité non compilée n'atteint pas le toolset (négatif : sans la feature `fs`).
    // Rhai resolves function calls when they run: the toolset assembles because `host` is in the register whatever
    // the features (../scratchpad/capabilities.sdd), and only the call of the function fails.
    #[cfg(not(feature = "fs"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn uncompiled_capability_function_fails_at_the_call() {
        let source = "import \"host\" as h;\nfn read_it() { h::file_read(\"absent.txt\") }";
        let toolsets = [depending_on(toolset("reader", source), "host")];
        let assembled = run_assemble(&toolsets, &capabilities());
        assert!(
            assembled.failures().is_empty(),
            "`host` is in the register whatever the features: {:?}",
            assembled.failures()
        );
        assert_eq!(module_names(&assembled), ["reader"], "host is not presented");
        let pad = pad(assembled);

        let called = run(&pad, "reader::read_it()").await;
        assert_eq!(
            called.status(),
            RunStatus::Failed(ScriptErrorClass::NotFound),
            "without `fs`, file_read is a function-not-found error: {}",
            called.text()
        );
    }

    // Scenario: la fonction d'une capacité non compilée n'atteint pas le toolset (positif : sous la feature `fs`).
    // The function is checked in the generated signatures of the `host` module, never called: no test reads a
    // real file (`agent-sessions#CON-1`).
    #[cfg(feature = "fs")]
    #[test]
    fn compiled_capability_lists_file_read_in_the_host_module() {
        let registry = capabilities();
        let host = registry.get("host").expect("host is always in the register");
        let prefix = "file_read(";
        let listed = host
            .gen_fn_signatures_with_mapper(Into::into)
            .any(|signature| signature.starts_with(prefix));
        assert!(listed, "with `fs`, the host module lists file_read");
    }

    #[test]
    fn module_that_does_not_evaluate_is_reported() {
        let toolsets = [
            toolset("broken", "fn sk_leak_token( {"),
            toolset("thrower", "throw \"boom-token\";"),
            toolset("fine", "fn f() { 1 }"),
        ];
        let assembled = run_assemble(&toolsets, &capabilities());
        assert_eq!(failed_names(&assembled), ["broken", "thrower"]);
        assert_eq!(module_names(&assembled), ["fine"]);
        for failure in assembled.failures() {
            let text = failure.to_string();
            assert!(!text.contains("boom-token"), "no source fragment in {text}");
            assert!(!text.contains("sk_leak_token"), "no source fragment in {text}");
        }
    }

    #[test]
    fn duplicate_toolset_name_fails_the_second_whole() {
        let toolsets = [toolset("ops", "fn f() { 1 }"), toolset("ops", "fn g() { 2 }")];
        let assembled = run_assemble(&toolsets, &capabilities());
        assert_eq!(failed_names(&assembled), ["ops"], "the second is refused");
        assert_eq!(
            module_names(&assembled),
            ["ops"],
            "the first module is kept, not the second"
        );
    }

    // Scenario: les skills suivent leur toolset
    #[test]
    fn skills_follow_their_toolset() {
        let failing = with_skill(k8s_toolset("ops"), "ops-skill");
        let fine = with_skill(toolset("fine", "fn f() { 1 }"), "fine-skill");
        let assembled = run_assemble(&[failing, fine], &capabilities());
        let skills = assembled.presentation().skills();
        assert_eq!(skills.len(), 1, "only the skill of the toolset that built");
        let skill = skills.first().expect("one skill");
        assert_eq!(skill.name(), "fine-skill");
        assert_eq!(skill.description(), "the skill fine-skill");
        assert_eq!(skill.toolset(), Some("fine"));
        let toolsets = assembled.presentation().toolsets();
        assert_eq!(toolsets.len(), 1);
        assert_eq!(
            toolsets.first().map(PresentedToolset::description),
            Some("the toolset fine")
        );
    }

    #[test]
    fn skills_of_a_failed_toolset_are_not_presented() {
        let toolsets = [
            with_skill(depending_on(toolset("ops", OPS), "helpers"), "ops-skill"),
            with_skill(toolset("fine", "fn f() { 1 }"), "fine-skill"),
        ];
        let assembled = run_assemble(&toolsets, &capabilities());
        let names: Vec<&str> = assembled
            .presentation()
            .skills()
            .iter()
            .map(PresentedSkill::name)
            .collect();
        assert_eq!(names, ["fine-skill"]);
    }

    #[test]
    fn skill_name_taken_fails_the_second_toolset_whole() {
        let toolsets = [
            with_skill(toolset("a", "fn f() { 1 }"), "shared"),
            with_skill(toolset("b", "fn g() { 1 }"), "shared"),
        ];
        let assembled = run_assemble(&toolsets, &capabilities());
        assert_eq!(failed_names(&assembled), ["b"]);
        assert_eq!(module_names(&assembled), ["a"], "not presented by halves");
    }

    // Scenario: un toolset ne peut pas prendre le nom d'un module natif, quelle que soit la nature de la session
    #[test]
    fn toolset_named_as_a_native_module_fails_whole() {
        let native = [
            "todo",
            "memory_session",
            "memory_shared",
            "memory_user",
            "delegate",
            "result",
            "session",
            "toolset_builder",
            "contexts",
            "sessions",
            "inspect",
        ];
        for name in native {
            let toolsets = [toolset(name, "fn f() { 1 }"), toolset("other", "fn g() { 1 }")];
            let assembled = run_assemble(&toolsets, &capabilities());
            assert_eq!(
                failed_names(&assembled),
                [name],
                "`{name}` is a native module name"
            );
            assert_eq!(
                module_names(&assembled),
                ["other"],
                "the toolset named `{name}` is not presented, and the other one is"
            );
        }
    }

    // modules#AC-2: a read-only toolset over `memory_user`.
    #[tokio::test(flavor = "multi_thread")]
    async fn read_only_toolset_over_memory_user_cannot_modify() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let session = SessionId::new("s-1").expect("a session id");
        let sink: Arc<dyn EventSink> = Arc::new(RecordingSink::new());
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(Timestamp::from_millis(1_000)));
        let memory = memory_module(
            MemoryLevel::User,
            Arc::clone(&store),
            MemoryScopes::new("alice", "alpha", &session),
            sink,
            clock,
        );
        let name = memory_module_name(MemoryLevel::User);
        let memory = Shared::new(memory);
        let mut engine = rhai::Engine::new();
        let _registered = engine.register_static_module(name, Shared::clone(&memory));
        engine
            .eval::<()>("memory_user::create(\"a\", \"A title\", \"A body\")")
            .expect("the seed is written");

        let source = "import \"memory_user\" as m;\nfn ids() { m::list() }\nfn get(id) { m::read(id) }";
        let toolsets = [depending_on(toolset("notes", source), name)];
        let capabilities = capabilities().with_module(name, memory).expect("a fresh name");
        let assembled = run_assemble(&toolsets, &capabilities);
        assert!(assembled.failures().is_empty(), "got {:?}", assembled.failures());
        assert_eq!(module_names(&assembled), ["notes"]);
        let pad = pad(assembled);

        let listed = run(&pad, "notes::ids()").await;
        assert_eq!(listed.status(), RunStatus::Ok, "got {}", listed.text());
        assert!(listed.text().contains("A title"), "got {}", listed.text());
        let read = run(&pad, "notes::get(\"a\")").await;
        assert!(read.text().contains("A body"), "got {}", read.text());
        for script in [
            "memory_user::create(\"b\", \"t\", \"b\")",
            "memory_user::list()",
            "notes::create(\"b\", \"t\", \"b\")",
            "notes::m::create(\"b\", \"t\", \"b\")",
        ] {
            let refused = run(&pad, script).await;
            assert!(
                matches!(refused.status(), RunStatus::Failed(_)),
                "`{script}` must fail: {}",
                refused.text()
            );
        }
        let modules = run(&pad, "modules()").await;
        assert!(!modules.text().contains("memory_user"), "got {}", modules.text());
        let seen = run(&pad, "notes::ids()").await;
        assert!(
            !seen.text().contains("\"b\""),
            "nothing was created: {}",
            seen.text()
        );
    }
}
