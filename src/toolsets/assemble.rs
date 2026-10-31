//! Assembly of what the scratchpad receives from the toolsets of an agent: a granted module, a granted toolset and the
//! granted skills for each toolset that builds, and the reason for each one that does not (`agent-sessions/toolsets#FR-2`,
//! `AC-1`, `agent-sessions/scratchpad#FR-3`).
//!
//! The module of a toolset is evaluated on a construction engine that knows the dependencies of that toolset, and only
//! those. No dependency is granted by that: the import sub-modules are emptied before the module is granted. Nothing here
//! performs I/O.

use std::collections::BTreeMap;

use rhai::module_resolvers::StaticModuleResolver;
use rhai::{Module, Scope, Shared};

use super::{ResolvedContext, Toolset, ToolsetsError};
use crate::scratchpad::{GrantedModule, GrantedSkill, GrantedToolset, Grants, ScratchpadLimits, base_engine};

/// What the scratchpad receives from the toolsets of an agent: the grants, and the toolsets that could not be built.
#[derive(Debug)]
#[non_exhaustive]
pub struct Assembled {
    grants: Grants,
    failures: Vec<ToolsetsError>,
}

impl Assembled {
    /// The granted modules, toolsets and skills.
    #[must_use]
    pub const fn grants(&self) -> &Grants {
        &self.grants
    }

    /// Consumes the result and gives the grants, to which the definition adds its groups.
    #[must_use]
    pub fn into_grants(self) -> Grants {
        self.grants
    }

    /// The toolsets that were left out, each as an [`ToolsetsError::InvalidToolset`] that names it and gives the reason.
    #[must_use]
    pub fn failures(&self) -> &[ToolsetsError] {
        &self.failures
    }
}

/// Assembles the toolsets of an agent.
///
/// `granted_modules` are the modules the definition grants; they are kept as they are. `dependencies` are the modules that
/// toolsets may depend on, by name: a toolset reaches only the ones it declares, while its module is evaluated, and the
/// LLM reaches them only when the definition grants them too. Each toolset is evaluated on an engine built from
/// `base_engine` with `limits`; one that cannot be built is left out and reported, the others are granted.
///
/// The resolved `context` is not read here: `session_context()` is registered on the engine of the session and read when
/// a function of a module is called, not when the module is evaluated.
#[must_use]
pub fn assemble(
    toolsets: &[Toolset],
    granted_modules: &[GrantedModule],
    dependencies: &BTreeMap<String, Shared<Module>>,
    _context: &ResolvedContext,
    limits: &ScratchpadLimits,
) -> Assembled {
    let mut grants = Grants::new();
    for module in granted_modules {
        if let Some(extended) = grants.clone().with_module(module.clone()) {
            grants = extended;
        }
    }
    let mut failures = Vec::new();
    for toolset in toolsets {
        let outcome = build(toolset, dependencies, limits).and_then(|module| grant(&grants, toolset, module));
        match outcome {
            Ok(extended) => grants = extended,
            Err(failure) => failures.push(failure),
        }
    }
    Assembled { grants, failures }
}

/// The refusal of a toolset, with a fixed reason and a detail.
fn failure(toolset: &Toolset, reason: &'static str, detail: String) -> ToolsetsError {
    ToolsetsError::InvalidToolset {
        name: toolset.name().to_owned(),
        reason,
        detail,
    }
}

/// Evaluates the module of a toolset and empties its import sub-modules.
fn build(
    toolset: &Toolset,
    dependencies: &BTreeMap<String, Shared<Module>>,
    limits: &ScratchpadLimits,
) -> Result<Module, ToolsetsError> {
    let mut resolver = StaticModuleResolver::new();
    for name in toolset.dependencies() {
        let Some(dependency) = dependencies.get(name) else {
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

/// The grants extended with a toolset: its module, its description and its skills. All or nothing.
fn grant(grants: &Grants, toolset: &Toolset, module: Module) -> Result<Grants, ToolsetsError> {
    let taken = || failure(toolset, "a name is already granted", String::new());
    let granted_module = GrantedModule::new(toolset.name(), Shared::new(module))
        .ok_or_else(|| failure(toolset, "the name is not a module name", String::new()))?;
    let granted_toolset = GrantedToolset::new(toolset.name(), toolset.description())
        .ok_or_else(|| failure(toolset, "the description must not be blank", String::new()))?;
    let mut extended = grants
        .clone()
        .with_module(granted_module)
        .and_then(|next| next.with_toolset(granted_toolset))
        .ok_or_else(taken)?;
    for skill in toolset.skills().values() {
        let granted_skill = GrantedSkill::new(skill.name(), skill.description(), Some(toolset.name()))
            .ok_or_else(|| failure(toolset, "a skill is not valid", String::new()))?;
        extended = extended.with_skill(granted_skill).ok_or_else(taken)?;
    }
    Ok(extended)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use rhai::{FuncRegistration, Module, Shared};
    use vynil_core::Semver;

    use super::{Assembled, assemble};
    use crate::clock::{Clock, ManualClock, Timestamp};
    use crate::modules::{InMemoryMemoryStore, MemoryLevel, MemoryScopes, memory_module, memory_module_name};
    use crate::scratchpad::{
        GrantedModule, GrantedSkill, GrantedToolset, RunEnv, RunOutcome, RunStatus, Scratchpad,
        ScratchpadEngine, ScratchpadLimits, ScriptErrorClass,
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

    /// The dependency table with `helpers` in it.
    fn with_helpers() -> BTreeMap<String, Shared<Module>> {
        BTreeMap::from([("helpers".to_owned(), helpers())])
    }

    fn context() -> ResolvedContext {
        ToolsetContext::new("ctx".to_owned(), crate::toolsets::ContextLevel::Session)
            .expect("a valid context")
            .resolve(Vec::<(String, String, serde_json::Value)>::new())
            .expect("an empty context resolves")
    }

    fn run_assemble(
        toolsets: &[Toolset],
        granted: &[GrantedModule],
        dependencies: &BTreeMap<String, Shared<Module>>,
    ) -> Assembled {
        assemble(
            toolsets,
            granted,
            dependencies,
            &context(),
            &ScratchpadLimits::new(),
        )
    }

    /// A pad over the grants of an assembly.
    fn pad(assembled: Assembled) -> Scratchpad {
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), assembled.into_grants())
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
            .grants()
            .modules()
            .iter()
            .map(GrantedModule::name)
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

    // Scenario: la dépendance fonctionne mais reste cachée
    #[tokio::test(flavor = "multi_thread")]
    async fn dependency_works_but_stays_hidden() {
        let toolsets = [depending_on(toolset("ops", OPS), "helpers")];
        let assembled = run_assemble(&toolsets, &[], &with_helpers());
        assert!(assembled.failures().is_empty(), "got {:?}", assembled.failures());
        assert_eq!(module_names(&assembled), ["ops"], "helpers is not granted");
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

    #[tokio::test(flavor = "multi_thread")]
    async fn dependency_is_absent_from_introspection() {
        let toolsets = [depending_on(toolset("ops", OPS), "helpers")];
        let pad = pad(run_assemble(&toolsets, &[], &with_helpers()));
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
        let assembled = run_assemble(&toolsets, &[], &with_helpers());
        let granted = assembled
            .grants()
            .modules()
            .first()
            .expect("the module is granted");
        assert!(
            granted
                .module()
                .iter_sub_modules()
                .all(|(_, module)| module.is_empty()),
            "no content of a dependency remains"
        );
        assert!(!granted.module().is_empty(), "the public functions remain");
    }

    #[test]
    fn dependency_granted_by_the_definition_is_granted() {
        let toolsets = [depending_on(toolset("ops", OPS), "helpers")];
        let granted = [GrantedModule::new("helpers", helpers()).expect("a valid name")];
        let assembled = run_assemble(&toolsets, &granted, &with_helpers());
        assert!(assembled.failures().is_empty());
        assert_eq!(module_names(&assembled), ["helpers", "ops"]);
    }

    #[test]
    fn undeclared_dependency_is_unreachable() {
        let toolsets = [toolset("ops", OPS)];
        let assembled = run_assemble(&toolsets, &[], &with_helpers());
        assert_eq!(failed_names(&assembled), ["ops"], "an import not declared fails");
        assert_eq!(module_names(&assembled).len(), 0);
    }

    // Scenario: dépendance indisponible
    #[test]
    fn missing_dependency_fails_the_toolset_only() {
        let toolsets = [
            depending_on(toolset("ops", OPS), "helpers"),
            toolset("plain", "fn one() { 1 }"),
        ];
        let assembled = run_assemble(&toolsets, &[], &BTreeMap::new());
        assert_eq!(failed_names(&assembled), ["ops"]);
        assert_eq!(module_names(&assembled), ["plain"]);
        let granted: Vec<&str> = assembled
            .grants()
            .toolsets()
            .iter()
            .map(GrantedToolset::name)
            .collect();
        assert_eq!(granted, ["plain"], "the failed toolset is not granted at all");
        let reason = assembled.failures().first().expect("a failure").to_string();
        assert!(
            reason.contains("helpers"),
            "the reason names the dependency: {reason}"
        );
    }

    #[test]
    fn module_that_does_not_evaluate_is_reported() {
        let toolsets = [
            toolset("broken", "fn sk_leak_token( {"),
            toolset("thrower", "throw \"boom-token\";"),
            toolset("fine", "fn f() { 1 }"),
        ];
        let assembled = run_assemble(&toolsets, &[], &BTreeMap::new());
        assert_eq!(failed_names(&assembled), ["broken", "thrower"]);
        assert_eq!(module_names(&assembled), ["fine"]);
        for failure in assembled.failures() {
            let text = failure.to_string();
            assert!(!text.contains("boom-token"), "no source fragment in {text}");
            assert!(!text.contains("sk_leak_token"), "no source fragment in {text}");
        }
    }

    #[test]
    fn toolset_name_taken_by_a_granted_module_fails() {
        let granted = [GrantedModule::new("ops", Shared::new(Module::new())).expect("a valid name")];
        let toolsets = [toolset("ops", "fn f() { 1 }"), toolset("other", "fn g() { 1 }")];
        let assembled = run_assemble(&toolsets, &granted, &BTreeMap::new());
        assert_eq!(failed_names(&assembled), ["ops"]);
        assert_eq!(
            module_names(&assembled),
            ["ops", "other"],
            "the definition's module is kept"
        );
    }

    // Scenario: les skills suivent leur toolset
    #[test]
    fn skills_follow_their_toolset() {
        let granted_toolset = with_skill(toolset("ops", "fn f() { 1 }"), "ops-skill");
        let _not_granted = with_skill(toolset("other", "fn g() { 1 }"), "other-skill");
        let assembled = run_assemble(&[granted_toolset], &[], &BTreeMap::new());
        let skills = assembled.grants().skills();
        assert_eq!(skills.len(), 1);
        let skill = skills.first().expect("one skill");
        assert_eq!(skill.name(), "ops-skill");
        assert_eq!(skill.description(), "the skill ops-skill");
        assert_eq!(skill.toolset(), Some("ops"));
        let toolsets = assembled.grants().toolsets();
        assert_eq!(toolsets.len(), 1);
        assert_eq!(
            toolsets.first().map(GrantedToolset::description),
            Some("the toolset ops")
        );
    }

    #[test]
    fn skills_of_a_failed_toolset_are_not_granted() {
        let toolsets = [
            with_skill(depending_on(toolset("ops", OPS), "helpers"), "ops-skill"),
            with_skill(toolset("fine", "fn f() { 1 }"), "fine-skill"),
        ];
        let assembled = run_assemble(&toolsets, &[], &BTreeMap::new());
        let names: Vec<&str> = assembled
            .grants()
            .skills()
            .iter()
            .map(GrantedSkill::name)
            .collect();
        assert_eq!(names, ["fine-skill"]);
    }

    #[test]
    fn skill_name_taken_fails_the_second_toolset_whole() {
        let toolsets = [
            with_skill(toolset("a", "fn f() { 1 }"), "shared"),
            with_skill(toolset("b", "fn g() { 1 }"), "shared"),
        ];
        let assembled = run_assemble(&toolsets, &[], &BTreeMap::new());
        assert_eq!(failed_names(&assembled), ["b"]);
        assert_eq!(module_names(&assembled), ["a"], "not granted by halves");
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
        let dependencies = BTreeMap::from([(name.to_owned(), memory)]);
        let assembled = run_assemble(&toolsets, &[], &dependencies);
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
