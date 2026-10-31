//! Module `toolset_builder` of the configuration agent: it writes drafts of toolsets, skills and context types, validates,
//! lists and deletes them, and never activates anything (`agent-sessions/toolsets#FR-8`, `agent-sessions#BR-8`,
//! `agent-sessions#DEC-13`). The functions are synchronous Rhai functions that call the asynchronous stores of the host
//! through `vynil_core::rt::block_on`. The module writes only through [`ToolsetStore::put_draft`] and [`DraftStore`].

use std::sync::Arc;

use rhai::{
    Array, Dynamic, Engine as RhaiEngine, EvalAltResult, ImmutableString, Map, Module, Position, Shared,
};
use serde_json::Value;
use vynil_core::Semver;
use vynil_core::rt::block_on;

use super::toolset_context::value_looks_like_secret;
use super::{
    ContextType, ParamDecl, RoleDecl, RoleSpec, Skill, Toolset, ToolsetStore, ToolsetsError, ToolsetsResult,
    TypeRequirement, check_template, extract_contract,
};
use crate::scratchpad::ScratchpadResult;
use crate::secret::SecretDetector;
use crate::session::Redactor;

/// The kind of object a draft holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DraftKind {
    /// A toolset, with the skills attached to it.
    Toolset,
    /// A context type.
    ContextType,
}

/// A draft as a listing shows it: its kind and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DraftEntry {
    kind: DraftKind,
    name: String,
}

impl DraftEntry {
    /// Builds the entry of the draft of kind `kind` named `name`.
    #[must_use]
    pub const fn new(kind: DraftKind, name: String) -> Self {
        Self { kind, name }
    }

    /// The kind of the draft.
    #[must_use]
    pub const fn kind(&self) -> DraftKind {
        self.kind
    }

    /// The name of the draft.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Storage of what [`ToolsetStore`] cannot do, implemented by the host: listing and deleting drafts, and keeping the drafts
/// of context types, which [`ContextTypeStore`](super::ContextTypeStore) has no notion of.
///
/// The returned futures are `Send`. The trait is used through a generic parameter only, never as a trait object. It has no
/// operation that activates a draft: activation belongs to the host (`agent-sessions/toolsets#FR-8`).
pub trait DraftStore: Send + Sync {
    /// Lists every draft, toolsets and context types alike.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`](super::ToolsetsError::Store) when the host storage fails.
    fn list_drafts(&self) -> impl Future<Output = ToolsetsResult<Vec<DraftEntry>>> + Send;

    /// Removes the draft of kind `kind` named `name`; true when a draft was removed. An active version is never touched.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`](super::ToolsetsError::Store) when the host storage fails.
    fn delete_draft(&self, kind: DraftKind, name: &str) -> impl Future<Output = ToolsetsResult<bool>> + Send;

    /// Stores `context_type` as the draft of its name, replacing any earlier draft of that name.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`](super::ToolsetsError::Store) when the host storage fails.
    fn put_type_draft(&self, context_type: &ContextType) -> impl Future<Output = ToolsetsResult<()>> + Send;

    /// Reads the draft of the context type `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`](super::ToolsetsError::Store) when the host storage fails.
    fn get_type_draft(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<ContextType>>> + Send;
}

/// The module's own state: the stores, the redactor that masks what a script receives, and the detector of secrets.
struct Builder<T, D> {
    toolsets: T,
    drafts: D,
    redactor: Redactor,
    detector: Arc<dyn SecretDetector>,
}

/// A refusal, as the text a script receives. It names a rule and never carries a value.
type Refusal = String;

/// A runtime error of the module, carrying its message. Callers box it, as Rhai expects.
fn runtime_error(message: String) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(message), Position::NONE)
}

/// Runs a storage future to completion through the `vynil-core` bridge, on the calling thread. A storage failure and a
/// refusal of the bridge both become a refusal that carries their message.
fn await_store<V>(future: impl Future<Output = ToolsetsResult<V>>) -> Result<V, Refusal> {
    match block_on(future) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Reads a text field of a definition.
fn text_of(value: &Dynamic, what: &str) -> Result<String, Refusal> {
    value
        .clone()
        .into_string()
        .map_err(|found| format!("{what} must be a string, not {found}"))
}

/// Reads a field of a definition as JSON.
fn json_of(value: &Dynamic, what: &str) -> Result<Value, Refusal> {
    rhai::serde::from_dynamic::<Value>(value).map_err(|_error| format!("{what} must hold plain data"))
}

/// Refuses a key that the definition does not know, naming the keys that it knows.
fn reject_unknown(map: &Map, allowed: &[&str], what: &str) -> Result<(), Refusal> {
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{what}: unknown key `{key}`; expected {}",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

/// Reads a required text field.
fn required_text(map: &Map, key: &str, what: &str) -> Result<String, Refusal> {
    let value = map
        .get(key)
        .ok_or_else(|| format!("{what}: the key `{key}` is missing"))?;
    text_of(value, &format!("{what}: `{key}`"))
}

/// Reads an optional list of maps; an absent key is an empty list.
fn maps_of(map: &Map, key: &str, what: &str) -> Result<Vec<Map>, Refusal> {
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .clone()
        .into_array()
        .map_err(|found| format!("{what}: `{key}` must be an array, not {found}"))?;
    items
        .into_iter()
        .map(|item| {
            item.try_cast::<Map>()
                .ok_or_else(|| format!("{what}: every entry of `{key}` must be a map"))
        })
        .collect()
}

/// Reads an optional list of texts; an absent key is an empty list.
fn texts_of(map: &Map, key: &str, what: &str) -> Result<Vec<String>, Refusal> {
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .clone()
        .into_array()
        .map_err(|found| format!("{what}: `{key}` must be an array, not {found}"))?;
    items
        .iter()
        .map(|item| text_of(item, &format!("{what}: every entry of `{key}`")))
        .collect()
}

/// Reads an optional boolean field, `false` when absent.
fn flag_of(map: &Map, key: &str, what: &str) -> Result<bool, Refusal> {
    map.get(key).map_or(Ok(false), |value| {
        value
            .as_bool()
            .map_err(|found| format!("{what}: `{key}` must be a boolean, not {found}"))
    })
}

/// Reads a version text.
fn version_of(map: &Map, what: &str) -> Result<Semver, Refusal> {
    let text = required_text(map, "version", what)?;
    Semver::parse(&text).map_err(|_error| format!("{what}: `version` is not a semantic version"))
}

/// The kind a script names: `toolset` or `context_type`.
fn kind_of(text: &str) -> Result<DraftKind, Refusal> {
    match text {
        "toolset" => Ok(DraftKind::Toolset),
        "context_type" => Ok(DraftKind::ContextType),
        _ => Err("unknown draft kind; expected toolset or context_type".to_owned()),
    }
}

/// The name of a kind, as a script reads it.
const fn kind_name(kind: DraftKind) -> &'static str {
    match kind {
        DraftKind::Toolset => "toolset",
        DraftKind::ContextType => "context_type",
    }
}

/// Builds a toolset from a definition map; its parameter defaults are judged by `detector`.
fn toolset_of(map: &Map, detector: &Arc<dyn SecretDetector>) -> Result<Toolset, Refusal> {
    const WHAT: &str = "put_toolset_draft";
    reject_unknown(
        map,
        &[
            "name",
            "version",
            "description",
            "functions",
            "params",
            "roles",
            "requires",
            "dependencies",
        ],
        WHAT,
    )?;
    let mut toolset = Toolset::new(
        required_text(map, "name", WHAT)?,
        required_text(map, "description", WHAT)?,
        version_of(map, WHAT)?,
        required_text(map, "functions", WHAT)?,
    )
    .map_err(|error| error.to_string())?
    .with_secret_detector(Arc::clone(detector));
    for param in maps_of(map, "params", WHAT)? {
        reject_unknown(
            &param,
            &["name", "schema", "default"],
            "put_toolset_draft: a parameter",
        )?;
        let schema = param.get("schema").map_or_else(
            || Ok(Value::Object(serde_json::Map::new())),
            |value| json_of(value, "a parameter `schema`"),
        )?;
        let mut declared = ParamDecl::new(
            required_text(&param, "name", "put_toolset_draft: a parameter")?,
            schema,
        );
        if let Some(default) = param.get("default") {
            declared = declared.with_default(json_of(default, "a parameter `default`")?);
        }
        toolset = toolset.with_param(declared).map_err(|error| error.to_string())?;
    }
    for role in maps_of(map, "roles", WHAT)? {
        reject_unknown(&role, &["name", "required"], "put_toolset_draft: a role")?;
        let declared = RoleDecl::new(
            required_text(&role, "name", "put_toolset_draft: a role")?,
            flag_of(&role, "required", "put_toolset_draft: a role")?,
        );
        toolset = toolset.with_role(declared).map_err(|error| error.to_string())?;
    }
    for requirement in texts_of(map, "requires", WHAT)? {
        let parsed = TypeRequirement::parse(&requirement).map_err(|error| error.to_string())?;
        toolset = toolset
            .with_type_requirement(parsed)
            .map_err(|error| error.to_string())?;
    }
    for dependency in texts_of(map, "dependencies", WHAT)? {
        toolset = toolset
            .with_dependency(dependency)
            .map_err(|error| error.to_string())?;
    }
    Ok(toolset)
}

/// Builds a skill from a definition map.
fn skill_of(map: &Map) -> Result<Skill, Refusal> {
    const WHAT: &str = "put_skill_draft";
    reject_unknown(map, &["name", "description", "body", "annexes"], WHAT)?;
    let mut skill = Skill::new(
        required_text(map, "name", WHAT)?,
        required_text(map, "description", WHAT)?,
        required_text(map, "body", WHAT)?,
    )
    .map_err(|error| error.to_string())?;
    if let Some(value) = map.get("annexes") {
        let annexes = value
            .clone()
            .try_cast::<Map>()
            .ok_or_else(|| format!("{WHAT}: `annexes` must be a map of texts"))?;
        for (name, text) in &annexes {
            skill = skill
                .with_annex(name.to_string(), text_of(text, &format!("{WHAT}: an annex"))?)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(skill)
}

/// Checks a toolset without writing it: its module gives a contract, its skill templates compile, and no default value of a
/// parameter nor skill text looks like a secret to `detector`. The render cap belongs to the store, which applies it when
/// the draft is written.
fn check_toolset(toolset: &Toolset, detector: &dyn SecretDetector) -> Result<(), Refusal> {
    let name = toolset.name();
    let _contract = extract_contract(toolset.source()).map_err(|error| {
        if let ToolsetsError::InvalidToolset { reason, detail, .. } = &error {
            invalid_toolset_text(name, reason, detail)
        } else {
            error.to_string()
        }
    })?;
    for param in toolset.params().values() {
        if param
            .default()
            .is_some_and(|value| value_looks_like_secret(value, detector))
        {
            return Err(format!(
                "invalid toolset `{name}`: the default value of parameter `{}` looks like a secret; a toolset holds no value of an environment",
                param.name()
            ));
        }
    }
    for skill in toolset.skills().values() {
        check_template(
            &format!("skill {} description", skill.name()),
            skill.description(),
        )
        .map_err(|error| error.to_string())?;
        check_template(&format!("skill {} body", skill.name()), skill.body())
            .map_err(|error| error.to_string())?;
        let texts = [skill.description(), skill.body()]
            .into_iter()
            .chain(skill.annexes().values().map(String::as_str));
        if texts.into_iter().any(|text| detector.looks_like_secret(text)) {
            return Err(format!(
                "invalid skill `{}` of toolset `{name}`: a text looks like a secret; a skill holds no value of an environment",
                skill.name()
            ));
        }
    }
    Ok(())
}

/// The message of a toolset refusal, as the stores write it.
fn invalid_toolset_text(name: &str, reason: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("invalid toolset `{name}`: {reason}")
    } else {
        format!("invalid toolset `{name}`: {reason} ({detail})")
    }
}

/// Builds a context type from a definition map; the defaults of its schema are judged by `detector`.
fn context_type_of(map: &Map, detector: &dyn SecretDetector) -> Result<ContextType, Refusal> {
    const WHAT: &str = "put_context_type_draft";
    reject_unknown(map, &["name", "version", "schema", "roles", "includes"], WHAT)?;
    let schema = map
        .get("schema")
        .ok_or_else(|| format!("{WHAT}: the key `schema` is missing"))
        .and_then(|value| json_of(value, "`schema`"))?;
    let mut context_type = ContextType::new_with_detector(
        required_text(map, "name", WHAT)?,
        version_of(map, WHAT)?,
        schema,
        detector,
    )
    .map_err(|error| error.to_string())?;
    for role in maps_of(map, "roles", WHAT)? {
        reject_unknown(&role, &["name", "required"], "put_context_type_draft: a role")?;
        let spec = RoleSpec::new(
            required_text(&role, "name", "put_context_type_draft: a role")?,
            flag_of(&role, "required", "put_context_type_draft: a role")?,
        );
        context_type = context_type.with_role(spec).map_err(|error| error.to_string())?;
    }
    for include in texts_of(map, "includes", WHAT)? {
        context_type =
            context_type.with_include(TypeRequirement::parse(&include).map_err(|error| error.to_string())?);
    }
    Ok(context_type)
}

impl<T: ToolsetStore, D: DraftStore> Builder<T, D> {
    /// Masks a refusal with the redactor of the session and turns it into a Rhai error.
    fn raise<V>(&self, outcome: Result<V, Refusal>) -> Result<V, Box<EvalAltResult>> {
        outcome.map_err(|message| Box::new(runtime_error(self.redactor.redact(&message))))
    }

    /// Writes a toolset draft after validating it.
    fn write_toolset(&self, toolset: &Toolset) -> Result<(), Refusal> {
        check_toolset(toolset, self.detector.as_ref())?;
        await_store(self.toolsets.put_draft(toolset))
    }

    fn put_toolset(&self, definition: &Map) -> Result<String, Refusal> {
        let toolset = toolset_of(definition, &self.detector)?;
        self.write_toolset(&toolset)?;
        Ok(toolset.name().to_owned())
    }

    /// Adds `skill` to the draft of `toolset` (replacing the skill of the same name), created from the active version when there is no draft.
    fn add_skill(&self, toolset: &str, skill: Skill) -> Result<String, Refusal> {
        let base = match await_store(self.toolsets.get_draft(toolset))? {
            Some(draft) => draft,
            None => await_store(self.toolsets.get_active(toolset))?
                .ok_or_else(|| format!("toolset `{toolset}` has neither a draft nor an active version"))?,
        };
        let name = skill.name().to_owned();
        let updated = base.replace_skill(skill).map_err(|error| error.to_string())?;
        self.write_toolset(&updated)?;
        Ok(name)
    }

    fn put_skill(&self, toolset: &str, definition: &Map) -> Result<String, Refusal> {
        self.add_skill(toolset, skill_of(definition)?)
    }

    fn put_skill_text(&self, toolset: &str, text: &str) -> Result<String, Refusal> {
        self.add_skill(toolset, Skill::parse(text).map_err(|error| error.to_string())?)
    }

    fn put_type(&self, definition: &Map) -> Result<String, Refusal> {
        let context_type = context_type_of(definition, self.detector.as_ref())?;
        await_store(self.drafts.put_type_draft(&context_type))?;
        Ok(context_type.name().to_owned())
    }

    /// The gaps of a draft, as texts; empty when it is valid. Nothing is written.
    fn validate(&self, kind: &str, name: &str) -> Result<Array, Refusal> {
        let gap = match kind_of(kind)? {
            DraftKind::Toolset => {
                let draft = await_store(self.toolsets.get_draft(name))?
                    .ok_or_else(|| format!("there is no draft of toolset `{name}`"))?;
                check_toolset(&draft, self.detector.as_ref()).err()
            }
            DraftKind::ContextType => {
                let draft = await_store(self.drafts.get_type_draft(name))?
                    .ok_or_else(|| format!("there is no draft of context type `{name}`"))?;
                reads_back(&draft).err()
            }
        };
        Ok(gap.into_iter().map(Dynamic::from).collect())
    }

    fn list(&self) -> Result<Array, Refusal> {
        Ok(await_store(self.drafts.list_drafts())?
            .iter()
            .map(|entry| {
                let mut map = Map::new();
                let _kind = map.insert("kind".into(), Dynamic::from(kind_name(entry.kind()).to_owned()));
                let _name = map.insert("name".into(), Dynamic::from(self.redactor.redact(entry.name())));
                Dynamic::from(map)
            })
            .collect())
    }

    fn delete(&self, kind: &str, name: &str) -> Result<bool, Refusal> {
        await_store(self.drafts.delete_draft(kind_of(kind)?, name))
    }
}

/// Checks that a context type draft reads back from its stored form.
fn reads_back(context_type: &ContextType) -> Result<(), Refusal> {
    let text = serde_json::to_string(context_type).map_err(|error| error.to_string())?;
    serde_json::from_str::<ContextType>(&text)
        .map(|_| ())
        .map_err(|_error| {
            format!(
                "invalid context type `{}`: the stored form does not read back",
                context_type.name()
            )
        })
}

/// The `toolset_builder` module over the builder state.
fn builder_module<T, D>(state: &Arc<Builder<T, D>>) -> Module
where
    T: ToolsetStore + 'static,
    D: DraftStore + 'static,
{
    let mut module = Module::new();
    let for_toolset = Arc::clone(state);
    let _put_toolset = module.set_native_fn("put_toolset_draft", move |definition: Map| {
        for_toolset.raise(for_toolset.put_toolset(&definition))
    });
    let for_skill = Arc::clone(state);
    let _put_skill = module.set_native_fn(
        "put_skill_draft",
        move |toolset: ImmutableString, definition: Map| {
            for_skill.raise(for_skill.put_skill(toolset.as_str(), &definition))
        },
    );
    let for_text = Arc::clone(state);
    let _put_skill_text = module.set_native_fn(
        "put_skill_draft_text",
        move |toolset: ImmutableString, text: ImmutableString| {
            for_text.raise(for_text.put_skill_text(toolset.as_str(), text.as_str()))
        },
    );
    let for_type = Arc::clone(state);
    let _put_type = module.set_native_fn("put_context_type_draft", move |definition: Map| {
        for_type.raise(for_type.put_type(&definition))
    });
    let for_validate = Arc::clone(state);
    let _validate = module.set_native_fn(
        "validate_draft",
        move |kind: ImmutableString, name: ImmutableString| {
            for_validate.raise(for_validate.validate(kind.as_str(), name.as_str()))
        },
    );
    let for_list = Arc::clone(state);
    let _list = module.set_native_fn("list_drafts", move || for_list.raise(for_list.list()));
    let for_delete = Arc::clone(state);
    let _delete = module.set_native_fn(
        "delete_draft",
        move |kind: ImmutableString, name: ImmutableString| {
            for_delete.raise(for_delete.delete(kind.as_str(), name.as_str()))
        },
    );
    module
}

/// Registration step of the module `toolset_builder`, for `EngineBuilder::with_registration`.
///
/// The module writes toolset drafts through `toolsets` and the other drafts through `drafts`; it has no operation that
/// activates a draft. `redactor` masks every error a script receives, and `detector` judges the default values, the
/// schemas and the skill texts (pass `Arc::new(DefaultSecretDetector)` for the library heuristic). The module is for
/// the configuration agent only: the creation rule of the compatibility check refuses any other agent at load time.
pub fn builder_registration<T, D>(
    toolsets: T,
    drafts: D,
    redactor: Redactor,
    detector: Arc<dyn SecretDetector>,
) -> impl FnOnce(&mut RhaiEngine) -> ScratchpadResult<()> + 'static
where
    T: ToolsetStore + 'static,
    D: DraftStore + 'static,
{
    let state = Arc::new(Builder {
        toolsets,
        drafts,
        redactor,
        detector,
    });
    move |engine: &mut RhaiEngine| {
        let _module = engine.register_static_module("toolset_builder", Shared::new(builder_module(&state)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, PoisonError};

    use super::*;
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        GrantedModule, Grants, RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits,
        ScriptErrorClass,
    };
    use crate::secret::DefaultSecretDetector;
    use crate::session::CancelHandle;

    /// A host that stores drafts without validating them, so a test can hold a draft that became invalid. Clones share the
    /// same storage.
    #[derive(Clone, Default)]
    struct Host {
        drafts: Arc<Mutex<BTreeMap<String, Toolset>>>,
        active: Arc<Mutex<BTreeMap<String, Toolset>>>,
        types: Arc<Mutex<BTreeMap<String, ContextType>>>,
        broken: Arc<AtomicBool>,
        /// When set, `put_draft` applies this render cap to the skills, as the in-memory store does.
        render_cap: Option<u64>,
    }

    impl Host {
        fn failure(&self) -> ToolsetsResult<()> {
            if self.broken.load(Ordering::SeqCst) {
                Err(ToolsetsError::Store("disk gone".to_owned()))
            } else {
                Ok(())
            }
        }

        fn draft(&self, name: &str) -> Option<Toolset> {
            self.drafts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned()
        }

        fn active_version(&self, name: &str) -> Option<Toolset> {
            self.active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned()
        }

        fn type_draft(&self, name: &str) -> Option<ContextType> {
            self.types
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned()
        }

        fn seed_draft(&self, toolset: Toolset) {
            let _previous = self
                .drafts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(toolset.name().to_owned(), toolset);
        }

        fn seed_active(&self, toolset: Toolset) {
            let _previous = self
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(toolset.name().to_owned(), toolset);
        }
    }

    impl ToolsetStore for Host {
        fn get_active(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send {
            std::future::ready(self.failure().map(|()| self.active_version(name)))
        }

        fn get_draft(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send {
            std::future::ready(self.failure().map(|()| self.draft(name)))
        }

        fn put_draft(&self, toolset: &Toolset) -> impl Future<Output = ToolsetsResult<()>> + Send {
            let capped = self.render_cap.map_or(Ok(()), |cap| {
                toolset
                    .skills()
                    .values()
                    .try_for_each(|skill| skill.check_size(cap))
            });
            std::future::ready(
                self.failure()
                    .and(capped)
                    .map(|()| self.seed_draft(toolset.clone())),
            )
        }
    }

    impl DraftStore for Host {
        fn list_drafts(&self) -> impl Future<Output = ToolsetsResult<Vec<DraftEntry>>> + Send {
            let mut entries: Vec<DraftEntry> = self
                .drafts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .map(|name| DraftEntry::new(DraftKind::Toolset, name.clone()))
                .collect();
            entries.extend(
                self.types
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .keys()
                    .map(|name| DraftEntry::new(DraftKind::ContextType, name.clone())),
            );
            std::future::ready(self.failure().map(|()| entries))
        }

        fn delete_draft(
            &self,
            kind: DraftKind,
            name: &str,
        ) -> impl Future<Output = ToolsetsResult<bool>> + Send {
            let removed = match kind {
                DraftKind::Toolset => self
                    .drafts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(name)
                    .is_some(),
                DraftKind::ContextType => self
                    .types
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(name)
                    .is_some(),
            };
            std::future::ready(self.failure().map(|()| removed))
        }

        fn put_type_draft(
            &self,
            context_type: &ContextType,
        ) -> impl Future<Output = ToolsetsResult<()>> + Send {
            let name = context_type.name().to_owned();
            let _previous = self
                .types
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name, context_type.clone());
            std::future::ready(self.failure())
        }

        fn get_type_draft(
            &self,
            name: &str,
        ) -> impl Future<Output = ToolsetsResult<Option<ContextType>>> + Send {
            std::future::ready(self.failure().map(|()| self.type_draft(name)))
        }
    }

    /// A detector that refuses what contains `abc` and nothing else: the default one would refuse `sk-` instead.
    struct AbcDetector;

    impl SecretDetector for AbcDetector {
        fn looks_like_secret(&self, text: &str) -> bool {
            text.contains("abc")
        }
    }

    fn version(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    fn pad_with(host: &Host, detector: Arc<dyn SecretDetector>) -> Scratchpad {
        let registration = builder_registration(host.clone(), host.clone(), Redactor::new(), detector);
        match ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .with_registration(registration)
            .build()
        {
            Ok(engine) => Scratchpad::new(Arc::new(engine)),
            Err(error) => panic!("the builder engine must build: {error}"),
        }
    }

    fn pad(host: &Host) -> Scratchpad {
        pad_with(host, Arc::new(DefaultSecretDetector))
    }

    async fn run(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("config-agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    /// The script of a valid toolset definition, whose `default` is the Rhai literal `default`.
    fn definition(default: &str) -> String {
        format!(
            r#"toolset_builder::put_toolset_draft(#{{
                name: "grafana",
                version: "1.0.0",
                description: "Grafana queries",
                functions: "fn query(expr) {{ expr }}",
                params: [#{{ name: "endpoint", schema: #{{ type: "string" }}, "default": {default} }}],
                dependencies: ["http"]
            }})"#
        )
    }

    fn stored_toolset(name: &str, source: &str) -> Toolset {
        Toolset::new(
            name.to_owned(),
            "A stored toolset".to_owned(),
            version("1.0.0"),
            source.to_owned(),
        )
        .expect("the toolset builds")
    }

    // Scenario: un toolset valide est écrit comme brouillon et reste inactif
    #[tokio::test(flavor = "multi_thread")]
    async fn a_valid_toolset_is_written_as_a_draft_and_stays_inactive() {
        let host = Host::default();
        host.seed_active(stored_toolset("grafana", "fn old() { 1 }"));
        let outcome = run(&pad(&host), &definition(r#""https://grafana.local""#)).await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        assert!(
            outcome.text().contains("grafana"),
            "the name is returned: {}",
            outcome.text()
        );
        let draft = host.draft("grafana").expect("the draft is readable");
        assert_eq!(draft.version(), &version("1.0.0"));
        assert!(
            draft.source().contains("fn query"),
            "the functions are the module source"
        );
        assert!(
            draft.params().contains_key("endpoint"),
            "the parameter is declared"
        );
        assert!(
            draft.dependencies().contains("http"),
            "the dependency is declared"
        );
        let active = host
            .active_version("grafana")
            .expect("the active version is still there");
        assert_eq!(
            active.source(),
            "fn old() { 1 }",
            "the active version is not touched"
        );
    }

    // Scenario: un toolset valide est écrit comme brouillon et reste inactif (nothing active without activation)
    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_toolset_has_no_active_version() {
        let host = Host::default();
        let outcome = run(&pad(&host), &definition(r#""https://grafana.local""#)).await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        assert!(host.draft("grafana").is_some(), "the draft is written");
        assert!(
            host.active_version("grafana").is_none(),
            "get_active gives nothing"
        );
    }

    // Scenario: un brouillon invalide n'est pas écrit
    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_draft_is_not_written() {
        let host = Host::default();
        let script = r#"toolset_builder::put_toolset_draft(#{
            name: "grafana", version: "1.0.0", description: "Grafana queries",
            functions: "fn query( {"
        })"#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("does not compile"),
            "the error names the rule: {}",
            outcome.text()
        );
        assert!(host.draft("grafana").is_none(), "nothing is written");
    }

    // Not a Scenario: a key the definition does not know is refused and named.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_key_is_refused() {
        let host = Host::default();
        let script = r#"toolset_builder::put_toolset_draft(#{
            name: "grafana", version: "1.0.0", description: "d", functions: "fn a() { 1 }", activate: true
        })"#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("activate"),
            "the key is named: {}",
            outcome.text()
        );
        assert!(host.draft("grafana").is_none(), "nothing is written");
    }

    // Scenario: un skill s'écrit en map et en texte
    #[tokio::test(flavor = "multi_thread")]
    async fn a_skill_is_written_as_a_map_and_as_text() {
        let host = Host::default();
        host.seed_draft(stored_toolset("grafana", "fn query() { 1 }"));
        let script = r#"
            toolset_builder::put_skill_draft("grafana", #{
                name: "queries", description: "How to query", body: "Call query().",
                annexes: #{ "notes.md": "Some notes" }
            });
            toolset_builder::put_skill_draft_text("grafana", "---\nname: alerts\ndescription: About alerts\n---\nLook at alerts.\n")
        "#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        assert!(
            outcome.text().contains("alerts"),
            "the skill name is returned: {}",
            outcome.text()
        );
        let draft = host.draft("grafana").expect("the draft is readable");
        let queries: &Skill = draft.skills().get("queries").expect("the map skill is there");
        assert_eq!(queries.body(), "Call query().");
        assert_eq!(queries.annex("notes.md"), Some("Some notes"));
        assert!(draft.skills().contains_key("alerts"), "the text skill is there");
        let refused = run(
            &pad(&host),
            r#"toolset_builder::put_skill_draft_text("grafana", "no frontmatter at all")"#,
        )
        .await;
        assert_eq!(
            refused.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            refused.text()
        );
        assert_eq!(host.draft("grafana").map(|stored| stored.skills().len()), Some(2));
    }

    // Scenario: un skill de même nom est remplacé dans le brouillon
    #[tokio::test(flavor = "multi_thread")]
    async fn a_skill_of_the_same_name_is_replaced_in_the_draft() {
        let host = Host::default();
        host.seed_active(stored_toolset("grafana", "fn query() { 1 }"));
        host.seed_draft(
            stored_toolset("grafana", "fn query() { 1 }")
                .with_skill(
                    Skill::new("deploy".to_owned(), "d".to_owned(), "first".to_owned()).expect("a skill"),
                )
                .expect("the skill is added"),
        );
        let script = r#"
            toolset_builder::put_skill_draft("grafana", #{ name: "deploy", description: "d", body: "second" });
            toolset_builder::put_skill_draft_text("grafana", "---\nname: deploy\ndescription: d\n---\nthird\n")
        "#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        let draft = host.draft("grafana").expect("the draft is readable");
        assert_eq!(draft.skills().len(), 1, "a single `deploy`");
        let deploy = draft.skills().get("deploy").expect("deploy is there");
        assert!(
            deploy.body().starts_with("third"),
            "the last one wins: {}",
            deploy.body()
        );
        let active = host
            .active_version("grafana")
            .expect("the active version is there");
        assert!(active.skills().is_empty(), "the active version did not move");
    }

    // Scenario: le plafond de rendu est appliqué par le store
    #[tokio::test(flavor = "multi_thread")]
    async fn the_render_cap_is_applied_by_the_store() {
        let host = Host {
            render_cap: Some(10),
            ..Host::default()
        };
        host.seed_draft(stored_toolset("grafana", "fn query() { 1 }"));
        let script = r#"toolset_builder::put_skill_draft("grafana", #{
            name: "big", description: "d", body: "this body is far longer than ten characters"
        })"#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            outcome.text()
        );
        let draft = host.draft("grafana").expect("the draft is readable");
        assert!(!draft.skills().contains_key("big"), "the skill is not stored");
    }

    // Not a Scenario: without a draft, the skill is added to a draft created from the active version.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_skill_starts_a_draft_from_the_active_version() {
        let host = Host::default();
        host.seed_active(stored_toolset("grafana", "fn query() { 1 }"));
        let script = r#"toolset_builder::put_skill_draft("grafana", #{
            name: "queries", description: "How to query", body: "Call query()."
        })"#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        let draft = host.draft("grafana").expect("a draft was created");
        assert_eq!(
            draft.source(),
            "fn query() { 1 }",
            "it starts from the active version"
        );
        assert!(draft.skills().contains_key("queries"));
        let missing = run(
            &pad(&host),
            r#"toolset_builder::put_skill_draft("absent", #{ name: "x", description: "d", body: "b" })"#,
        )
        .await;
        assert_eq!(
            missing.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            missing.text()
        );
        assert!(
            missing.text().contains("absent"),
            "the toolset is named: {}",
            missing.text()
        );
    }

    // Scenario: un secret est refusé et jamais rendu
    #[tokio::test(flavor = "multi_thread")]
    async fn a_secret_is_refused_by_the_host_detector_and_never_returned() {
        let host = Host::default();
        let pad = pad_with(&host, Arc::new(AbcDetector));
        let refused = run(&pad, &definition(r#""sk-abc123""#)).await;
        assert_eq!(
            refused.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            refused.text()
        );
        assert!(
            !refused.text().contains("sk-abc123"),
            "the value is not in the error: {}",
            refused.text()
        );
        assert!(host.draft("grafana").is_none(), "nothing is written");
        // The detector of the host replaces the default one: `sk-other` passes.
        let accepted = run(&pad, &definition(r#""sk-other""#)).await;
        assert_eq!(accepted.status(), RunStatus::Ok, "{}", accepted.text());
        // A skill text is judged by the same detector.
        let skill = run(
            &pad,
            r#"toolset_builder::put_skill_draft("grafana", #{ name: "s", description: "d", body: "token abc123" })"#,
        )
        .await;
        assert_eq!(
            skill.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            skill.text()
        );
        assert!(
            !skill.text().contains("abc123"),
            "the text is not returned: {}",
            skill.text()
        );
    }

    // Scenario: validate, list et delete
    #[tokio::test(flavor = "multi_thread")]
    async fn validate_list_and_delete() {
        let host = Host::default();
        host.seed_draft(stored_toolset("good", "fn a() { 1 }"));
        host.seed_draft(stored_toolset("bad", "fn a( {"));
        let pad = pad(&host);
        let good = run(&pad, r#"toolset_builder::validate_draft("toolset", "good")"#).await;
        assert_eq!(good.status(), RunStatus::Ok, "{}", good.text());
        assert_eq!(
            good.text().trim(),
            "[]",
            "a valid draft has no gap: {}",
            good.text()
        );
        let bad = run(&pad, r#"toolset_builder::validate_draft("toolset", "bad")"#).await;
        assert_eq!(bad.status(), RunStatus::Ok, "{}", bad.text());
        assert!(
            bad.text().contains("does not compile"),
            "the gap is listed: {}",
            bad.text()
        );
        let list = run(&pad, "toolset_builder::list_drafts()").await;
        assert_eq!(list.status(), RunStatus::Ok, "{}", list.text());
        assert!(
            list.text().contains("good") && list.text().contains("bad"),
            "{}",
            list.text()
        );
        assert!(
            list.text().contains("toolset"),
            "the kind is listed: {}",
            list.text()
        );
        let first = run(&pad, r#"toolset_builder::delete_draft("toolset", "bad")"#).await;
        assert_eq!(first.text().trim(), "true", "{}", first.text());
        let second = run(&pad, r#"toolset_builder::delete_draft("toolset", "bad")"#).await;
        assert_eq!(second.text().trim(), "false", "{}", second.text());
        assert!(host.draft("bad").is_none());
        assert!(host.draft("good").is_some());
        let unknown = run(&pad, r#"toolset_builder::validate_draft("toolset", "absent")"#).await;
        assert_eq!(
            unknown.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            unknown.text()
        );
    }

    // Scenario: aucune activation n'existe
    #[tokio::test(flavor = "multi_thread")]
    async fn no_function_activates() {
        let host = Host::default();
        // The module is granted as well, because introspection lists granted modules only.
        let state = Arc::new(Builder {
            toolsets: host.clone(),
            drafts: host.clone(),
            redactor: Redactor::new(),
            detector: Arc::new(DefaultSecretDetector),
        });
        let granted = GrantedModule::new("toolset_builder", Shared::new(builder_module(&state)))
            .expect("a valid module name");
        let grants = Grants::new().with_module(granted).expect("one module");
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), grants)
            .build()
            .expect("the engine builds");
        let outcome = run(
            &Scratchpad::new(Arc::new(engine)),
            r#"functions("toolset_builder").map(|f| f.path)"#,
        )
        .await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        let text = outcome.text();
        for name in [
            "put_toolset_draft",
            "put_skill_draft",
            "put_skill_draft_text",
            "put_context_type_draft",
            "validate_draft",
            "list_drafts",
            "delete_draft",
        ] {
            assert!(text.contains(name), "`{name}` is exposed: {text}");
        }
        assert!(
            !text.to_lowercase().contains("activate"),
            "no function activates: {text}"
        );
        assert!(
            !text.to_lowercase().contains("active"),
            "no function writes an active version: {text}"
        );
    }

    // Not a Scenario: a context type draft goes to the DraftStore.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_context_type_is_written_through_the_draft_store() {
        let host = Host::default();
        let script = r#"toolset_builder::put_context_type_draft(#{
            name: "cluster", version: "1.0.0",
            schema: #{ type: "object", properties: #{ namespace: #{ type: "string" } }, required: ["namespace"] },
            roles: [#{ name: "k8s", required: true }]
        })"#;
        let outcome = run(&pad(&host), script).await;
        assert_eq!(outcome.status(), RunStatus::Ok, "{}", outcome.text());
        assert!(host.type_draft("cluster").is_some(), "the type draft is stored");
        let invalid = run(
            &pad(&host),
            r#"toolset_builder::put_context_type_draft(#{ name: "Bad Name", version: "1.0.0", schema: #{ type: "object" } })"#,
        )
        .await;
        assert_eq!(
            invalid.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            invalid.text()
        );
        assert!(host.type_draft("Bad Name").is_none());
        let validated = run(
            &pad(&host),
            r#"toolset_builder::validate_draft("context_type", "cluster")"#,
        )
        .await;
        assert_eq!(validated.text().trim(), "[]", "{}", validated.text());
    }

    // Not a Scenario: a failure of the host storage reaches the script.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_storage_failure_reaches_the_script() {
        let host = Host::default();
        host.broken.store(true, Ordering::SeqCst);
        let outcome = run(&pad(&host), &definition(r#""https://grafana.local""#)).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            outcome.text()
        );
        assert!(outcome.text().contains("disk gone"), "{}", outcome.text());
    }

    // Not a Scenario: the redactor of the session masks what the script receives.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_redactor_masks_errors() {
        let host = Host::default();
        host.broken.store(true, Ordering::SeqCst);
        let mut redactor = Redactor::new();
        redactor.add_secret("disk gone").expect("a secret");
        let registration = builder_registration(
            host.clone(),
            host.clone(),
            redactor,
            Arc::new(DefaultSecretDetector),
        );
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), Grants::new())
            .with_registration(registration)
            .build()
            .expect("the engine builds");
        let pad = Scratchpad::new(Arc::new(engine));
        let outcome = run(&pad, &definition(r#""https://grafana.local""#)).await;
        assert_eq!(
            outcome.status(),
            RunStatus::Failed(ScriptErrorClass::Runtime),
            "{}",
            outcome.text()
        );
        assert!(!outcome.text().contains("disk gone"), "{}", outcome.text());
    }
}
