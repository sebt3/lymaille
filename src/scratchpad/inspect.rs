//! The inspection pad of the `rhai_prod_inspect` tool: a scratchpad that reads a definition or a past session without
//! running anything of its target. Its modules are shells: same names, signatures and documentation as the real ones, and
//! every call of a shell is refused with `ScratchpadError::InspectionOnly`. A session target has its variables as
//! constants and its functions listed but not callable. See `src/scratchpad/inspect.sdd`.

use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::Arc;

use rhai::{
    Dynamic, Engine, EvalAltResult, FuncRegistration, ImmutableString, Module, NativeCallContext, Position,
    RhaiFunc, Shared,
};

use super::engine::{EngineBuilder, ScratchpadEngine};
use super::error::{ScratchpadError, ScratchpadResult};
use super::format::register;
use super::grant::{GrantedModule, Grants};
use super::introspect::{Catalog, FunctionEntry, Origin};
use super::run::{PadSnapshot, RunEnv, RunOutcome, Scratchpad};
use crate::session::CancelHandle;

/// A function of a shell module: its name, its parameter names and its documentation, when it has one. Its arity is the
/// number of its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionDescriptor {
    name: String,
    params: Vec<String>,
    doc: Option<String>,
}

impl FunctionDescriptor {
    /// Describes a function: its name, its parameter names and its documentation, when it has one.
    #[must_use]
    pub fn new(name: &str, params: &[&str], doc: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            params: params.iter().map(|param| (*param).to_owned()).collect(),
            doc: doc.map(str::to_owned),
        }
    }
}

/// A module projected into the inspection pad as a shell: its name and its functions. The name is a Rhai identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModuleDescriptor {
    name: String,
    functions: Vec<FunctionDescriptor>,
}

impl ModuleDescriptor {
    /// Describes a module with no function yet. Returns nothing when the name is not a Rhai identifier that a module can
    /// take: the rule of granted modules applies.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        GrantedModule::new(name, Shared::new(Module::new())).map(|granted| Self {
            name: granted.name().to_owned(),
            functions: Vec::new(),
        })
    }

    /// Adds a function to the module.
    #[must_use]
    pub fn with_function(mut self, function: FunctionDescriptor) -> Self {
        self.functions.push(function);
        self
    }
}

/// What the inspection pad reads.
///
/// A definition has nothing but its descriptions, which come with the grants, and no variable. A session has the
/// restored state of a past pad: its variables are read-only, its functions are listed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum InspectionTarget {
    /// The definition of an agent: no variable.
    Definition,
    /// A past session, from its pad snapshot.
    Session(PadSnapshot),
}

/// The scratchpad of an inspection. It never persists: its outcome carries no snapshot to keep.
#[derive(Debug)]
#[non_exhaustive]
pub struct InspectionPad {
    pad: Scratchpad,
}

/// Builds the shell module of a described module.
///
/// Each function has the arity, the parameter names and the documentation of the description, and each call is refused
/// with `InspectionOnly` naming `module::function`.
#[must_use]
pub fn shell_module(descriptor: &ModuleDescriptor) -> Module {
    let mut module = Module::new();
    for function in &descriptor.functions {
        let path = format!("{}::{}", descriptor.name, function.name);
        let _metadata = FuncRegistration::new(function.name.as_str())
            .with_params_info(function.params.iter())
            .with_comments(comments_of(function.doc.as_deref()))
            .set_into_module_raw(
                &mut module,
                vec![TypeId::of::<Dynamic>(); function.params.len()],
                refusal(path),
            );
    }
    module
}

/// Descriptors of the modules named, read from a catalog: the direct functions of each module, with their parameter names
/// and their documentation. A module the catalog does not list gives no descriptor.
#[must_use]
pub fn descriptors_from_catalog(catalog: &Catalog, modules: &[&str]) -> Vec<ModuleDescriptor> {
    modules
        .iter()
        .filter_map(|module| describe_module(catalog, module))
        .collect()
}

/// The descriptor of a granted module of the catalog, or nothing when the catalog lists no function of it.
fn describe_module(catalog: &Catalog, module: &str) -> Option<ModuleDescriptor> {
    let prefix = format!("{module}::");
    let functions: Vec<FunctionDescriptor> = catalog
        .functions()
        .iter()
        .filter(|entry| matches!(entry.origin(), Origin::Module(name) if name.as_str() == module))
        .filter_map(|entry| describe_function(entry, &prefix))
        .collect();
    if functions.is_empty() {
        return None;
    }
    let descriptor = ModuleDescriptor::new(module)?;
    Some(
        functions
            .into_iter()
            .fold(descriptor, ModuleDescriptor::with_function),
    )
}

/// The descriptor of a function of a module, its name being the path without the module prefix.
fn describe_function(entry: &FunctionEntry, prefix: &str) -> Option<FunctionDescriptor> {
    let name = entry.path().strip_prefix(prefix)?;
    let params = params_of(entry.signature(), entry.arity());
    let names: Vec<&str> = params.iter().map(String::as_str).collect();
    Some(FunctionDescriptor::new(name, &names, entry.doc()))
}

/// The parameter names of a signature such as `name(a: string, b: Dynamic) -> string`, when they are as many as the arity.
/// Otherwise, or for a parameter without a name, the names are `arg1`, `arg2`, and so on.
fn params_of(signature: &str, arity: usize) -> Vec<String> {
    let named: Vec<String> = parameter_texts(signature)
        .iter()
        .map(|text| {
            text.split_once(':')
                .map_or_else(String::new, |(name, _type)| name.trim().to_owned())
        })
        .collect();
    let usable = named.len() == arity && named.iter().all(|name| !name.is_empty() && name != "_");
    if usable {
        named
    } else {
        (1..=arity).map(|index| format!("arg{index}")).collect()
    }
}

/// The texts of the parameters between the first parenthesis of a signature and its matching close, split at the top-level
/// commas. Nested brackets, parentheses and angle brackets keep their commas.
fn parameter_texts(signature: &str) -> Vec<String> {
    let Some(open) = signature.find('(') else {
        return Vec::new();
    };
    let inside = signature.get(open.saturating_add(1)..).unwrap_or_default();
    let mut texts = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;
    for character in inside.chars() {
        match character {
            ')' if depth == 0 => break,
            ',' if depth == 0 => texts.push(std::mem::take(&mut current)),
            '(' | '[' | '<' => {
                depth = depth.saturating_add(1);
                current.push(character);
            }
            ')' | ']' | '>' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            _ => current.push(character),
        }
    }
    if !current.trim().is_empty() {
        texts.push(current);
    }
    texts
        .into_iter()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

/// The documentation of a shell function as doc-comment lines, each starting with `///` as Rhai reads them.
fn comments_of(doc: Option<&str>) -> Vec<String> {
    doc.map_or_else(Vec::new, |doc| {
        doc.lines().map(|line| format!("/// {line}")).collect()
    })
}

/// The body of a shell function: every call is refused. The refusal travels through Rhai as the value of a runtime
/// error, which `ScratchpadError::from_rhai` gives back as `InspectionOnly`. Nothing else runs.
fn refusal(path: String) -> RhaiFunc {
    RhaiFunc::Pure {
        func: Shared::new(
            move |_context: Option<NativeCallContext<'_>>,
                  _args: &mut [&mut Dynamic]|
                  -> Result<Dynamic, Box<EvalAltResult>> {
                let marker = ScratchpadError::InspectionOnly { name: path.clone() };
                Err(Box::new(EvalAltResult::ErrorRuntime(
                    Dynamic::from(marker),
                    Position::NONE,
                )))
            },
        ),
        has_context: false,
        is_pure: false,
        is_volatile: true,
    }
}

/// The error of a name that the inspection pad would declare twice.
fn duplicate(kind: &str, name: &str) -> ScratchpadError {
    ScratchpadError::Engine {
        message: format!("the {kind} `{name}` is declared twice in the inspection pad"),
    }
}

/// The engine builder of an inspection pad: the limits of the base engine, the toolsets and the skills of the target
/// grants, and one shell module per described module. No group is granted.
fn shell_builder(
    base: &ScratchpadEngine,
    grants: &Grants,
    modules: &[ModuleDescriptor],
) -> ScratchpadResult<EngineBuilder> {
    let mut shells = Grants::new();
    for toolset in grants.toolsets() {
        shells = shells
            .with_toolset(toolset.clone())
            .ok_or_else(|| duplicate("toolset", toolset.name()))?;
    }
    for skill in grants.skills() {
        shells = shells
            .with_skill(skill.clone())
            .ok_or_else(|| duplicate("skill", skill.name()))?;
    }
    for descriptor in modules {
        let shell =
            GrantedModule::new(&descriptor.name, Shared::new(shell_module(descriptor))).ok_or_else(|| {
                ScratchpadError::Engine {
                    message: format!("the module `{}` cannot be an inspection shell", descriptor.name),
                }
            })?;
        shells = shells
            .with_module(shell)
            .ok_or_else(|| duplicate("module", &descriptor.name))?;
    }
    Ok(ScratchpadEngine::builder(*base.limits(), shells))
}

/// The source texts that `source` reads: those of a session target, none for a definition.
fn sources_of(target: &InspectionTarget) -> BTreeMap<String, String> {
    match target {
        InspectionTarget::Definition => BTreeMap::new(),
        InspectionTarget::Session(snapshot) => snapshot.functions().clone(),
    }
}

/// Registers `source(name)` on the engine of an inspection pad. Its sources are keyed `name/arity`.
fn register_source(engine: &mut Engine, sources: Arc<BTreeMap<String, String>>) {
    register(
        engine,
        "source",
        &["name: string", "string"],
        "/// Source text of a function the LLM defined in the session. The name alone, or `name/arity` when several functions share it. Fails with the known functions otherwise.",
        move |name: ImmutableString| source_of(&sources, name.as_str()),
    );
}

/// The source text of the function named `name`, a key `name/arity` or a bare name that only one function has.
fn source_of(sources: &BTreeMap<String, String>, name: &str) -> Result<String, Box<EvalAltResult>> {
    if let Some(text) = sources.get(name) {
        return Ok(text.clone());
    }
    let matching: Vec<(&String, &String)> = sources
        .iter()
        .filter(|(key, _text)| {
            key.rsplit_once('/')
                .is_some_and(|(function, _arity)| function == name)
        })
        .collect();
    match matching.as_slice() {
        [(_key, text)] => Ok((*text).clone()),
        [] => Err(Box::new(source_error(&format!(
            "source: unknown function `{name}`; the session defines: {}",
            known_list(sources)
        )))),
        several => Err(Box::new(source_error(&format!(
            "source: `{name}` names several functions, give the arity: {}",
            several
                .iter()
                .map(|(key, _text)| key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))),
    }
}

/// The keys of the known functions, or `none`.
fn known_list(sources: &BTreeMap<String, String>) -> String {
    if sources.is_empty() {
        "none".to_owned()
    } else {
        sources.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
    }
}

/// A runtime error of a script, carrying its text.
fn source_error(message: &str) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(message.to_owned()), Position::NONE)
}

impl InspectionPad {
    /// Builds the inspection pad of a target. The base engine gives the limits; the target grants give the toolsets and
    /// the skills; the modules are projected as shells from their descriptors. No group is granted. The engine also
    /// holds `source`, which the catalog does not list.
    ///
    /// # Errors
    ///
    /// Returns an engine error when a module name is declared twice, or when the engine cannot be built.
    pub fn new(
        base: &ScratchpadEngine,
        grants: &Grants,
        modules: &[ModuleDescriptor],
        target: InspectionTarget,
    ) -> ScratchpadResult<Self> {
        let sources = Arc::new(sources_of(&target));
        let engine = Arc::new(
            shell_builder(base, grants, modules)?
                .with_unlisted_registration(move |engine| {
                    register_source(engine, sources);
                    Ok(())
                })
                .build()?,
        );
        let pad = match target {
            InspectionTarget::Definition => {
                Scratchpad::inspection(engine, &BTreeMap::new(), &BTreeMap::new())
            }
            InspectionTarget::Session(snapshot) => {
                Scratchpad::inspection(engine, snapshot.variables(), snapshot.functions())
            }
        };
        Ok(Self { pad })
    }

    /// Runs a script in the inspection pad, under the cancellation handle of the run.
    pub async fn run(&self, script: &str, cancel: &CancelHandle, env: &RunEnv<'_>) -> RunOutcome {
        self.pad.run(script, cancel, env).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FunctionDescriptor, InspectionPad, InspectionTarget, ModuleDescriptor, descriptors_from_catalog,
        params_of,
    };
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        Catalog, GrantedModule, Grants, PadSnapshot, RunEnv, RunOutcome, RunStatus, Scratchpad,
        ScratchpadEngine, ScratchpadLimits, ScriptErrorClass,
    };
    use crate::session::{CancelHandle, Redactor};
    use rhai::{FuncRegistration, ImmutableString, Module, Shared};

    /// A base engine with the default limits and no granted group.
    fn base() -> ScratchpadEngine {
        match ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new()).build() {
            Ok(engine) => engine,
            Err(error) => panic!("the base engine must build: {error}"),
        }
    }

    /// A module `grafana` with `query(expr)` documented as `Runs a query`.
    fn grafana() -> ModuleDescriptor {
        ModuleDescriptor::new("grafana")
            .expect("a valid module name")
            .with_function(FunctionDescriptor::new("query", &["expr"], Some("Runs a query")))
    }

    /// Runs a script in a pad with an empty redactor, a fresh clock and a handle that is not cancelled.
    async fn eval(pad: &InspectionPad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    /// The pad snapshot of a session that ran `script` on a plain pad.
    async fn snapshot_of(script: &str) -> PadSnapshot {
        let pad = Scratchpad::new(std::sync::Arc::new(base()));
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        let outcome = pad.run(script, &cancel, &env).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the session script must run: {}",
            outcome.text()
        );
        pad.snapshot()
    }

    /// The class of the failure of a run, which must have failed.
    fn failed(outcome: &RunOutcome) -> ScriptErrorClass {
        match outcome.status() {
            RunStatus::Failed(class) => class,
            RunStatus::Ok | RunStatus::Cancelled => {
                panic!(
                    "the run must fail, its status is {:?}: {}",
                    outcome.status(),
                    outcome.text()
                )
            }
        }
    }

    // Scenario: la fonction de coquille est listée avec sa documentation
    #[tokio::test]
    async fn shell_function_lists_with_its_documentation() {
        let pad = InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana()],
            InspectionTarget::Definition,
        );
        let Ok(pad) = pad else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(
            &pad,
            r#"let f = functions("grafana")[0]; f.path + " | " + f.signature + " | " + f.doc"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the listing runs: {}",
            outcome.text()
        );
        assert_eq!(
            outcome.text(),
            "grafana::query | query(expr) | Runs a query",
            "the path, the signature and the documentation of the shell are listed"
        );
    }

    // Scenario: l'appel à une coquille échoue sans effet de bord
    #[tokio::test]
    async fn shell_call_fails_without_side_effect() {
        let Ok(pad) = InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana()],
            InspectionTarget::Definition,
        ) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, r#"grafana::query("up")"#).await;
        assert_eq!(
            failed(&outcome),
            ScriptErrorClass::Runtime,
            "a refused call is a runtime failure"
        );
        assert!(
            outcome.text().contains("grafana::query is not executed"),
            "the text names the shell function and says it is not executed: {}",
            outcome.text()
        );
    }

    // Scenario: les variables de session sont en lecture seule
    #[tokio::test]
    async fn session_variables_are_read_only() {
        let snapshot = snapshot_of("let n = 3;").await;
        let Ok(pad) = InspectionPad::new(&base(), &Grants::new(), &[], InspectionTarget::Session(snapshot))
        else {
            panic!("the inspection pad must build");
        };
        let read = eval(&pad, "n").await;
        assert_eq!(read.status(), RunStatus::Ok, "the read runs: {}", read.text());
        assert_eq!(read.text(), "3", "the variable reads its value");
        let write = eval(&pad, "n = 4").await;
        assert_eq!(
            failed(&write),
            ScriptErrorClass::Runtime,
            "a write to a session variable fails"
        );
        assert!(
            write.text().contains("constant"),
            "the text says the variable is read-only: {}",
            write.text()
        );
        let again = eval(&pad, "n").await;
        assert_eq!(again.text(), "3", "the failed write left the variable unchanged");
    }

    // Scenario: les fonctions de session sont listées mais non appelables
    #[tokio::test]
    async fn session_functions_are_listed_but_not_callable() {
        let snapshot = snapshot_of("fn f(x) { x + 1 }").await;
        let Ok(pad) = InspectionPad::new(&base(), &Grants::new(), &[], InspectionTarget::Session(snapshot))
        else {
            panic!("the inspection pad must build");
        };
        let listed = eval(&pad, r#"functions("script")[0].path"#).await;
        assert_eq!(
            listed.status(),
            RunStatus::Ok,
            "the listing runs: {}",
            listed.text()
        );
        assert_eq!(listed.text(), "f", "the session function is listed");
        let source = eval(&pad, r#"source("f")"#).await;
        assert_eq!(
            source.status(),
            RunStatus::Ok,
            "the source of the listed function is read: {}",
            source.text()
        );
        assert_eq!(
            source.text(),
            "fn f(x) { x + 1 }",
            "the source text of the function is returned"
        );
        let called = eval(&pad, "f(1)").await;
        assert_eq!(
            failed(&called),
            ScriptErrorClass::NotFound,
            "the listed function cannot be called"
        );
    }

    // Scenario: les fonctions de session sont listées mais non appelables (la source d'une fonction se lit par `source`,
    // sur le bac d'inspection seulement)
    #[tokio::test]
    async fn source_is_refused_on_a_definition_target() {
        let Ok(pad) = InspectionPad::new(&base(), &Grants::new(), &[], InspectionTarget::Definition) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, r#"source("f")"#).await;
        assert_eq!(
            failed(&outcome),
            ScriptErrorClass::Runtime,
            "a definition has no function to read"
        );
        assert!(
            outcome.text().contains("unknown function `f`"),
            "the text names the unknown function: {}",
            outcome.text()
        );
    }

    #[tokio::test]
    async fn source_of_an_unknown_name_names_the_known_functions() {
        let snapshot = snapshot_of("fn f(x) { x + 1 }").await;
        let Ok(pad) = InspectionPad::new(&base(), &Grants::new(), &[], InspectionTarget::Session(snapshot))
        else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, r#"source("g")"#).await;
        assert_eq!(
            failed(&outcome),
            ScriptErrorClass::Runtime,
            "an unknown name is a runtime error of the source function"
        );
        assert!(
            outcome.text().contains("unknown function `g`") && outcome.text().contains("f/1"),
            "the text names the unknown name and the known function f/1: {}",
            outcome.text()
        );
    }

    #[tokio::test]
    async fn source_of_an_ambiguous_name_takes_the_arity() {
        let snapshot = snapshot_of("fn f(x) { x } fn f(x, y) { x + y }").await;
        let Ok(pad) = InspectionPad::new(&base(), &Grants::new(), &[], InspectionTarget::Session(snapshot))
        else {
            panic!("the inspection pad must build");
        };
        let ambiguous = eval(&pad, r#"source("f")"#).await;
        assert_eq!(
            failed(&ambiguous),
            ScriptErrorClass::Runtime,
            "a bare name of two overloads is ambiguous"
        );
        assert!(
            ambiguous.text().contains("f/1") && ambiguous.text().contains("f/2"),
            "the text lists both arities: {}",
            ambiguous.text()
        );
        let by_arity = eval(&pad, r#"source("f/2")"#).await;
        assert_eq!(
            by_arity.text(),
            "fn f(x, y) { x + y }",
            "the arity selects the overload"
        );
    }

    #[tokio::test]
    async fn source_is_not_on_the_ordinary_pad() {
        let pad = Scratchpad::new(std::sync::Arc::new(base()));
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        let outcome = pad.run(r#"fn f(x) { x + 1 } source("f")"#, &cancel, &env).await;
        assert_eq!(
            failed(&outcome),
            ScriptErrorClass::NotFound,
            "the ordinary pad has no `source` function"
        );
    }

    // Scenario: aucun groupe fs ni shell (le groupe Host est absent aussi)
    #[tokio::test]
    async fn host_group_is_absent_even_when_the_target_grants_it() {
        let target = Grants::new().with_group(crate::scratchpad::CoreGroup::Host);
        let Ok(pad) = InspectionPad::new(&base(), &target, &[grafana()], InspectionTarget::Definition) else {
            panic!("the inspection pad must build");
        };
        let names = eval(&pad, r#"let names = modules().map(|m| m.name); "host" in names"#).await;
        assert_eq!(names.text(), "false", "the host group is not listed");
        let listed = eval(
            &pad,
            r#"functions("global").filter(|f| f.path.starts_with("get_env") || f.path.starts_with("log_")).len()"#,
        )
        .await;
        assert_eq!(listed.text(), "0", "no host function is listed");
        let env = eval(&pad, r#"get_env("HOME")"#).await;
        assert_eq!(
            failed(&env),
            ScriptErrorClass::NotFound,
            "get_env is not reachable"
        );
        let log = eval(&pad, r#"log_info("x")"#).await;
        assert_eq!(
            failed(&log),
            ScriptErrorClass::NotFound,
            "log_info is not reachable"
        );
    }

    #[test]
    fn a_module_declared_twice_is_refused() {
        match InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana(), grafana()],
            InspectionTarget::Definition,
        ) {
            Ok(_pad) => panic!("two declarations of the same module must be refused"),
            Err(error) => assert!(
                error.to_string().contains("module `grafana` is declared twice"),
                "the error names the module: {error}"
            ),
        }
    }

    /// A granted module `grafana` with `loki_query(query, range)` documented, and `ping()` without documentation.
    fn grafana_granted() -> GrantedModule {
        let mut module = Module::new();
        let _loki = FuncRegistration::new("loki_query")
            .with_comments(["/// Runs a Loki query"])
            .with_params_info(["query: string", "range: string", "string"])
            .set_into_module(&mut module, |query: ImmutableString, range: ImmutableString| {
                format!("{query}/{range}")
            });
        let _ping = FuncRegistration::new("ping").set_into_module(&mut module, || "pong".to_owned());
        let Some(granted) = GrantedModule::new("grafana", Shared::new(module)) else {
            panic!("a valid module name");
        };
        granted
    }

    /// The catalog of an engine that grants `grafana_granted`.
    fn catalog_with_grafana() -> Catalog {
        let Some(grants) = Grants::new().with_module(grafana_granted()) else {
            panic!("the module is added once");
        };
        match ScratchpadEngine::builder(ScratchpadLimits::new(), grants).build() {
            Ok(engine) => engine.catalog().clone(),
            Err(error) => panic!("the engine must build: {error}"),
        }
    }

    #[test]
    fn descriptors_list_the_direct_functions_of_a_requested_module() {
        let catalog = catalog_with_grafana();
        let descriptors = descriptors_from_catalog(&catalog, &["grafana"]);
        let expected = ModuleDescriptor::new("grafana").map(|module| {
            module
                .with_function(FunctionDescriptor::new(
                    "loki_query",
                    &["query", "range"],
                    Some("Runs a Loki query"),
                ))
                .with_function(FunctionDescriptor::new("ping", &[], None))
        });
        assert_eq!(
            descriptors,
            expected.into_iter().collect::<Vec<_>>(),
            "the module carries its functions, their parameter names and their documentation"
        );
    }

    #[test]
    fn descriptors_ignore_an_unknown_module() {
        let catalog = catalog_with_grafana();
        assert!(
            descriptors_from_catalog(&catalog, &["other"]).is_empty(),
            "a module the catalog does not list gives no descriptor"
        );
    }

    #[tokio::test]
    async fn a_real_module_projected_as_a_shell_is_listed_and_refused() {
        let catalog = catalog_with_grafana();
        let descriptors = descriptors_from_catalog(&catalog, &["grafana"]);
        let Ok(pad) = InspectionPad::new(
            &base(),
            &Grants::new(),
            &descriptors,
            InspectionTarget::Definition,
        ) else {
            panic!("the inspection pad must build");
        };
        let listed = eval(
            &pad,
            r#"let f = functions("grafana")[0]; f.path + " | " + f.signature + " | " + f.doc"#,
        )
        .await;
        assert_eq!(
            listed.text(),
            "grafana::loki_query | loki_query(query, range) | Runs a Loki query",
            "the projected module is listed with the names and the documentation of its functions"
        );
        let called = eval(&pad, r#"grafana::loki_query("a", "b")"#).await;
        assert_eq!(
            failed(&called),
            ScriptErrorClass::Runtime,
            "the projected function is a shell and refuses the call"
        );
        assert!(
            called.text().contains("grafana::loki_query is not executed"),
            "the refusal names the function: {}",
            called.text()
        );
    }

    // Scenario: une cible de définition n'a aucune variable
    #[tokio::test]
    async fn definition_target_has_no_variables() {
        let Ok(pad) = InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana()],
            InspectionTarget::Definition,
        ) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, "vars().len()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the count runs: {}",
            outcome.text()
        );
        assert_eq!(outcome.text(), "0", "a definition has no variable");
    }

    // Scenario: aucun groupe fs ni shell (ils relèvent de Host, jamais accordé à l'inspection)
    #[tokio::test]
    async fn no_filesystem_or_shell_group() {
        let target = Grants::new();
        let Ok(pad) = InspectionPad::new(&base(), &target, &[grafana()], InspectionTarget::Definition) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(
            &pad,
            r#"let names = modules().map(|m| m.name); "fs" in names || "shell" in names"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the listing runs: {}",
            outcome.text()
        );
        assert_eq!(outcome.text(), "false", "neither `fs` nor `shell` is listed");
        let file = eval(&pad, r#"file_read("/etc/hostname")"#).await;
        assert_eq!(
            failed(&file),
            ScriptErrorClass::NotFound,
            "file functions are not reachable"
        );
    }

    #[test]
    fn parameter_names_come_from_the_signature() {
        assert_eq!(
            params_of("loki(query: string, range: string) -> string", 2),
            vec!["query", "range"]
        );
        assert_eq!(
            params_of("f(a: Map<String, Dynamic>, b: i64) -> i64", 2),
            vec!["a", "b"],
            "a comma inside angle brackets does not split a parameter"
        );
        assert_eq!(
            params_of("f(a: Fn(int, int) -> int, b: i64) -> i64", 2),
            vec!["a", "b"],
            "a comma inside parentheses does not split a parameter"
        );
        assert!(
            params_of("ping() -> string", 0).is_empty(),
            "a function without parameter has none"
        );
    }

    #[test]
    fn unnamed_or_mismatched_parameters_get_generic_names() {
        assert_eq!(
            params_of("pair(_: i64, _: i64) -> i64", 2),
            vec!["arg1", "arg2"],
            "the placeholder name `_` is replaced"
        );
        assert_eq!(
            params_of("f(i64) -> i64", 1),
            vec!["arg1"],
            "a parameter with a type only has no name"
        );
        assert_eq!(
            params_of("f(a: i64) -> i64", 2),
            vec!["arg1", "arg2"],
            "names that do not match the arity are replaced"
        );
    }

    #[test]
    fn module_descriptor_refuses_an_invalid_name() {
        for name in ["1abc", "a-b", "", "fn", "let"] {
            assert!(
                ModuleDescriptor::new(name).is_none(),
                "the name `{name}` is refused"
            );
        }
        assert!(
            ModuleDescriptor::new("grafana_prod").is_some(),
            "a valid name is accepted"
        );
    }

    #[tokio::test]
    async fn shell_function_keeps_its_arity() {
        let Ok(pad) = InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana()],
            InspectionTarget::Definition,
        ) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, "grafana::query()").await;
        assert_eq!(
            failed(&outcome),
            ScriptErrorClass::NotFound,
            "a call with another arity matches no shell"
        );
    }

    #[tokio::test]
    async fn module_outside_the_target_is_not_reachable() {
        let Ok(pad) = InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana()],
            InspectionTarget::Definition,
        ) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, "other::query(1)").await;
        assert_eq!(
            failed(&outcome),
            ScriptErrorClass::NotFound,
            "a module that is not in the target is not found"
        );
    }

    #[tokio::test]
    async fn toolsets_of_the_target_are_listed() {
        let Some(toolset) = crate::scratchpad::GrantedToolset::new("grafana", "Dashboards and logs") else {
            panic!("a valid toolset");
        };
        let Some(target) = Grants::new().with_toolset(toolset) else {
            panic!("the toolset is added once");
        };
        let Ok(pad) = InspectionPad::new(&base(), &target, &[], InspectionTarget::Definition) else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, "toolsets().len()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the count runs: {}",
            outcome.text()
        );
        assert_eq!(outcome.text(), "1", "the toolset of the target is listed");
    }

    // T9 (1): the outcome of an inspection run carries an empty snapshot, whatever the target holds and the script defines.
    #[tokio::test]
    async fn inspection_outcome_carries_an_empty_snapshot() {
        let snapshot = snapshot_of("let n = 3;").await;
        let Ok(pad) = InspectionPad::new(&base(), &Grants::new(), &[], InspectionTarget::Session(snapshot))
        else {
            panic!("the inspection pad must build");
        };
        let outcome = eval(&pad, "let m = 4; fn g(x) { x + 1 } n + m").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the run succeeds: {}",
            outcome.text()
        );
        assert_eq!(
            outcome.text(),
            "7",
            "the target variable and the script variable are both read"
        );
        let carried = outcome.snapshot();
        assert!(
            carried.variables().is_empty(),
            "the outcome of an inspection carries no variable, got {:?}",
            carried.variables().keys().collect::<Vec<_>>()
        );
        assert!(
            carried.functions().is_empty(),
            "the outcome of an inspection carries no function source, got {:?}",
            carried.functions().keys().collect::<Vec<_>>()
        );
        assert!(
            carried.warnings().is_empty(),
            "the outcome of an inspection carries no persistence warning"
        );
        assert!(
            outcome.warnings().is_empty(),
            "the outcome of an inspection raises no persistence warning"
        );
    }

    // T9 (3): `source` is callable on the inspection pad but no listing shows it (`functions()`, `functions(origin)`,
    // `modules()`).
    #[tokio::test]
    async fn source_is_never_listed() {
        let snapshot = snapshot_of("fn f(x) { x + 1 }").await;
        let Ok(pad) = InspectionPad::new(
            &base(),
            &Grants::new(),
            &[grafana()],
            InspectionTarget::Session(snapshot),
        ) else {
            panic!("the inspection pad must build");
        };
        let read = eval(&pad, r#"source("f")"#).await;
        assert_eq!(
            read.text(),
            "fn f(x) { x + 1 }",
            "control: source is callable on this pad, so the listings below are about a live function"
        );
        for script in [
            r#"functions().filter(|f| f.path == "source").len()"#,
            r#"functions("global").filter(|f| f.path == "source").len()"#,
            r#"functions("script").filter(|f| f.path == "source").len()"#,
            r#"functions("grafana").filter(|f| f.path == "source").len()"#,
        ] {
            let outcome = eval(&pad, script).await;
            assert_eq!(
                outcome.text(),
                "0",
                "`source` must not be listed by {script}: {}",
                outcome.text()
            );
        }
        let modules = eval(&pad, r#"modules().filter(|m| m.name == "source").len()"#).await;
        assert_eq!(modules.text(), "0", "`source` is not a module either");
    }
}
