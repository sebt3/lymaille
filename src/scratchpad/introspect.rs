//! Introspection that the LLM calls from Rhai: variables of the scope, granted modules, functions with their signature and
//! documentation, toolsets and skills, and a lexical search `find`. It shows only what is granted: an ungranted group or
//! module never appears, and neither does a dependency of a toolset (`agent-sessions/scratchpad#FR-3`,
//! `agent-sessions/toolsets#FR-2`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};

use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map};
use serde::Deserialize;

use super::error::{ScratchpadError, ScratchpadResult};
use super::format::register;
use super::grant::{GrantedModule, Grants};
use super::limits::ScratchpadLimits;
use super::render::render_value;

/// Longest preview of a variable, in characters.
const PREVIEW_CHARS: usize = 60;
/// Most results returned by `find`.
const MAX_FIND_RESULTS: usize = 50;
/// Prefix of the documentation line that declares the expected call of a function (`agent-sessions/scratchpad#FR-14`).
const USAGE_PREFIX: &str = "Usage: ";

/// Where a function of the catalog comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// A function of a group of the `vynil-core` common core, named after the group (`pure`, `host`, `sleep`, `fs`, `shell`).
    Core(String),
    /// A formatting function of the scratchpad.
    Format,
    /// A function registered by the host or by a trusted domain.
    Host,
    /// A function of a granted module, named after the module.
    Module(String),
    /// A function the LLM defined in its own scripts.
    Script,
}

impl Origin {
    /// Kind of the origin, as `modules()` shows it: `core`, `format`, `host`, `module` or `script`.
    const fn kind(&self) -> &'static str {
        match self {
            Self::Core(_) => "core",
            Self::Format => "format",
            Self::Host => "host",
            Self::Module(_) => "module",
            Self::Script => "script",
        }
    }

    /// Label of the origin, as `functions()` shows it: `core:<group>`, `format`, `host`, `module:<name>` or `script`.
    fn label(&self) -> String {
        match self {
            Self::Core(group) => format!("core:{group}"),
            Self::Module(name) => format!("module:{name}"),
            Self::Format => "format".to_owned(),
            Self::Host => "host".to_owned(),
            Self::Script => "script".to_owned(),
        }
    }
}

/// A function of the catalog: its call path, its signature, its documentation when it has one, the usage line its
/// documentation declares when it has one, and its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionEntry {
    path: String,
    signature: String,
    arity: usize,
    doc: Option<String>,
    usage: Option<String>,
    origin: Origin,
}

impl FunctionEntry {
    /// Builds an entry from its call path, its signature, its number of parameters, its documentation and its origin.
    /// The usage line is read from the documentation at construction.
    #[must_use]
    pub(crate) fn new(
        path: String,
        signature: String,
        arity: usize,
        doc: Option<String>,
        origin: Origin,
    ) -> Self {
        let usage = doc.as_deref().and_then(usage_of).map(str::to_owned);
        Self {
            path,
            signature,
            arity,
            doc,
            usage,
            origin,
        }
    }

    /// Call path, such as `json_encode` or `grafana::loki_query`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Signature, with the parameter names and types and the return type.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// Number of parameters.
    #[must_use]
    pub const fn arity(&self) -> usize {
        self.arity
    }

    /// Documentation, when the function has one.
    #[must_use]
    pub fn doc(&self) -> Option<&str> {
        self.doc.as_deref()
    }

    /// Usage line declared in the documentation: the expected call that follows the `Usage: ` prefix of the first such
    /// line, when the function declares one. The line itself stays in the documentation that `functions()` and `find`
    /// render.
    #[must_use]
    pub fn usage(&self) -> Option<&str> {
        self.usage.as_deref()
    }

    /// Where the function comes from.
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        &self.origin
    }
}

/// Functions of the granted surface, as read from the metadata of the engine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Catalog {
    functions: Vec<FunctionEntry>,
}

impl Catalog {
    /// Catalog of the granted functions of an engine. Each global function is attributed to the step whose registration added
    /// its signature; each function of a granted module is attributed to that module. Other modules are left out, and so are
    /// the sub-modules, which are the dependencies of a toolset.
    ///
    /// # Errors
    ///
    /// Returns an engine error when the metadata of the engine cannot be read.
    pub fn build(
        engine: &Engine,
        grants: &Grants,
        steps: &[(Origin, BTreeSet<String>)],
    ) -> ScratchpadResult<Self> {
        let metadata = read_metadata(engine)?;
        let mut functions = Vec::new();
        for function in &metadata.functions {
            let attributed = steps
                .iter()
                .find(|(_, signatures)| signatures.contains(&function.signature));
            if let Some((origin, _)) = attributed {
                functions.push(entry(function, function.name.clone(), origin.clone()));
            }
        }
        for (name, module) in &metadata.modules {
            if grants.modules().iter().any(|granted| granted.name() == name) {
                functions.extend(module_entries(name, module));
            }
        }
        functions.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.signature.cmp(&right.signature))
        });
        Ok(Self { functions })
    }

    /// The functions of the catalog, sorted by call path.
    #[must_use]
    pub fn functions(&self) -> &[FunctionEntry] {
        &self.functions
    }

    /// Whether a function of this call path and this number of parameters is granted.
    #[must_use]
    pub fn contains(&self, path: &str, arity: usize) -> bool {
        self.functions
            .iter()
            .any(|function| function.path == path && function.arity == arity)
    }
}

/// Entries of the catalog for the functions of a granted module, under its name.
fn module_entries(name: &str, module: &MetadataModule) -> Vec<FunctionEntry> {
    module
        .functions
        .iter()
        .map(|function| {
            let path = format!("{name}::{}", function.name);
            entry(function, path, Origin::Module(name.to_owned()))
        })
        .collect()
}

/// Entry of the catalog for a function of the metadata.
fn entry(function: &MetadataFunction, path: String, origin: Origin) -> FunctionEntry {
    FunctionEntry::new(
        path,
        function.signature.clone(),
        function.num_params,
        doc_of(&function.doc_comments),
        origin,
    )
}

/// Documentation of a function: its `///` comments without the marker, one line each. Absent when there is none.
fn doc_of(comments: &[String]) -> Option<String> {
    let lines: Vec<&str> = comments
        .iter()
        .map(|comment| comment.trim_start_matches('/').trim())
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Usage line of a documentation: the text after the prefix of the first line that starts with it, without trailing
/// whitespace. Nothing when no line starts with the prefix, or when the text is empty; one line per function, the first
/// one wins.
fn usage_of(doc: &str) -> Option<&str> {
    doc.lines()
        .find_map(|line| line.strip_prefix(USAGE_PREFIX))
        .map(str::trim_end)
        .filter(|text| !text.is_empty())
}

/// A function of the metadata of an engine, as Rhai describes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataFunction {
    name: String,
    signature: String,
    #[serde(default)]
    num_params: usize,
    #[serde(default)]
    doc_comments: Vec<String>,
}

/// A static module of the metadata of an engine: its own functions only, its sub-modules being dependencies.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataModule {
    #[serde(default)]
    functions: Vec<MetadataFunction>,
}

/// The metadata of an engine: the global functions and the static modules.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(default)]
    functions: Vec<MetadataFunction>,
    #[serde(default)]
    modules: BTreeMap<String, MetadataModule>,
}

/// Metadata of the functions of an engine, without the standard packages of Rhai.
fn read_metadata(engine: &Engine) -> ScratchpadResult<Metadata> {
    let json = engine
        .gen_fn_metadata_to_json(false)
        .map_err(|error| ScratchpadError::Engine {
            message: format!("metadata of the functions: {error}"),
        })?;
    serde_json::from_str(&json).map_err(|error| ScratchpadError::Internal {
        message: format!("metadata of the functions: {error}"),
    })
}

/// Signatures of the global functions of an engine.
fn global_signatures(engine: &Engine) -> ScratchpadResult<BTreeSet<String>> {
    Ok(read_metadata(engine)?
        .functions
        .into_iter()
        .map(|function| function.signature)
        .collect())
}

/// Runs one registration on an engine and returns the origin with the signatures it added, which the catalog attributes to
/// that origin.
pub(in crate::scratchpad) fn registration_step(
    engine: &mut Engine,
    origin: Origin,
    install: impl FnOnce(&mut Engine) -> ScratchpadResult<()>,
) -> ScratchpadResult<(Origin, BTreeSet<String>)> {
    let before = global_signatures(engine)?;
    install(engine)?;
    let after = global_signatures(engine)?;
    let added: BTreeSet<String> = after.difference(&before).cloned().collect();
    Ok((origin, added))
}

/// A variable of the scope as introspection shows it: its name, the Rhai name of its type and a preview of its value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VariableInfo {
    name: String,
    type_name: String,
    preview: String,
}

impl VariableInfo {
    /// Describes the variable `name` holding `value`: the Rhai name of its type and a preview of at most 60 characters.
    #[must_use]
    pub fn new(name: &str, value: &Dynamic) -> Self {
        Self {
            name: name.to_owned(),
            type_name: value.type_name().to_owned(),
            preview: render_value(value, PREVIEW_CHARS).text().to_owned(),
        }
    }
}

/// A function the LLM defined in its scripts: its name, its parameters and its documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScriptFunction {
    name: String,
    params: Vec<String>,
    doc: Option<String>,
}

impl ScriptFunction {
    /// Describes a public function: its name, its parameter names and its documentation, when it has one.
    #[must_use]
    pub const fn new(name: String, params: Vec<String>, doc: Option<String>) -> Self {
        Self { name, params, doc }
    }

    /// Signature, as `name(a, b)`.
    fn signature(&self) -> String {
        format!("{}({})", self.name, self.params.join(", "))
    }
}

/// State shared between the engine and its runs: the grants, the catalog and the limits it reads, and the variables and
/// script functions set by `update` before each run.
#[derive(Debug)]
#[non_exhaustive]
pub struct IntrospectionSource {
    grants: Grants,
    catalog: Catalog,
    limits: ScratchpadLimits,
    state: RwLock<ScopeState>,
}

/// Variables and script functions of the last update.
#[derive(Debug, Clone, Default)]
struct ScopeState {
    variables: Vec<VariableInfo>,
    script_functions: Vec<ScriptFunction>,
}

/// A function as introspection lists it, whatever its origin.
#[derive(Debug, Clone)]
struct View {
    path: String,
    signature: String,
    doc: Option<String>,
    origin: Origin,
}

/// A result of `find`.
#[derive(Debug, Clone)]
struct Found {
    kind: &'static str,
    name: String,
    path: String,
    text: String,
    haystack: String,
}

impl IntrospectionSource {
    /// Shared state for an engine with these grants, this catalog and these limits.
    #[must_use]
    pub const fn new(grants: Grants, catalog: Catalog, limits: ScratchpadLimits) -> Self {
        Self {
            grants,
            catalog,
            limits,
            state: RwLock::new(ScopeState {
                variables: Vec::new(),
                script_functions: Vec::new(),
            }),
        }
    }

    /// Replaces the variables and the script functions that introspection shows. Called before each run.
    pub fn update(&self, variables: Vec<VariableInfo>, script_functions: Vec<ScriptFunction>) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.variables = variables;
        state.script_functions = script_functions;
    }

    /// A copy of the state; a poisoned lock is read anyway, its content being plain data.
    fn snapshot(&self) -> ScopeState {
        self.state.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Every function visible to the script: the catalog, then the script functions.
    fn views(state: &ScopeState, catalog: &Catalog) -> Vec<View> {
        let granted = catalog.functions().iter().map(|entry| View {
            path: entry.path.clone(),
            signature: entry.signature.clone(),
            doc: entry.doc.clone(),
            origin: entry.origin.clone(),
        });
        let scripted = state.script_functions.iter().map(|function| View {
            path: function.name.clone(),
            signature: function.signature(),
            doc: function.doc.clone(),
            origin: Origin::Script,
        });
        granted.chain(scripted).collect()
    }

    /// `vars()`: the variables of the scope at the start of the run.
    fn vars(&self) -> Array {
        self.snapshot()
            .variables
            .iter()
            .map(|variable| {
                record(vec![
                    ("name", text(&variable.name)),
                    ("type", text(&variable.type_name)),
                    ("preview", text(&variable.preview)),
                ])
            })
            .collect()
    }

    /// `modules()`: the granted modules, with the functions of each, and the groups of functions of the catalog.
    fn modules(&self) -> Array {
        let mut counts: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
        for module in self.grants.modules() {
            let _count = counts.entry(("module", module.name().to_owned())).or_default();
        }
        for view in Self::views(&self.snapshot(), &self.catalog) {
            let name = match &view.origin {
                Origin::Core(group) => group.clone(),
                Origin::Module(module) => module.clone(),
                Origin::Format => "format".to_owned(),
                Origin::Host => "host".to_owned(),
                Origin::Script => continue,
            };
            let count = counts.entry((view.origin.kind(), name)).or_default();
            *count = count.saturating_add(1);
        }
        counts
            .into_iter()
            .map(|((kind, name), count)| {
                record(vec![
                    ("name", text(&name)),
                    ("kind", text(kind)),
                    (
                        "functions",
                        Dynamic::from(i64::try_from(count).unwrap_or(i64::MAX)),
                    ),
                ])
            })
            .collect()
    }

    /// `functions()`: every function the script can call.
    fn functions_all(&self) -> Array {
        function_rows(&Self::views(&self.snapshot(), &self.catalog))
    }

    /// `functions(origin)`: the functions of `global`, `script` or of a granted module.
    fn functions_of(&self, origin: &str) -> Result<Array, Box<EvalAltResult>> {
        let views = Self::views(&self.snapshot(), &self.catalog);
        let selected: Vec<View> = match origin {
            "global" => views
                .into_iter()
                .filter(|view| matches!(view.origin, Origin::Core(_) | Origin::Format | Origin::Host))
                .collect(),
            "script" => views
                .into_iter()
                .filter(|view| view.origin == Origin::Script)
                .collect(),
            name if self.is_granted_module(name) => views
                .into_iter()
                .filter(|view| view.origin == Origin::Module(name.to_owned()))
                .collect(),
            name => return Err(Box::new(self.unknown_origin(name))),
        };
        Ok(function_rows(&selected))
    }

    /// Whether a module of this name is granted.
    fn is_granted_module(&self, name: &str) -> bool {
        self.grants.modules().iter().any(|module| module.name() == name)
    }

    /// Error of `functions(origin)` for an origin that is not known: it lists what can be asked for.
    fn unknown_origin(&self, name: &str) -> EvalAltResult {
        let modules: Vec<&str> = self.grants.modules().iter().map(GrantedModule::name).collect();
        EvalAltResult::ErrorRuntime(
            Dynamic::from(format!(
                "functions: unknown origin `{name}`; use \"global\", \"script\" or a granted module: {}",
                modules.join(", ")
            )),
            rhai::Position::NONE,
        )
    }

    /// `toolsets()`: the granted toolsets, with their description.
    fn toolsets(&self) -> Array {
        self.grants
            .toolsets()
            .iter()
            .map(|toolset| {
                record(vec![
                    ("name", text(toolset.name())),
                    ("description", text(toolset.description())),
                ])
            })
            .collect()
    }

    /// `skills()`: the granted skills, with their description and the toolset they are attached to, or unit.
    fn skills(&self) -> Array {
        self.grants
            .skills()
            .iter()
            .map(|skill| {
                record(vec![
                    ("name", text(skill.name())),
                    ("description", text(skill.description())),
                    ("toolset", skill.toolset().map_or(Dynamic::UNIT, text)),
                ])
            })
            .collect()
    }

    /// `find(query)`: the granted elements whose path, signature, documentation or description holds every word of the
    /// query, ignoring case; the exact names first, then the names containing the first word, then the rest.
    fn find(&self, query: &str) -> Result<Array, Box<EvalAltResult>> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let Some(first) = words.first().cloned() else {
            return Err(Box::<EvalAltResult>::from("find: the query is empty"));
        };
        let whole = query.trim().to_lowercase();
        let mut found: Vec<(usize, Found)> = self
            .candidates()
            .into_iter()
            .filter(|candidate| {
                words
                    .iter()
                    .all(|word| candidate.haystack.contains(word.as_str()))
            })
            .map(|candidate| {
                let name = candidate.name.to_lowercase();
                let path = candidate.path.to_lowercase();
                let rank = if name == whole || path == whole {
                    0
                } else if name.contains(first.as_str()) {
                    1
                } else {
                    2
                };
                (rank, candidate)
            })
            .collect();
        found.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.path.cmp(&right.1.path)));
        Ok(found
            .into_iter()
            .take(MAX_FIND_RESULTS)
            .map(|(_, candidate)| {
                record(vec![
                    ("kind", text(candidate.kind)),
                    ("name", text(&candidate.name)),
                    ("path", text(&candidate.path)),
                    ("text", text(&candidate.text)),
                ])
            })
            .collect())
    }

    /// The elements `find` searches: functions, granted modules, toolsets and skills, never a variable.
    fn candidates(&self) -> Vec<Found> {
        let functions = Self::views(&self.snapshot(), &self.catalog)
            .into_iter()
            .map(|view| {
                let name = view.path.rsplit("::").next().unwrap_or(&view.path).to_owned();
                let documentation = view.doc.clone().unwrap_or_default();
                let text = match &view.doc {
                    Some(doc) => format!("{}\n{doc}", view.signature),
                    None => view.signature.clone(),
                };
                Found {
                    kind: "function",
                    name,
                    haystack: format!("{} {} {documentation}", view.path, view.signature).to_lowercase(),
                    path: view.path,
                    text,
                }
            });
        let modules = self.grants.modules().iter().map(|module| Found {
            kind: "module",
            name: module.name().to_owned(),
            path: module.name().to_owned(),
            text: format!("granted module {}", module.name()),
            haystack: module.name().to_lowercase(),
        });
        let toolsets = self.grants.toolsets().iter().map(|toolset| Found {
            kind: "toolset",
            name: toolset.name().to_owned(),
            path: toolset.name().to_owned(),
            text: toolset.description().to_owned(),
            haystack: format!("{} {}", toolset.name(), toolset.description()).to_lowercase(),
        });
        let skills = self.grants.skills().iter().map(|skill| Found {
            kind: "skill",
            name: skill.name().to_owned(),
            path: skill.name().to_owned(),
            text: skill.description().to_owned(),
            haystack: format!("{} {}", skill.name(), skill.description()).to_lowercase(),
        });
        functions.chain(modules).chain(toolsets).chain(skills).collect()
    }
}

/// Rows of `functions()` for a list of views.
fn function_rows(views: &[View]) -> Array {
    views
        .iter()
        .map(|view| {
            record(vec![
                ("path", text(&view.path)),
                ("signature", text(&view.signature)),
                ("doc", view.doc.as_deref().map_or(Dynamic::UNIT, text)),
                ("origin", text(&view.origin.label())),
            ])
        })
        .collect()
}

/// A Rhai map of the given entries.
fn record(entries: Vec<(&str, Dynamic)>) -> Dynamic {
    let mut map = Map::new();
    for (key, value) in entries {
        let _previous = map.insert(key.into(), value);
    }
    Dynamic::from_map(map)
}

/// A Rhai string of a text.
fn text(value: &str) -> Dynamic {
    Dynamic::from(value.to_owned())
}

/// Text of the limits of the sandbox that the LLM reads through `rules()` and through the skill of the toolsets.
///
/// It names what a `fn` cannot see, what a restore loses, what does not exist, the output cap and the memory limits
/// (written readably: `16 MiB`, `1 000 000`), and it states that nothing limits the duration of a script, that the model
/// bounds its own loops and cannot interrupt one.
#[must_use]
pub fn limits_notice(limits: &ScratchpadLimits) -> String {
    format!(
        "Sandbox rules: a function defined with `fn` cannot see the variables of the scope; pass them as arguments. \
         Its toolset context is read with `session_context()`. A closure or a function pointer works within the session \
         but is lost when the session is restored: define a `fn` for anything you must keep. `const` does not exist; use \
         `let`. No operation limit and no duration limit apply: bound your own loops: you cannot interrupt a script, \
         only the cancellation of the host stops one, and you cannot trigger it. The output is capped at {} characters: \
         the middle of a longer result is replaced by a notice, so store the result in a variable and extract the useful \
         part with the `fmt_*` functions. Memory: strings up to {}, arrays up to {} elements, maps up to {} entries.",
        limits.render_cap_chars(),
        readable_bytes(limits.max_string_bytes()),
        grouped(limits.max_array_len()),
        grouped(limits.max_map_len()),
    )
}

/// A count with a space between thousands: `1 000 000`.
fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut reversed = String::with_capacity(digits.len().saturating_mul(2));
    for (position, digit) in digits.chars().rev().enumerate() {
        if position > 0 && position % 3 == 0 {
            reversed.push(' ');
        }
        reversed.push(digit);
    }
    reversed.chars().rev().collect()
}

/// A size in bytes as a round number of `MiB` or `KiB` when it is one, else in grouped bytes.
fn readable_bytes(bytes: usize) -> String {
    [(1024 * 1024, "MiB"), (1024, "KiB")]
        .into_iter()
        .find_map(
            |(size, unit)| match (bytes.checked_div(size), bytes.checked_rem(size)) {
                (Some(count), Some(0)) if count > 0 => Some(format!("{} {unit}", grouped(count))),
                _ => None,
            },
        )
        .unwrap_or_else(|| format!("{} bytes", grouped(bytes)))
}

/// Registers the introspection functions on an engine. Each is documented with its parameter names; they read the shared
/// state and nothing else: no I/O, no clock.
pub(in crate::scratchpad) fn register_introspection(engine: &mut Engine, source: &Arc<IntrospectionSource>) {
    let vars = Arc::clone(source);
    register(
        engine,
        "vars",
        &[],
        "/// Variables of the scope at the start of this run: name, type and a preview of the value.",
        move || vars.vars(),
    );
    let modules = Arc::clone(source);
    register(
        engine,
        "modules",
        &[],
        "/// Granted modules and groups of functions: name, kind (core, format, host or module) and number of functions.",
        move || modules.modules(),
    );
    let all = Arc::clone(source);
    register(
        engine,
        "functions",
        &[],
        "/// Every function the script can call: path, signature, documentation and origin.",
        move || all.functions_all(),
    );
    let one = Arc::clone(source);
    register(
        engine,
        "functions",
        &["origin: string", "array"],
        "/// Functions of an origin: \"global\" for the built-in groups, \"script\" for the functions the LLM defined, or the name of a granted module. Fails on any other name.",
        move |origin: ImmutableString| one.functions_of(origin.as_str()),
    );
    let toolsets = Arc::clone(source);
    register(
        engine,
        "toolsets",
        &[],
        "/// Granted toolsets: name and description.",
        move || toolsets.toolsets(),
    );
    let skills = Arc::clone(source);
    register(
        engine,
        "skills",
        &[],
        "/// Granted skills: name, description and the toolset they are attached to, or unit.",
        move || skills.skills(),
    );
    let search = Arc::clone(source);
    register(
        engine,
        "find",
        &["query: string", "array"],
        "/// Search of the granted functions, modules, toolsets and skills: every word of the query must appear in the path, the signature, the documentation or the description. Fails on an empty query.",
        move |query: ImmutableString| search.find(query.as_str()),
    );
    let rules = Arc::clone(source);
    register(
        engine,
        "rules",
        &[],
        "/// Limits of the sandbox: what functions cannot see, what is not kept, what does not exist, the output cap and the memory limits.",
        move || limits_notice(&rules.limits),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        Catalog, FunctionEntry, IntrospectionSource, Origin, ScriptFunction, VariableInfo, limits_notice,
        register_introspection, registration_step, usage_of,
    };
    use crate::scratchpad::{
        GrantedModule, GrantedSkill, GrantedToolset, Grants, ScratchpadLimits, register_format_functions,
    };
    use rhai::{Array, Dynamic, Engine, FuncRegistration, Map, Module, Shared};
    use std::sync::Arc;

    /// Module `grafana` with `loki_query(query, range)` documented, and a dependency `dep` that is not granted.
    fn grafana_module() -> Module {
        let mut module = Module::new();
        let _hash = FuncRegistration::new("loki_query")
            .with_comments(["/// Runs a Loki query"])
            .with_params_info(["query: string", "range: string", "map"])
            .set_into_module(
                &mut module,
                |query: rhai::ImmutableString, range: rhai::ImmutableString| format!("{query}{range}"),
            );
        let mut dependency = Module::new();
        let _dependency_hash =
            FuncRegistration::new("secret").set_into_module(&mut dependency, || "token".to_owned());
        let _module = module.set_sub_module("dep", Shared::new(dependency));
        module
    }

    /// An engine with the format functions, the `grafana` and `k8s` modules granted, a toolset, two skills, and the
    /// introspection registered.
    fn fixture() -> (Engine, Arc<IntrospectionSource>) {
        let mut engine = Engine::new();
        let format = registration_step(&mut engine, Origin::Format, |engine| {
            register_format_functions(engine);
            Ok(())
        });
        let steps = vec![format.expect("the format functions register")];
        let grafana = Shared::new(grafana_module());
        let _grafana = engine.register_static_module("grafana", Shared::clone(&grafana));
        let _k8s = engine.register_static_module("k8s", Shared::new(Module::new()));
        let mut ungranted = Module::new();
        let _ungranted_hash =
            FuncRegistration::new("loki_secret").set_into_module(&mut ungranted, || "value".to_owned());
        let _other = engine.register_static_module("other", Shared::new(ungranted));
        let grants = Grants::new()
            .with_module(GrantedModule::new("grafana", grafana).expect("valid name"))
            .and_then(|grants| {
                grants.with_module(GrantedModule::new("k8s", Shared::new(Module::new())).expect("valid name"))
            })
            .and_then(|grants| {
                grants.with_toolset(GrantedToolset::new("grafana", "Grafana queries").expect("valid"))
            })
            .and_then(|grants| {
                grants.with_skill(
                    GrantedSkill::new("loki-debug", "Debugs Loki queries", Some("grafana")).expect("valid"),
                )
            })
            .and_then(|grants| {
                grants.with_skill(GrantedSkill::new("plain", "A plain skill", None).expect("valid"))
            })
            .expect("distinct names");
        let catalog = Catalog::build(&engine, &grants, &steps).expect("the catalog builds");
        let source = Arc::new(IntrospectionSource::new(grants, catalog, ScratchpadLimits::new()));
        register_introspection(&mut engine, &source);
        (engine, source)
    }

    /// Rows of an array returned by a script, each a map.
    fn rows(engine: &Engine, script: &str) -> Vec<Map> {
        match engine.eval::<Array>(script) {
            Ok(array) => array.into_iter().filter_map(Dynamic::try_cast::<Map>).collect(),
            Err(error) => panic!("`{script}` must succeed: {error}"),
        }
    }

    /// Text of a key of a row, empty when absent.
    fn field(row: &Map, key: &str) -> String {
        row.get(key).map(ToString::to_string).unwrap_or_default()
    }

    /// The field `key` of every row of a script result.
    fn column(engine: &Engine, script: &str, key: &str) -> Vec<String> {
        rows(engine, script).iter().map(|row| field(row, key)).collect()
    }

    #[test]
    fn catalog_attributes_each_function_and_leaves_out_the_dependencies() {
        let (_engine, source) = fixture();
        let catalog = &source.catalog;
        let paths: Vec<&str> = catalog.functions().iter().map(FunctionEntry::path).collect();
        assert!(
            paths.contains(&"fmt_json"),
            "the format functions are listed: {paths:?}"
        );
        assert!(
            paths.contains(&"grafana::loki_query"),
            "the granted module is listed: {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.contains("secret")),
            "the dependency is hidden: {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with("other")),
            "nothing ungranted: {paths:?}"
        );
        let loki = catalog
            .functions()
            .iter()
            .find(|entry| entry.path() == "grafana::loki_query")
            .expect("the entry exists");
        assert_eq!(
            loki.origin(),
            &Origin::Module("grafana".to_owned()),
            "the module origin"
        );
        assert_eq!(
            loki.doc(),
            Some("Runs a Loki query"),
            "the documentation without the marker"
        );
        let format = catalog
            .functions()
            .iter()
            .find(|entry| entry.path() == "fmt_json")
            .expect("entry");
        assert_eq!(format.origin(), &Origin::Format, "the format origin");
    }

    #[test]
    fn contains_checks_the_arity() {
        let (_engine, source) = fixture();
        assert!(
            source.catalog.contains("grafana::loki_query", 2),
            "the arity of the loki function"
        );
        assert!(
            !source.catalog.contains("grafana::loki_query", 1),
            "another arity is not granted"
        );
    }

    #[test]
    fn functions_carry_signature_and_documentation() {
        let (engine, _source) = fixture();
        let found = rows(
            &engine,
            "functions(\"global\").filter(|f| f.path == \"fmt_json\")",
        );
        let first = found.first().expect("fmt_json is listed");
        assert!(
            field(first, "signature").starts_with("fmt_json("),
            "signature: {}",
            field(first, "signature")
        );
        assert!(!field(first, "doc").is_empty(), "a documentation is present");
    }

    #[test]
    fn script_functions_are_listed_with_their_documentation() {
        let (engine, source) = fixture();
        source.update(
            Vec::new(),
            vec![ScriptFunction::new(
                "f".to_owned(),
                vec!["a".to_owned()],
                Some("Doubles a.".to_owned()),
            )],
        );
        assert_eq!(
            column(&engine, "functions(\"script\")", "path"),
            vec!["f".to_owned()],
            "f is listed"
        );
        assert_eq!(
            column(&engine, "functions(\"script\")", "doc"),
            vec!["Doubles a.".to_owned()],
            "its doc"
        );
    }

    #[test]
    fn variables_are_listed_with_type_and_preview() {
        let (engine, source) = fixture();
        source.update(
            vec![
                VariableInfo::new("n", &Dynamic::from(3i64)),
                VariableInfo::new("s", &Dynamic::from("hello")),
            ],
            Vec::new(),
        );
        assert_eq!(
            column(&engine, "vars()", "type"),
            vec!["i64".to_owned(), "string".to_owned()],
            "types"
        );
        assert_eq!(
            column(&engine, "vars()", "preview"),
            vec!["3".to_owned(), "hello".to_owned()],
            "previews"
        );
    }

    #[test]
    fn modules_and_unknown_origin() {
        let (engine, _source) = fixture();
        let modules = rows(&engine, "modules()");
        let grafana = modules
            .iter()
            .find(|row| field(row, "name") == "grafana")
            .expect("grafana listed");
        assert_eq!(field(grafana, "kind"), "module", "its kind");
        assert_eq!(
            field(grafana, "functions"),
            "1",
            "one function, the dependency is not counted"
        );
        assert!(
            modules.iter().any(|row| field(row, "name") == "k8s"),
            "k8s listed"
        );
        let failure = engine.eval::<Array>("functions(\"nope\")");
        let message = match failure {
            Ok(value) => panic!("an unknown origin must fail, it returned {value:?}"),
            Err(error) => error.to_string(),
        };
        assert!(
            message.contains("grafana") && message.contains("k8s"),
            "lists the modules: {message}"
        );
    }

    #[test]
    fn toolsets_and_skills() {
        let (engine, _source) = fixture();
        assert_eq!(
            column(&engine, "toolsets()", "name"),
            vec!["grafana".to_owned()],
            "the toolset"
        );
        let skills = rows(&engine, "skills()");
        let attached = skills
            .iter()
            .find(|row| field(row, "name") == "loki-debug")
            .expect("attached skill");
        assert_eq!(field(attached, "toolset"), "grafana", "its toolset");
        let plain = skills
            .iter()
            .find(|row| field(row, "name") == "plain")
            .expect("plain skill");
        assert!(
            plain.get("toolset").is_some_and(Dynamic::is_unit),
            "no toolset is unit"
        );
    }

    #[test]
    fn search_returns_granted_functions_and_skills_only() {
        let (engine, _source) = fixture();
        let names = column(&engine, "find(\"loki\")", "path");
        assert!(
            names.contains(&"grafana::loki_query".to_owned()),
            "the function: {names:?}"
        );
        assert!(names.contains(&"loki-debug".to_owned()), "the skill: {names:?}");
        assert!(
            !names.iter().any(|name| name.contains("secret")),
            "the dependency: {names:?}"
        );
    }

    #[test]
    fn search_is_case_insensitive_and_multi_word() {
        let (engine, _source) = fixture();
        let names = column(&engine, "find(\"LOKI query\")", "path");
        assert!(
            names.contains(&"grafana::loki_query".to_owned()),
            "both words: {names:?}"
        );
        assert!(
            !names.contains(&"loki-debug".to_owned()),
            "the skill lacks the word query: {names:?}"
        );
    }

    #[test]
    fn search_ranks_exact_names_first() {
        let (engine, source) = fixture();
        source.update(
            Vec::new(),
            vec![
                ScriptFunction::new("loki_query".to_owned(), Vec::new(), None),
                ScriptFunction::new("query".to_owned(), Vec::new(), None),
                ScriptFunction::new(
                    "doc_only".to_owned(),
                    Vec::new(),
                    Some("Runs a query.".to_owned()),
                ),
            ],
        );
        let names = column(&engine, "find(\"query\")", "path");
        assert_eq!(
            names.first().map(String::as_str),
            Some("query"),
            "the exact name first: {names:?}"
        );
        let containing = names.iter().position(|name| name == "loki_query");
        let documented = names.iter().position(|name| name == "doc_only");
        assert!(
            containing.is_some_and(|at| documented.is_some_and(|doc| at < doc)),
            "the name containing the word before the documentation match: {names:?}"
        );
    }

    #[test]
    fn search_is_bounded_and_rejects_an_empty_query() {
        let (engine, source) = fixture();
        let many: Vec<ScriptFunction> = (0..60)
            .map(|index| ScriptFunction::new(format!("a{index}"), Vec::new(), None))
            .collect();
        source.update(Vec::new(), many);
        assert_eq!(rows(&engine, "find(\"a\")").len(), 50, "at most fifty results");
        assert!(
            engine.eval::<Array>("find(\"  \")").is_err(),
            "an empty query is a script error"
        );
    }

    #[test]
    fn rules_mention_the_sandbox_limits() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(8000);
        let text = limits_notice(&limits);
        assert!(
            text.contains("cannot see the variables"),
            "fn does not see the scope: {text}"
        );
        assert!(text.contains("`const` does not exist"), "const: {text}");
        assert!(
            text.contains("No operation limit and no duration limit apply"),
            "no operation limit and no duration limit: {text}"
        );
        assert!(
            text.contains("bound your own loops: you cannot interrupt a script"),
            "the model bounds its own loops and cannot interrupt a script: {text}"
        );
        assert!(
            text.contains("only the cancellation of the host stops one, and you cannot trigger it"),
            "only the host's cancellation stops a script, and the model cannot trigger it: {text}"
        );
        assert!(
            !text.contains("runaway") && !text.contains("closure or a function pointer is not kept"),
            "the old phrases are gone: {text}"
        );
        assert!(
            text.contains("A closure or a function pointer works within the session but is lost when the session is restored"),
            "closures work in the session and are lost at restore: {text}"
        );
        assert!(
            text.contains("capped at 8000 characters"),
            "the output cap: {text}"
        );
    }

    // Scenario: les règles énoncent chaque plafond de mémoire à sa place
    #[test]
    fn rules_state_each_memory_limit_in_its_place() {
        let limits = ScratchpadLimits::new()
            .with_max_array_len(7)
            .and_then(|limits| limits.with_max_map_len(9))
            .expect("the limits are not zero");
        let text = limits_notice(&limits);
        assert!(
            text.contains("strings up to 16 MiB, arrays up to 7 elements, maps up to 9 entries"),
            "the memory limits, in order: {text}"
        );
    }

    // Scenario: les règles énoncent chaque plafond de mémoire à sa place (valeurs par défaut lisibles)
    #[test]
    fn rules_write_the_default_memory_limits_readably() {
        let text = limits_notice(&ScratchpadLimits::new());
        assert!(
            text.contains(
                "strings up to 16 MiB, arrays up to 1 000 000 elements, maps up to 100 000 entries"
            ),
            "the default limits are readable: {text}"
        );
    }

    #[test]
    fn rules_write_a_byte_limit_that_is_not_a_round_size_in_bytes() {
        let kib = ScratchpadLimits::new()
            .with_max_string_bytes(3 * 1024)
            .expect("not zero");
        assert!(
            limits_notice(&kib).contains("strings up to 3 KiB,"),
            "a round KiB size"
        );
        let odd = ScratchpadLimits::new()
            .with_max_string_bytes(1500)
            .expect("not zero");
        assert!(
            limits_notice(&odd).contains("strings up to 1 500 bytes,"),
            "any other size in bytes"
        );
        let small = ScratchpadLimits::new().with_max_array_len(999).expect("not zero");
        assert!(
            limits_notice(&small).contains("arrays up to 999 elements"),
            "no separator below a thousand"
        );
    }

    // Scenario: la documentation garde une ligne par commentaire
    #[test]
    fn documentation_keeps_one_line_per_comment() {
        let mut engine = Engine::new();
        let mut module = Module::new();
        let _hash = FuncRegistration::new("two_lines")
            .with_comments(["/// First line.", "/// Second line."])
            .set_into_module(&mut module, || 0i64);
        let _registered = engine.register_static_module("docs", Shared::new(module));
        let grants = Grants::new()
            .with_module(GrantedModule::new("docs", Shared::new(Module::new())).expect("valid name"))
            .expect("the module is granted");
        let catalog = Catalog::build(&engine, &grants, &[]).expect("the catalog builds");
        let entry = catalog
            .functions()
            .iter()
            .find(|entry| entry.path() == "docs::two_lines");
        assert_eq!(
            entry.and_then(FunctionEntry::doc),
            Some("First line.\nSecond line."),
            "one line per comment, joined by a newline"
        );
    }

    // Scenario: une ligne d'usage est lue dans la documentation
    #[test]
    fn usage_line_is_read_from_the_documentation() {
        let mut engine = Engine::new();
        let mut module = Module::new();
        let _hash = FuncRegistration::new("loki_query")
            .with_comments([
                "/// Runs a Loki query.",
                "/// Usage: grafana::loki_query(#{ query: \"\u{2026}\" })",
                "/// Returns the matching log lines.",
            ])
            .set_into_module(&mut module, || 0i64);
        let _registered = engine.register_static_module("grafana", Shared::new(module));
        let grants = Grants::new()
            .with_module(GrantedModule::new("grafana", Shared::new(Module::new())).expect("valid name"))
            .expect("the module is granted");
        let catalog = Catalog::build(&engine, &grants, &[]).expect("the catalog builds");
        let entry = catalog
            .functions()
            .iter()
            .find(|entry| entry.path() == "grafana::loki_query")
            .expect("the entry exists");
        assert_eq!(
            entry.usage(),
            Some("grafana::loki_query(#{ query: \"\u{2026}\" })"),
            "the text after the prefix of the second line"
        );
        let source = Arc::new(IntrospectionSource::new(grants, catalog, ScratchpadLimits::new()));
        register_introspection(&mut engine, &source);
        let found = rows(&engine, "functions(\"grafana\")");
        let row = found.first().expect("the function is listed");
        assert_eq!(
            field(row, "doc"),
            "Runs a Loki query.\nUsage: grafana::loki_query(#{ query: \"\u{2026}\" })\nReturns the matching log lines.",
            "functions() still renders the three lines, the usage line included"
        );
    }

    // Scenario: sans ligne d'usage, rien
    #[test]
    fn usage_is_absent_without_a_usage_line() {
        let mut engine = Engine::new();
        let mut module = Module::new();
        let _plain = FuncRegistration::new("plain")
            .with_comments(["/// Runs a Loki query."])
            .set_into_module(&mut module, || 0i64);
        let _bare = FuncRegistration::new("bare")
            .with_comments(["/// Usage: "])
            .set_into_module(&mut module, || 1i64);
        let _registered = engine.register_static_module("usage_test", Shared::new(module));
        let grants = Grants::new()
            .with_module(GrantedModule::new("usage_test", Shared::new(Module::new())).expect("valid name"))
            .expect("the module is granted");
        let catalog = Catalog::build(&engine, &grants, &[]).expect("the catalog builds");
        for path in ["usage_test::plain", "usage_test::bare"] {
            let entry = catalog.functions().iter().find(|entry| entry.path() == path);
            assert_eq!(
                entry.and_then(FunctionEntry::usage),
                None,
                "`{path}` declares no usage line"
            );
        }
    }

    // Scenario: la première ligne d'usage gagne
    #[test]
    fn the_first_usage_line_wins() {
        let mut engine = Engine::new();
        let mut module = Module::new();
        let _hash = FuncRegistration::new("twice")
            .with_comments(["/// Usage: first(a)", "/// Middle.", "/// Usage: second(b)"])
            .set_into_module(&mut module, || 0i64);
        let _registered = engine.register_static_module("usage_test", Shared::new(module));
        let grants = Grants::new()
            .with_module(GrantedModule::new("usage_test", Shared::new(Module::new())).expect("valid name"))
            .expect("the module is granted");
        let catalog = Catalog::build(&engine, &grants, &[]).expect("the catalog builds");
        let entry = catalog
            .functions()
            .iter()
            .find(|entry| entry.path() == "usage_test::twice")
            .expect("the entry exists");
        assert_eq!(entry.usage(), Some("first(a)"), "the text of the first line");
        assert_eq!(
            entry.doc(),
            Some("Usage: first(a)\nMiddle.\nUsage: second(b)"),
            "both lines stay in the documentation"
        );
    }

    // La branche « texte vide » du Must : `doc_of` tronque chaque ligne, la branche est injoignable par le pipeline,
    // elle se vérifie sur `usage_of` directement.
    #[test]
    fn an_empty_usage_line_gives_nothing() {
        assert_eq!(usage_of("Usage: "), None, "a line that is only the prefix");
        assert_eq!(
            usage_of("First.\nUsage: call(a)  \nLast."),
            Some("call(a)"),
            "trailing whitespace dropped"
        );
        assert_eq!(usage_of("use: call(a)"), None, "the prefix is case-sensitive");
        assert_eq!(usage_of(" Usage: call(a)"), None, "no leading whitespace");
    }

    // Scenario: find cherche dans la description d'un toolset
    #[test]
    fn search_reads_the_description_of_a_toolset() {
        let (engine, _source) = fixture();
        let found = rows(&engine, "find(\"queries\")");
        assert!(
            found
                .iter()
                .any(|row| field(row, "kind") == "toolset" && field(row, "name") == "grafana"),
            "the toolset is found by its description \"Grafana queries\""
        );
    }

    #[test]
    fn introspection_reads_a_poisoned_lock() {
        let (engine, source) = fixture();
        let poisoner = Arc::clone(&source);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.state.write();
            panic!("poisoning the state");
        })
        .join();
        assert!(joined.is_err(), "the thread panicked");
        assert!(engine.eval::<Array>("vars()").is_ok(), "vars still answers");
        assert!(
            engine.eval::<Array>("functions()").is_ok(),
            "functions still answer"
        );
    }
}
