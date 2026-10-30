//! Engine of a session: the Rhai engine built from the common core of `vynil-core`, restricted to the granted groups and
//! modules, with the scratchpad's own functions, its limits, a closed module resolver, a captured output and the
//! cancellation hook. See `src/scratchpad/engine.sdd`.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::Arc;

use rhai::module_resolvers::DummyModuleResolver;
use rhai::packages::Package as _;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicBlobPackage, BasicFnPackage, BasicIteratorPackage,
    BasicMapPackage, BasicMathPackage, BasicStringPackage, BasicTimePackage, BitFieldPackage, LogicPackage,
    MoreStringPackage,
};
use rhai::{Dynamic, Engine as RhaiEngine, Position, Shared};

use super::error::{ScratchpadError, ScratchpadResult};
use super::format::register_format_functions;
use super::grant::{CoreGroup, Grants};
use super::introspect::{Catalog, IntrospectionSource, Origin, register_introspection, registration_step};
use super::limits::ScratchpadLimits;
use crate::session::CancelHandle;

/// A registration closure of the host or of a trusted domain, run once when the engine is built.
type Registration = Box<dyn FnOnce(&mut RhaiEngine) -> ScratchpadResult<()>>;

/// The Rhai engine of a session, with its granted surface, its limits and its introspection. It is `Send + Sync`: one
/// engine serves every run of its session, behind an `Arc`.
#[non_exhaustive]
pub struct ScratchpadEngine {
    engine: RhaiEngine,
    grants: Grants,
    limits: ScratchpadLimits,
    catalog: Catalog,
    introspection: Arc<IntrospectionSource>,
}

/// Builder of a [`ScratchpadEngine`]. Its groups and modules come from the grants.
///
/// The registration closures run once, at build time, after the pure group, the format functions and the granted groups
/// and modules.
#[non_exhaustive]
pub struct EngineBuilder {
    limits: ScratchpadLimits,
    grants: Grants,
    registrations: Vec<Registration>,
    unlisted: Vec<Registration>,
}

impl ScratchpadEngine {
    /// A builder for an engine with these limits and these grants.
    #[must_use]
    pub fn builder(limits: ScratchpadLimits, grants: Grants) -> EngineBuilder {
        EngineBuilder {
            limits,
            grants,
            registrations: Vec::new(),
            unlisted: Vec::new(),
        }
    }

    /// The limits the engine was built with.
    #[must_use]
    pub const fn limits(&self) -> &ScratchpadLimits {
        &self.limits
    }

    /// The grants the engine was built with.
    #[must_use]
    pub const fn grants(&self) -> &Grants {
        &self.grants
    }

    /// The catalog of the functions the scripts can call.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The Rhai engine, for the runs of the session and the persistence of their scope.
    pub(in crate::scratchpad) const fn rhai(&self) -> &RhaiEngine {
        &self.engine
    }

    /// The state the introspection functions read, updated by each run before it evaluates its script.
    pub(in crate::scratchpad) const fn introspection(&self) -> &Arc<IntrospectionSource> {
        &self.introspection
    }
}

impl std::fmt::Debug for ScratchpadEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScratchpadEngine")
            .field("grants", &self.grants)
            .field("limits", &self.limits)
            .field("functions", &self.catalog.functions().len())
            .finish_non_exhaustive()
    }
}

impl EngineBuilder {
    /// Adds a registration closure, run once when the engine is built. Its functions are catalogued with the host origin.
    #[must_use]
    pub fn with_registration<F>(mut self, registration: F) -> Self
    where
        F: FnOnce(&mut RhaiEngine) -> ScratchpadResult<()> + 'static,
    {
        self.registrations.push(Box::new(registration));
        self
    }

    /// Adds a registration closure run after the catalog and the introspection: its functions are callable but the
    /// catalog does not list them. Only the inspection pad uses it, for its own functions.
    #[must_use]
    pub(in crate::scratchpad) fn with_unlisted_registration<F>(mut self, registration: F) -> Self
    where
        F: FnOnce(&mut RhaiEngine) -> ScratchpadResult<()> + 'static,
    {
        self.unlisted.push(Box::new(registration));
        self
    }

    /// Builds the engine: the base engine, the pure group, the format functions, the granted groups in the order host,
    /// sleep, files, shell, the granted modules, the registration closures, then the catalog and the introspection, and
    /// last the unlisted registration closures.
    ///
    /// # Errors
    ///
    /// Returns an engine error when a granted group is not available in this build, when a registration closure fails, or
    /// when the metadata of the engine cannot be read.
    pub fn build(self) -> ScratchpadResult<ScratchpadEngine> {
        let Self {
            limits,
            grants,
            registrations,
            unlisted,
        } = self;
        let mut engine = base_engine(&limits);
        let mut steps = vec![registration_step(
            &mut engine,
            Origin::Core("pure".to_owned()),
            |engine| {
                vynil_core::engine::core_pure_rhai_register(engine);
                Ok(())
            },
        )?];
        steps.push(registration_step(&mut engine, Origin::Format, |engine| {
            register_format_functions(engine);
            Ok(())
        })?);
        for group in grants.groups() {
            let group = *group;
            steps.push(registration_step(
                &mut engine,
                Origin::Core(group_name(group).to_owned()),
                |engine| register_group(engine, group),
            )?);
        }
        for module in grants.modules() {
            let _module = engine.register_static_module(module.name(), Shared::clone(module.module()));
        }
        for registration in registrations {
            steps.push(registration_step(&mut engine, Origin::Host, registration)?);
        }
        let catalog = Catalog::build(&engine, &grants, &steps)?;
        let source = Arc::new(IntrospectionSource::new(grants.clone(), catalog.clone(), limits));
        register_introspection(&mut engine, &source);
        for registration in unlisted {
            registration(&mut engine)?;
        }
        Ok(ScratchpadEngine {
            engine,
            grants,
            limits,
            catalog,
            introspection: source,
        })
    }
}

impl std::fmt::Debug for EngineBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBuilder")
            .field("grants", &self.grants)
            .field("limits", &self.limits)
            .field("registrations", &self.registrations.len())
            .field("unlisted", &self.unlisted.len())
            .finish_non_exhaustive()
    }
}

/// The Rhai engine every session engine and every toolset module is built from: `const` disabled, the module resolver
/// closed, `print` and `debug` captured, the progress hook reading the run's cancellation handle, and the memory limits.
/// It has no granted function.
pub(in crate::scratchpad) fn base_engine(limits: &ScratchpadLimits) -> RhaiEngine {
    let mut engine = RhaiEngine::new_raw();
    register_standard_packages(&mut engine);
    let _const = engine.disable_symbol("const");
    let _resolver = engine.set_module_resolver(DummyModuleResolver);
    let _print = engine.on_print(capture_print);
    let _debug = engine.on_debug(capture_debug);
    limits.apply_to(&mut engine);
    let _progress = engine.on_progress(cancellation_requested);
    engine
}

/// The standard packages of Rhai, all but the language core package. `Engine::new()` loads that package too, and its
/// `sleep` blocks the thread with no cancellation point; it also exposes the engine's own `exit`, `take`, `tag`,
/// `parse_json` and `get_fn_metadata_list`, which no granted group provides. The other packages are the ones `Engine::new()`
/// loads, in the same order.
fn register_standard_packages(engine: &mut RhaiEngine) {
    let _arithmetic = engine.register_global_module(ArithmeticPackage::new().as_shared_module());
    let _strings = engine.register_global_module(BasicStringPackage::new().as_shared_module());
    let _iterators = engine.register_global_module(BasicIteratorPackage::new().as_shared_module());
    let _functions = engine.register_global_module(BasicFnPackage::new().as_shared_module());
    let _bit_fields = engine.register_global_module(BitFieldPackage::new().as_shared_module());
    let _logic = engine.register_global_module(LogicPackage::new().as_shared_module());
    let _math = engine.register_global_module(BasicMathPackage::new().as_shared_module());
    let _arrays = engine.register_global_module(BasicArrayPackage::new().as_shared_module());
    let _blobs = engine.register_global_module(BasicBlobPackage::new().as_shared_module());
    let _maps = engine.register_global_module(BasicMapPackage::new().as_shared_module());
    let _times = engine.register_global_module(BasicTimePackage::new().as_shared_module());
    let _more_strings = engine.register_global_module(MoreStringPackage::new().as_shared_module());
}

/// Registers one granted group of the common core. Fails when this build does not compile the feature the group needs.
fn register_group(engine: &mut RhaiEngine, group: CoreGroup) -> ScratchpadResult<()> {
    match group {
        CoreGroup::Host => vynil_core::engine::core_host_rhai_register(engine),
        CoreGroup::Sleep => vynil_core::engine::core_sleep_rhai_register(engine),
        CoreGroup::Fs => {
            require_feature(cfg!(feature = "fs"), "fs", "fs")?;
            #[cfg(feature = "fs")]
            vynil_core::engine::fs_rhai_register(engine);
        }
        CoreGroup::Shell => {
            require_feature(cfg!(feature = "shell"), "shell", "shell")?;
            #[cfg(feature = "shell")]
            vynil_core::shell::shell_rhai_register(engine);
        }
    }
    Ok(())
}

/// Refuses a group whose `vynil-core` feature this build does not enable.
fn require_feature(available: bool, group: &str, feature: &str) -> ScratchpadResult<()> {
    if available {
        Ok(())
    } else {
        Err(ScratchpadError::Engine {
            message: format!(
                "group `{group}` is granted but this build of lymaille does not enable the `{feature}` feature"
            ),
        })
    }
}

/// Name of a group in the catalog, as `modules()` shows it.
const fn group_name(group: CoreGroup) -> &'static str {
    match group {
        CoreGroup::Host => "host",
        CoreGroup::Sleep => "sleep",
        CoreGroup::Fs => "fs",
        CoreGroup::Shell => "shell",
    }
}

thread_local! {
    /// The run open on this thread, if any: its cancellation handle and what its `print` and `debug` calls captured.
    static RUN: RefCell<Option<RunState>> = const { RefCell::new(None) };
}

/// What a run holds on its thread while it is open.
#[derive(Debug)]
struct RunState {
    cancel: CancelHandle,
    output: String,
    output_chars: usize,
    cap_chars: usize,
}

impl RunState {
    /// Appends one line of captured text, up to the cap, in characters.
    fn capture(&mut self, text: &str) {
        for character in text.chars().chain(std::iter::once('\n')) {
            if self.output_chars >= self.cap_chars {
                break;
            }
            self.output.push(character);
            self.output_chars = self.output_chars.saturating_add(1);
        }
    }
}

/// Runs `action` on the run open on this thread. Nothing happens when no run is open or when the cell is busy.
fn with_run(action: impl FnOnce(&mut RunState)) {
    let _outcome = RUN.try_with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut()
            && let Some(run) = slot.as_mut()
        {
            action(run);
        }
    });
}

/// The `print` hook: the text goes into the run's buffer, or nowhere when no run is open.
fn capture_print(text: &str) {
    with_run(|run| run.capture(text));
}

/// The `debug` hook: as `print`, the source and the position being left out.
fn capture_debug(text: &str, _source: Option<&str>, _position: Position) {
    with_run(|run| run.capture(text));
}

/// The progress hook: stops the script once the run's handle is cancelled. Without a run, nothing is stopped.
fn cancellation_requested(_operations: u64) -> Option<Dynamic> {
    let requested = RUN.try_with(|cell| {
        cell.try_borrow()
            .is_ok_and(|slot| slot.as_ref().is_some_and(|run| run.cancel.is_cancelled()))
    });
    matches!(requested, Ok(true)).then_some(Dynamic::UNIT)
}

/// Guard that opens a run on the current thread: it installs the cancellation handle that the progress hook reads and the
/// buffer that `print` and `debug` write into, and restores what was installed before when dropped, panic included.
///
/// The guard is not `Send`: the state it installs is thread-local, so the run must be dropped on the thread that opened it.
#[derive(Debug)]
pub(in crate::scratchpad) struct RunScope {
    previous: Option<RunState>,
    thread_bound: PhantomData<*const ()>,
}

impl RunScope {
    /// Opens a run whose handle is read by the progress hook and whose captured output holds at most `render_cap`
    /// characters.
    pub(in crate::scratchpad) fn enter(cancel: &CancelHandle, render_cap: usize) -> Self {
        let state = RunState {
            cancel: cancel.clone(),
            output: String::new(),
            output_chars: 0,
            cap_chars: render_cap,
        };
        let previous = RUN
            .try_with(|cell| {
                cell.try_borrow_mut()
                    .ok()
                    .and_then(|mut slot| slot.replace(state))
            })
            .ok()
            .flatten();
        Self {
            previous,
            thread_bound: PhantomData,
        }
    }
}

/// Text captured so far by `print` and `debug` in the run open on this thread, at most the render cap. Empty when no run is
/// open.
pub(in crate::scratchpad) fn captured_output() -> String {
    RUN.try_with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().map(|run| run.output.clone()))
    })
    .ok()
    .flatten()
    .unwrap_or_default()
}

impl Drop for RunScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        let _outcome = RUN.try_with(|cell| {
            if let Ok(mut slot) = cell.try_borrow_mut() {
                *slot = previous;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use rhai::{Array, Dynamic, FuncRegistration, Map, Module, Shared};

    use super::{EngineBuilder, RunScope, ScratchpadEngine, base_engine, captured_output};
    use crate::scratchpad::{
        CandidateSet, CoreGroup, GrantedModule, Grants, Origin, ScratchpadError, ScratchpadLimits,
        ScriptErrorClass, suggest_for,
    };
    use crate::session::CancelHandle;

    /// A module `grafana` with `loki_query(query)`, documented.
    fn grafana() -> GrantedModule {
        let mut module = Module::new();
        let _hash = FuncRegistration::new("loki_query")
            .with_comments(["/// Runs a Loki query"])
            .with_params_info(["query: string", "string"])
            .set_into_module(&mut module, |query: rhai::ImmutableString| {
                format!("loki:{query}")
            });
        GrantedModule::new("grafana", Shared::new(module)).expect("a valid name")
    }

    /// A toolset module `ts` with `f()`, which holds a dependency `dep` exposing `secret()`.
    fn toolset_module() -> GrantedModule {
        let mut dependency = Module::new();
        let _secret = FuncRegistration::new("secret").set_into_module(&mut dependency, || "token".to_owned());
        let mut module = Module::new();
        let _f = FuncRegistration::new("f").set_into_module(&mut module, || "ok".to_owned());
        let _dep = module.set_sub_module("dep", Shared::new(dependency));
        GrantedModule::new("ts", Shared::new(module)).expect("a valid name")
    }

    /// An engine built with these grants and the default limits.
    fn built(grants: Grants) -> ScratchpadEngine {
        match ScratchpadEngine::builder(ScratchpadLimits::new(), grants).build() {
            Ok(engine) => engine,
            Err(error) => panic!("the engine must build: {error}"),
        }
    }

    /// The value of a script, which must succeed.
    fn value(engine: &ScratchpadEngine, script: &str) -> Dynamic {
        match engine.rhai().eval::<Dynamic>(script) {
            Ok(value) => value,
            Err(error) => panic!("`{script}` must succeed: {error}"),
        }
    }

    /// The error of a script, classified as the scratchpad classifies it, which must fail.
    fn failure(engine: &ScratchpadEngine, script: &str) -> ScratchpadError {
        match engine.rhai().eval::<Dynamic>(script) {
            Ok(value) => panic!("`{script}` must fail, it returned {value}"),
            Err(error) => ScratchpadError::from_rhai(&error),
        }
    }

    /// The rows of an array returned by a script.
    fn rows(engine: &ScratchpadEngine, script: &str) -> Vec<Map> {
        match engine.rhai().eval::<Array>(script) {
            Ok(array) => array.into_iter().filter_map(Dynamic::try_cast::<Map>).collect(),
            Err(error) => panic!("`{script}` must succeed: {error}"),
        }
    }

    /// The text of a key of every row of a script result.
    fn column(engine: &ScratchpadEngine, script: &str, key: &str) -> Vec<String> {
        rows(engine, script)
            .iter()
            .map(|row| row.get(key).map(ToString::to_string).unwrap_or_default())
            .collect()
    }

    /// The candidates of the suggestions, built from the engine as the run builds them: visible functions, granted modules.
    fn candidates_of(engine: &ScratchpadEngine) -> CandidateSet {
        let functions = engine
            .catalog()
            .functions()
            .iter()
            .map(|entry| (entry.path().to_owned(), entry.signature().to_owned()))
            .collect();
        let modules = engine
            .grants()
            .modules()
            .iter()
            .map(|module| module.name().to_owned())
            .collect();
        CandidateSet::new(functions, modules, Vec::new())
    }

    #[test]
    fn only_the_pure_group_by_default() {
        let engine = built(Grants::new());
        assert_eq!(
            value(&engine, "json_encode(#{a: 1})")
                .into_string()
                .ok()
                .as_deref(),
            Some("{\"a\":1}"),
            "the pure group answers"
        );
        for script in [
            "get_env(\"PATH\")",
            "sleep(1)",
            "file_read(\"/etc/hostname\")",
            "shell_run(\"ls\")",
        ] {
            assert_eq!(
                failure(&engine, script).class(),
                ScriptErrorClass::NotFound,
                "`{script}` is a function-not-found error"
            );
        }
    }

    #[test]
    fn ungranted_groups_are_absent_from_introspection() {
        let engine = built(Grants::new());
        let global = column(&engine, "functions(\"global\")", "path");
        assert!(
            global.iter().any(|path| path == "json_encode"),
            "the pure group: {global:?}"
        );
        for name in ["get_env", "sleep", "file_read", "shell_run"] {
            assert!(
                !global.iter().any(|path| path == name),
                "`{name}` is absent: {global:?}"
            );
        }
        let all = column(&engine, "functions()", "path");
        assert!(
            !all.iter().any(|path| path == "shell_run"),
            "absent from functions(): {all:?}"
        );
    }

    #[test]
    fn functions_carry_signature_and_documentation() {
        let engine = built(Grants::new());
        let found = rows(&engine, "functions()")
            .into_iter()
            .find(|row| row.get("path").map(ToString::to_string).as_deref() == Some("json_encode"));
        let Some(row) = found else {
            panic!("json_encode is listed");
        };
        assert_eq!(
            row.get("signature").map(ToString::to_string).as_deref(),
            Some("json_encode(value: Dynamic) -> string"),
            "the real signature"
        );
        let doc = row.get("doc").map(ToString::to_string).unwrap_or_default();
        assert!(!doc.is_empty(), "documented: {row:?}");
    }

    #[test]
    fn granted_host_group_is_registered_and_sleep_is_not() {
        let engine = built(Grants::new().with_group(CoreGroup::Host));
        assert_eq!(
            value(&engine, "get_env(\"LYMAILLE_ABSENT\")")
                .into_string()
                .ok()
                .as_deref(),
            Some(""),
            "the granted host group answers"
        );
        assert_eq!(
            failure(&engine, "sleep(1)").class(),
            ScriptErrorClass::NotFound,
            "sleep is not granted"
        );
    }

    #[cfg(feature = "fs")]
    #[test]
    fn granted_fs_group_is_catalogued_under_its_feature() {
        let engine = built(Grants::new().with_group(CoreGroup::Fs));
        let global = column(&engine, "functions(\"global\")", "path");
        assert!(
            global.iter().any(|path| path == "file_read"),
            "the fs group is registered: {global:?}"
        );
    }

    #[cfg(feature = "shell")]
    #[test]
    fn granted_shell_group_is_catalogued_under_its_feature() {
        let engine = built(Grants::new().with_group(CoreGroup::Shell));
        let global = column(&engine, "functions(\"global\")", "path");
        assert!(
            global.iter().any(|path| path == "shell_run"),
            "the shell group is registered: {global:?}"
        );
    }

    #[cfg(not(feature = "fs"))]
    #[test]
    fn group_fs_refused_without_its_feature() {
        let grants = Grants::new().with_group(CoreGroup::Fs);
        match ScratchpadEngine::builder(ScratchpadLimits::new(), grants).build() {
            Ok(_) => panic!("a build without the fs feature cannot grant the fs group"),
            Err(error) => {
                assert_eq!(error.class(), ScriptErrorClass::Engine, "an engine error");
                assert!(error.to_string().contains("fs"), "names the group: {error}");
            }
        }
    }

    #[cfg(not(feature = "shell"))]
    #[test]
    fn group_shell_refused_without_its_feature() {
        let grants = Grants::new().with_group(CoreGroup::Shell);
        match ScratchpadEngine::builder(ScratchpadLimits::new(), grants).build() {
            Ok(_) => panic!("a build without the shell feature cannot grant the shell group"),
            Err(error) => {
                assert_eq!(error.class(), ScriptErrorClass::Engine, "an engine error");
                assert!(error.to_string().contains("shell"), "names the group: {error}");
            }
        }
    }

    #[test]
    fn const_is_a_syntax_error_on_both_engines_and_let_mutates() {
        let session = built(Grants::new());
        let toolset = base_engine(&ScratchpadLimits::new());
        for engine in [session.rhai(), &toolset] {
            match engine.eval::<Dynamic>("const C = #{a: 1}; C.a = 2") {
                Ok(value) => panic!("`const` must not compile, it returned {value}"),
                Err(error) => assert_eq!(
                    ScratchpadError::from_rhai(&error).class(),
                    ScriptErrorClass::Syntax,
                    "a syntax error"
                ),
            }
            assert_eq!(
                engine
                    .eval::<Dynamic>("let c = #{a: 1}; c.a = 2; c.a")
                    .ok()
                    .and_then(|value| value.as_int().ok()),
                Some(2),
                "let mutates"
            );
        }
    }

    #[test]
    fn import_is_closed() {
        let engine = built(Grants::new());
        assert!(
            engine
                .rhai()
                .eval::<Dynamic>("import \"anything\" as m; 1")
                .is_err(),
            "import is refused"
        );
    }

    #[test]
    fn print_and_debug_are_captured_in_the_run() {
        let engine = built(Grants::new());
        let _scope = RunScope::enter(&CancelHandle::new(), 100);
        let _value = value(&engine, "print(\"hello\"); debug(\"world\"); 1");
        let captured = captured_output();
        assert!(captured.contains("hello"), "print is captured: {captured:?}");
        assert!(captured.contains("world"), "debug is captured: {captured:?}");
    }

    #[test]
    fn print_outside_a_run_is_dropped_without_panic() {
        let engine = built(Grants::new());
        assert_eq!(
            value(&engine, "print(\"hello\"); 2").as_int().ok(),
            Some(2),
            "the script runs"
        );
    }

    #[test]
    fn captured_output_is_capped_at_the_render_cap() {
        let engine = built(Grants::new());
        let _scope = RunScope::enter(&CancelHandle::new(), 5);
        let _value = value(&engine, "print(\"hello world, a long line\"); 1");
        let captured = captured_output();
        assert_eq!(captured, "hello", "exactly the cap, the rest dropped");
    }

    #[test]
    fn granted_module_is_callable_and_an_ungranted_one_is_not() {
        let engine = built(Grants::new().with_module(grafana()).expect("first module"));
        assert_eq!(
            value(&engine, "grafana::loki_query(\"x\")")
                .into_string()
                .ok()
                .as_deref(),
            Some("loki:x"),
            "the granted module answers"
        );
        assert_eq!(
            failure(&engine, "other::f()").class(),
            ScriptErrorClass::NotFound,
            "the ungranted module is absent"
        );
    }

    #[test]
    fn toolset_dependency_stays_out_of_reach() {
        let engine = built(
            Grants::new()
                .with_module(toolset_module())
                .expect("the toolset module"),
        );
        assert_eq!(
            value(&engine, "ts::f()").into_string().ok().as_deref(),
            Some("ok"),
            "the toolset function works"
        );
        assert_eq!(
            failure(&engine, "dep::secret()").class(),
            ScriptErrorClass::NotFound,
            "dep is absent"
        );
        assert_eq!(
            failure(&engine, "ts::secret()").class(),
            ScriptErrorClass::NotFound,
            "secret is absent"
        );
        let paths = column(&engine, "functions()", "path");
        assert!(
            !paths
                .iter()
                .any(|path| path.contains("secret") || path.starts_with("dep")),
            "functions() mentions neither dep nor secret: {paths:?}"
        );
        let modules = column(&engine, "modules()", "name");
        assert!(
            !modules.iter().any(|name| name == "dep"),
            "modules() does not name dep: {modules:?}"
        );
        assert!(
            column(&engine, "find(\"secret\")", "path").is_empty(),
            "find does not reach the dependency"
        );
    }

    #[test]
    fn cancellation_stops_a_rhai_loop() {
        let engine = Arc::new(built(Grants::new()));
        let handle = CancelHandle::new();
        let (result_sender, result_receiver) = std::sync::mpsc::channel::<Option<ScriptErrorClass>>();
        let worker_engine = Arc::clone(&engine);
        let worker_handle = handle.clone();
        let _worker = std::thread::spawn(move || {
            let _scope = RunScope::enter(&worker_handle, 100);
            let outcome = worker_engine.rhai().eval::<Dynamic>("loop { }");
            let class = outcome
                .err()
                .map(|error| ScratchpadError::from_rhai(&error).class());
            let _sent = result_sender.send(class);
        });
        // The timer cancels after 200 milliseconds: the sender is kept alive until the end of the test.
        let (timer_keep_alive, timer_wait) = std::sync::mpsc::channel::<()>();
        let _timer = std::thread::spawn(move || {
            let _elapsed_or_closed = timer_wait.recv_timeout(Duration::from_millis(200));
            handle.cancel();
        });
        match result_receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(class) => assert_eq!(class, Some(ScriptErrorClass::Cancelled), "the loop is cancelled"),
            Err(error) => panic!("the loop was not stopped within five seconds: {error}"),
        }
        drop(timer_keep_alive);
    }

    // Scenario: try/catch does not survive the cancellation of a native call
    #[test]
    fn try_catch_does_not_survive_the_cancellation_of_a_native_call() {
        let handle = CancelHandle::new();
        let marks = Arc::new(AtomicUsize::new(0));
        let (native_handle, native_marks) = (handle.clone(), Arc::clone(&marks));
        let engine = match ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .with_registration(move |engine| {
                let cancelling = native_handle.clone();
                let _hash = engine.register_fn(
                    "cancelled_native",
                    move || -> Result<i64, Box<rhai::EvalAltResult>> {
                        cancelling.cancel();
                        Err("native call cancelled".into())
                    },
                );
                let counter = Arc::clone(&native_marks);
                let _mark = engine.register_fn("mark", move || {
                    i64::try_from(counter.fetch_add(1, Ordering::SeqCst)).unwrap_or(-1)
                });
                Ok(())
            })
            .build()
        {
            Ok(engine) => engine,
            Err(error) => panic!("the engine must build: {error}"),
        };
        // Control: without a run scope nothing stops the script, so the catch block and the next call both run.
        assert_eq!(
            value(&engine, "try { throw \"x\"; } catch (e) { mark(); } mark(); 7")
                .as_int()
                .ok(),
            Some(7),
            "control: the catch swallows an ordinary error and the script goes on"
        );
        assert_eq!(marks.load(Ordering::SeqCst), 2, "control: both marks ran");
        let _reset = marks.swap(0, Ordering::SeqCst);
        let result = {
            let _scope = RunScope::enter(&handle, 100);
            engine
                .rhai()
                .eval::<Dynamic>("try { cancelled_native(); } catch (e) { mark(); } mark(); 7")
        };
        let Err(error) = result else {
            panic!("the script must be stopped, it returned a value");
        };
        assert_eq!(
            ScratchpadError::from_rhai(&error),
            ScratchpadError::Cancelled,
            "the stop of on_progress is not caught: {error:?}"
        );
        assert_eq!(
            marks.load(Ordering::SeqCst),
            0,
            "neither the catch block nor the next statement ran after the cancelled native call"
        );
    }

    #[test]
    fn without_a_run_no_interruption() {
        let engine = built(Grants::new());
        assert_eq!(
            value(&engine, "let i = 0; while i < 1000 { i += 1; } i")
                .as_int()
                .ok(),
            Some(1000),
            "a short loop completes"
        );
    }

    #[test]
    fn a_dropped_run_scope_restores_the_previous_state() {
        let engine = built(Grants::new());
        let cancelled = CancelHandle::new();
        cancelled.cancel();
        let _outer = RunScope::enter(&CancelHandle::new(), 100);
        {
            let _inner = RunScope::enter(&cancelled, 100);
            assert!(
                engine
                    .rhai()
                    .eval::<Dynamic>("let i = 0; while i < 1000 { i += 1; } i")
                    .is_err(),
                "the inner run is cancelled"
            );
        }
        let _restored = value(&engine, "print(\"back\"); 1");
        assert_eq!(
            captured_output(),
            "back\n",
            "the outer run is back and captures again"
        );
        assert_eq!(
            value(&engine, "let i = 0; while i < 1000 { i += 1; } i")
                .as_int()
                .ok(),
            Some(1000),
            "the outer run is not cancelled"
        );
    }

    #[test]
    fn scopes_do_not_cross_threads() {
        let engine = Arc::new(built(Grants::new()));
        let cancelled = CancelHandle::new();
        cancelled.cancel();
        let _scope = RunScope::enter(&cancelled, 100);
        let own = engine
            .rhai()
            .eval::<Dynamic>("let i = 0; while i < 1000 { i += 1; } i");
        assert!(own.is_err(), "the open scope cancels this thread");
        let other = Arc::clone(&engine);
        let joined = std::thread::spawn(move || {
            other
                .rhai()
                .eval::<Dynamic>("let i = 0; while i < 1000 { i += 1; } i")
                .ok()
                .and_then(|value| value.as_int().ok())
        })
        .join();
        assert_eq!(joined.ok(), Some(Some(1000)), "another thread is not interrupted");
    }

    #[test]
    fn registration_closure_runs_once_and_is_catalogued() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let builder: EngineBuilder = ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .with_registration(move |engine| {
                let _count = counter.fetch_add(1, Ordering::SeqCst);
                let _hash = engine.register_fn("session_context", || 42i64);
                Ok(())
            });
        let engine = match builder.build() {
            Ok(engine) => engine,
            Err(error) => panic!("the engine must build: {error}"),
        };
        assert_eq!(calls.load(Ordering::SeqCst), 1, "run once");
        assert_eq!(
            value(&engine, "session_context()").as_int().ok(),
            Some(42),
            "callable"
        );
        let catalogued = engine
            .catalog()
            .functions()
            .iter()
            .any(|entry| entry.path() == "session_context" && entry.origin() == &Origin::Host);
        assert!(catalogued, "catalogued as host");
    }

    #[test]
    fn failing_registration_closure_refused() {
        let builder =
            ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new()).with_registration(|_engine| {
                Err(ScratchpadError::Engine {
                    message: "refused".to_owned(),
                })
            });
        match builder.build() {
            Ok(_) => panic!("a failing closure fails the build"),
            Err(error) => assert_eq!(error.class(), ScriptErrorClass::Engine, "an engine error"),
        }
    }

    #[test]
    fn engine_is_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScratchpadEngine>();
    }

    #[test]
    fn suggest_end_to_end_names_the_closest_granted_function() {
        let engine = built(Grants::new().with_module(grafana()).expect("first module"));
        let error = failure(&engine, "grafana::loki_queryy(\"x\")");
        let message = suggest_for(error, &candidates_of(&engine)).to_string();
        assert!(
            message.contains("grafana::loki_query"),
            "the closest candidate: {message}"
        );
    }

    #[test]
    fn suggest_end_to_end_never_names_an_ungranted_function() {
        let engine = built(Grants::new().with_module(grafana()).expect("first module"));
        let error = failure(&engine, "grafana::loki_queryy(\"x\")");
        let message = suggest_for(error, &candidates_of(&engine)).to_string();
        assert!(!message.contains("other::"), "no ungranted module: {message}");
        assert!(
            !message.contains("loki_queryx"),
            "no ungranted function: {message}"
        );
    }

    #[test]
    fn suggest_end_to_end_never_suggests_a_toolset_dependency() {
        let engine = built(
            Grants::new()
                .with_module(toolset_module())
                .expect("the toolset module"),
        );
        let error = failure(&engine, "ts::secrt()");
        let message = suggest_for(error, &candidates_of(&engine)).to_string();
        assert!(
            !message.contains("Closest candidates"),
            "no suggestion: {message}"
        );
        assert!(
            !message.contains("secret"),
            "the dependency is never named: {message}"
        );
    }

    #[test]
    fn suggest_end_to_end_never_suggests_an_ungranted_group() {
        let engine = built(Grants::new());
        let error = failure(&engine, "get_envv(\"X\")");
        let message = suggest_for(error, &candidates_of(&engine)).to_string();
        assert_eq!(
            message, "function not found at line 1, column 1: get_envv",
            "no suggestion names get_env, which is not granted"
        );
    }
}
