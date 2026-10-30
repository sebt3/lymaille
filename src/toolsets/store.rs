//! Storage traits of the toolsets, of the context types and of the toolset contexts, implemented by the host, and an in-memory
//! store for tests. A toolset written by `put_draft` is a draft: no session sees it until the host activates it
//! (`agent-sessions/toolsets#FR-5`, `FR-8`, `FR-9`, `AC-13`, `agent-sessions#CON-1`).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    ContextType, Toolset, ToolsetContext, ToolsetsError, ToolsetsResult, check_template, extract_contract,
};
use crate::agents::Layered;

/// Storage of toolsets, implemented by the host. Reads and writes are asynchronous; the returned futures are `Send`. The
/// trait is used through a generic parameter only, never as a trait object.
///
/// The trait has no operation that activates a draft: activation belongs to the host (`agent-sessions/toolsets#FR-8`).
/// The trait has no listing and no removal either.
pub trait ToolsetStore: Send + Sync {
    /// Reads the active version of the toolset `name`, or `None` when there is none. A draft is never returned.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`] when the host storage fails.
    fn get_active(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send;

    /// Reads the draft of the toolset `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`] when the host storage fails.
    fn get_draft(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send;

    /// Stores `toolset` as the draft of its name, replacing any earlier draft of that name. The toolset is validated before
    /// it is stored: its source must give a contract, its skill templates must compile, and its skills must fit the render
    /// cap. An implementation other than [`InMemoryToolsets`] must apply the same checks.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`], [`ToolsetsError::Template`] or [`ToolsetsError::SkillTooLarge`] when the
    /// toolset is refused, and [`ToolsetsError::Store`] when the host storage fails.
    fn put_draft(&self, toolset: &Toolset) -> impl Future<Output = ToolsetsResult<()>> + Send;
}

/// Storage of context types, implemented by the host.
///
/// The trait is used through a generic parameter only, never as a trait object.
pub trait ContextTypeStore: Send + Sync {
    /// Reads the context type `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`] when the host storage fails.
    fn get_type(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<ContextType>>> + Send;

    /// Stores `context_type` under its name, replacing any earlier type of that name.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContextType`] when the type is refused, and [`ToolsetsError::Store`] when the host
    /// storage fails.
    fn put_type(&self, context_type: &ContextType) -> impl Future<Output = ToolsetsResult<()>> + Send;
}

/// Storage of toolset contexts, implemented by the host.
///
/// The trait is used through a generic parameter only, never as a trait object.
pub trait ContextStore: Send + Sync {
    /// Reads the toolset context `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`] when the host storage fails.
    fn get_context(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<ToolsetContext>>> + Send;

    /// Stores `context` under its name, replacing any earlier context of that name.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::Store`] when the host storage fails.
    fn put_context(&self, context: &ToolsetContext) -> impl Future<Output = ToolsetsResult<()>> + Send;
}

/// In-memory toolsets store.
///
/// It keeps the JSON text of each object, never the live object, so a value read back goes through the same format as a
/// value read from a real storage. Each draft is validated against the render cap given at construction. A poisoned lock
/// is reported as [`ToolsetsError::Poisoned`]. It performs no I/O.
#[non_exhaustive]
pub struct InMemoryToolsets {
    render_cap_chars: u64,
    drafts: Mutex<BTreeMap<String, String>>,
    active: Mutex<BTreeMap<String, String>>,
    types: Mutex<BTreeMap<String, String>>,
    contexts: Mutex<BTreeMap<String, String>>,
}

impl fmt::Debug for InMemoryToolsets {
    /// Shows the render cap and the names of the stored objects, never their JSON text: a draft holds the source of a module.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryToolsets")
            .field("render_cap_chars", &self.render_cap_chars)
            .field("drafts", &names(&self.drafts))
            .field("active", &names(&self.active))
            .field("types", &names(&self.types))
            .field("contexts", &names(&self.contexts))
            .finish()
    }
}

impl InMemoryToolsets {
    /// Builds an empty store. `render_cap_chars` is the largest number of characters a skill body or annex may hold.
    #[must_use]
    pub const fn new(render_cap_chars: u64) -> Self {
        Self {
            render_cap_chars,
            drafts: Mutex::new(BTreeMap::new()),
            active: Mutex::new(BTreeMap::new()),
            types: Mutex::new(BTreeMap::new()),
            contexts: Mutex::new(BTreeMap::new()),
        }
    }

    /// Makes the draft of `name` the active version. This is an operation of the host, outside the traits: the trait
    /// offers none.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when there is no draft named `name`, and [`ToolsetsError::Poisoned`] when
    /// an internal lock was poisoned.
    pub fn activate(&self, name: &str) -> ToolsetsResult<()> {
        let draft = lock(&self.drafts)?.get(name).cloned();
        let Some(text) = draft else {
            return Err(ToolsetsError::InvalidToolset {
                name: name.to_owned(),
                reason: "there is no draft to activate",
                detail: String::new(),
            });
        };
        let _previous = lock(&self.active)?.insert(name.to_owned(), text);
        Ok(())
    }
}

impl ToolsetStore for InMemoryToolsets {
    fn get_active(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send {
        std::future::ready(read(&self.active, name))
    }

    fn get_draft(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send {
        std::future::ready(read(&self.drafts, name))
    }

    fn put_draft(&self, toolset: &Toolset) -> impl Future<Output = ToolsetsResult<()>> + Send {
        std::future::ready(
            validate_draft(toolset, self.render_cap_chars)
                .and_then(|()| write(&self.drafts, toolset.name(), toolset)),
        )
    }
}

impl ContextTypeStore for InMemoryToolsets {
    fn get_type(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<ContextType>>> + Send {
        std::future::ready(read(&self.types, name))
    }

    fn put_type(&self, context_type: &ContextType) -> impl Future<Output = ToolsetsResult<()>> + Send {
        std::future::ready(
            check_readable_type(context_type)
                .and_then(|()| write(&self.types, context_type.name(), context_type)),
        )
    }
}

impl ContextStore for InMemoryToolsets {
    fn get_context(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<ToolsetContext>>> + Send {
        std::future::ready(read(&self.contexts, name))
    }

    fn put_context(&self, context: &ToolsetContext) -> impl Future<Output = ToolsetsResult<()>> + Send {
        std::future::ready(write(&self.contexts, context.name(), context))
    }
}

/// Cascade: the top layer answers first, and the base layer answers when the top layer gives nothing. Every `put_*` writes
/// to the top layer only. An object of the top layer replaces the object of the base layer whole, never merged.
impl<Top: ToolsetStore, Base: ToolsetStore> ToolsetStore for Layered<Top, Base> {
    async fn get_active(&self, name: &str) -> ToolsetsResult<Option<Toolset>> {
        match self.top().get_active(name).await? {
            Some(found) => Ok(Some(found)),
            None => self.base().get_active(name).await,
        }
    }

    async fn get_draft(&self, name: &str) -> ToolsetsResult<Option<Toolset>> {
        match self.top().get_draft(name).await? {
            Some(found) => Ok(Some(found)),
            None => self.base().get_draft(name).await,
        }
    }

    fn put_draft(&self, toolset: &Toolset) -> impl Future<Output = ToolsetsResult<()>> + Send {
        self.top().put_draft(toolset)
    }
}

impl<Top: ContextTypeStore, Base: ContextTypeStore> ContextTypeStore for Layered<Top, Base> {
    async fn get_type(&self, name: &str) -> ToolsetsResult<Option<ContextType>> {
        match self.top().get_type(name).await? {
            Some(found) => Ok(Some(found)),
            None => self.base().get_type(name).await,
        }
    }

    fn put_type(&self, context_type: &ContextType) -> impl Future<Output = ToolsetsResult<()>> + Send {
        self.top().put_type(context_type)
    }
}

impl<Top: ContextStore, Base: ContextStore> ContextStore for Layered<Top, Base> {
    async fn get_context(&self, name: &str) -> ToolsetsResult<Option<ToolsetContext>> {
        match self.top().get_context(name).await? {
            Some(found) => Ok(Some(found)),
            None => self.base().get_context(name).await,
        }
    }

    fn put_context(&self, context: &ToolsetContext) -> impl Future<Output = ToolsetsResult<()>> + Send {
        self.top().put_context(context)
    }
}

/// The names of the objects of one map, for a debug output. A poisoned lock is shown as an error, never recovered.
fn names(map: &Mutex<BTreeMap<String, String>>) -> Result<Vec<String>, &'static str> {
    match map.lock() {
        Ok(guard) => Ok(guard.keys().cloned().collect()),
        Err(_poisoned) => Err("poisoned lock"),
    }
}

/// Locks one of the maps of JSON texts. A poisoned lock is reported, never recovered.
fn lock(map: &Mutex<BTreeMap<String, String>>) -> ToolsetsResult<MutexGuard<'_, BTreeMap<String, String>>> {
    map.lock().map_err(|_poisoned| ToolsetsError::Poisoned)
}

/// Reads the object whose JSON text is stored under `key`, or `None` when there is none.
fn read<T: DeserializeOwned>(map: &Mutex<BTreeMap<String, String>>, key: &str) -> ToolsetsResult<Option<T>> {
    let text = lock(map)?.get(key).cloned();
    text.map(|text| serde_json::from_str(&text).map_err(|error| ToolsetsError::Store(error.to_string())))
        .transpose()
}

/// Stores the JSON text of `value` under `key`, replacing the previous text.
fn write<T: Serialize>(map: &Mutex<BTreeMap<String, String>>, key: &str, value: &T) -> ToolsetsResult<()> {
    let text = serde_json::to_string(value).map_err(|error| ToolsetsError::Store(error.to_string()))?;
    let _previous = lock(map)?.insert(key.to_owned(), text);
    Ok(())
}

/// Validates a draft before it is stored: its source gives a contract, its skill templates compile, and its skills fit the
/// render cap. The error names the toolset; a source text is never carried.
fn validate_draft(toolset: &Toolset, render_cap_chars: u64) -> ToolsetsResult<()> {
    match extract_contract(toolset.source()) {
        Ok(_contract) => {}
        Err(ToolsetsError::InvalidToolset { reason, detail, .. }) => {
            return Err(ToolsetsError::InvalidToolset {
                name: toolset.name().to_owned(),
                reason,
                detail,
            });
        }
        Err(error) => return Err(error),
    }
    for skill in toolset.skills().values() {
        skill.check_size(render_cap_chars)?;
        check_template(
            &format!("skill {} description", skill.name()),
            skill.description(),
        )?;
        check_template(&format!("skill {} body", skill.name()), skill.body())?;
    }
    Ok(())
}

/// Checks that a context type reads back from its stored form. A type built by its constructors always does; the check
/// keeps the store from holding a type that could not be read.
fn check_readable_type(context_type: &ContextType) -> ToolsetsResult<()> {
    let text =
        serde_json::to_string(context_type).map_err(|error| ToolsetsError::Store(error.to_string()))?;
    serde_json::from_str::<ContextType>(&text)
        .map(|_| ())
        .map_err(|_error| ToolsetsError::InvalidContextType {
            name: context_type.name().to_owned(),
            reason: "the stored form does not read back",
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use serde_json::json;
    use vynil_core::Semver;

    use super::{ContextStore as _, ContextTypeStore as _, InMemoryToolsets, ToolsetStore};
    use crate::agents::Layered;
    use crate::toolsets::{
        ContextLevel, ContextType, RoleSpec, Skill, Toolset, ToolsetContext, ToolsetsError, ToolsetsResult,
    };

    /// Render cap used by the tests, in characters.
    const CAP: u64 = 8_000;

    fn semver(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    fn context_type() -> ContextType {
        ContextType::new(
            "cluster".to_owned(),
            semver("1.0.0"),
            json!({"type": "object", "properties": {"cluster": {"type": "string"}}, "required": ["cluster"]}),
        )
        .expect("a valid context type")
        .with_role(RoleSpec::new("k8s".to_owned(), true))
        .expect("a plain role")
    }

    fn toolset_context() -> ToolsetContext {
        ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter("cluster".to_owned(), json!("prod"))
            .expect("a plain parameter")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("a plain role")
    }

    /// A toolset whose module is `source`, with one skill whose body is `body`.
    fn toolset_with(source: &str, body: &str) -> Toolset {
        let skill = Skill::new(
            "deploy".to_owned(),
            "Deploy a release".to_owned(),
            body.to_owned(),
        )
        .expect("a valid skill");
        Toolset::new(
            "k8s_ops".to_owned(),
            "Kubernetes operations".to_owned(),
            semver("1.0.0"),
            source.to_owned(),
        )
        .expect("a valid toolset")
        .with_skill(skill)
        .expect("a plain skill")
    }

    fn valid_toolset() -> Toolset {
        toolset_with(
            "fn ping() { 1 }",
            "Run the deploy steps on {{context.params.cluster}}",
        )
    }

    // Scenario: put then get round trips
    #[tokio::test]
    async fn put_then_get_round_trips() {
        let store = InMemoryToolsets::new(CAP);
        let context_type = context_type();
        store
            .put_type(&context_type)
            .await
            .expect("a valid type is stored");
        assert_eq!(
            store.get_type("cluster").await.expect("the lookup succeeds"),
            Some(context_type),
            "the type read back equals the one put"
        );

        let context = toolset_context();
        store.put_context(&context).await.expect("the context is stored");
        assert_eq!(
            store.get_context("site").await.expect("the lookup succeeds"),
            Some(context),
            "the context read back equals the one put"
        );

        let toolset = valid_toolset();
        store.put_draft(&toolset).await.expect("a valid draft is stored");
        store.activate("k8s_ops").expect("the draft is activated");
        assert_eq!(
            store.get_active("k8s_ops").await.expect("the lookup succeeds"),
            Some(toolset),
            "the active toolset read back equals the draft put"
        );
    }

    // Scenario: a draft is invisible until activated
    #[tokio::test]
    async fn draft_is_invisible_until_activated() {
        let store = InMemoryToolsets::new(CAP);
        store
            .put_draft(&valid_toolset())
            .await
            .expect("a valid draft is stored");
        assert!(
            store
                .get_active("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a draft is not the active version"
        );
        store.activate("k8s_ops").expect("the draft is activated");
        assert!(
            store
                .get_active("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_some(),
            "the activated draft is the active version"
        );
    }

    /// A host that implements the three methods of [`ToolsetStore`] and nothing else. It keeps drafts and never serves an
    /// active version: if the trait required an activation, this host would not compile.
    #[derive(Debug, Default)]
    struct TraitOnlyHost {
        drafts: Mutex<BTreeMap<String, Toolset>>,
    }

    impl TraitOnlyHost {
        fn lookup(&self, name: &str) -> ToolsetsResult<Option<Toolset>> {
            let drafts = self.drafts.lock().map_err(|_poisoned| ToolsetsError::Poisoned)?;
            Ok(drafts.get(name).cloned())
        }

        fn store(&self, toolset: &Toolset) -> ToolsetsResult<()> {
            let _previous = self
                .drafts
                .lock()
                .map_err(|_poisoned| ToolsetsError::Poisoned)?
                .insert(toolset.name().to_owned(), toolset.clone());
            Ok(())
        }
    }

    impl ToolsetStore for TraitOnlyHost {
        fn get_active(&self, _name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send {
            std::future::ready(Ok(None))
        }

        fn get_draft(&self, name: &str) -> impl Future<Output = ToolsetsResult<Option<Toolset>>> + Send {
            std::future::ready(self.lookup(name))
        }

        fn put_draft(&self, toolset: &Toolset) -> impl Future<Output = ToolsetsResult<()>> + Send {
            std::future::ready(self.store(toolset))
        }
    }

    // Scenario: the trait cannot activate
    #[tokio::test]
    async fn trait_cannot_activate() {
        async fn read_both<S: ToolsetStore>(store: &S) -> (Option<Toolset>, Option<Toolset>) {
            let active = store.get_active("k8s_ops").await.expect("the lookup succeeds");
            let draft = store.get_draft("k8s_ops").await.expect("the lookup succeeds");
            (active, draft)
        }
        let host = TraitOnlyHost::default();
        host.put_draft(&valid_toolset())
            .await
            .expect("the host keeps the draft");
        let (active, draft) = read_both(&host).await;
        assert!(
            active.is_none(),
            "the trait offers no way to make the draft active"
        );
        assert!(draft.is_some(), "the draft is readable as a draft");
    }

    // Scenario: invalid draft refused
    #[tokio::test]
    async fn invalid_draft_refused() {
        let store = InMemoryToolsets::new(CAP);
        let broken = toolset_with("fn (", "body");
        let refused = store.put_draft(&broken).await;
        assert!(
            matches!(refused, Err(ToolsetsError::InvalidToolset { .. })),
            "a source that does not compile must be refused with InvalidToolset, got {refused:?}"
        );
        assert!(
            store
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a refused toolset is not stored"
        );
    }

    // No Scenario in store.sdd: the Must "put_draft valide ... plafonds des skills".
    #[tokio::test]
    async fn oversized_skill_refused_at_put_draft() {
        let store = InMemoryToolsets::new(10);
        let refused = store
            .put_draft(&toolset_with(
                "fn ping() { 1 }",
                "a body of more than ten characters",
            ))
            .await;
        assert!(
            matches!(refused, Err(ToolsetsError::SkillTooLarge { .. })),
            "a skill body above the render cap must be refused with SkillTooLarge, got {refused:?}"
        );
        assert!(
            store
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a refused toolset is not stored"
        );
    }

    // No Scenario in store.sdd: the Must "put_draft valide ... gabarits des skills compilables".
    #[tokio::test]
    async fn broken_skill_template_refused_at_put_draft() {
        let store = InMemoryToolsets::new(CAP);
        let refused = store
            .put_draft(&toolset_with("fn ping() { 1 }", "{{#if open}}never closed"))
            .await;
        assert!(
            matches!(refused, Err(ToolsetsError::Template { .. })),
            "a skill template that does not compile must be refused with Template, got {refused:?}"
        );
        assert!(
            store
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a refused toolset is not stored"
        );
    }

    // No Scenario in store.sdd: a second draft under the same name replaces the first one.
    #[tokio::test]
    async fn last_draft_replaces_the_earlier_one() {
        let store = InMemoryToolsets::new(CAP);
        store
            .put_draft(&valid_toolset())
            .await
            .expect("the first draft is stored");
        let newer = toolset_with("fn pong() { 2 }", "Newer body");
        store.put_draft(&newer).await.expect("the second draft is stored");
        assert_eq!(
            store.get_draft("k8s_ops").await.expect("the lookup succeeds"),
            Some(newer),
            "the draft read back is the last one put"
        );
    }

    /// A layered store: the top layer is the nearer level, the base layer the farther one. Both are in memory.
    fn layered() -> Layered<InMemoryToolsets, InMemoryToolsets> {
        Layered::new(InMemoryToolsets::new(CAP), InMemoryToolsets::new(CAP))
    }

    /// The valid toolset `k8s_ops` with two skills: `deploy` and `triage`, both with a body that compiles.
    fn toolset_with_two_skills(source: &str) -> Toolset {
        let triage = Skill::new(
            "triage".to_owned(),
            "Triage an incident".to_owned(),
            "Look at the pods first".to_owned(),
        )
        .expect("a valid skill");
        toolset_with(source, "Run the deploy steps")
            .with_skill(triage)
            .expect("a plain skill")
    }

    // Layered stores (`store.sdd` Must: un objet de la couche haute remplace entièrement celui de la base).
    #[tokio::test]
    async fn layered_active_top_replaces_base_whole() {
        let stack = layered();
        stack
            .base()
            .put_draft(&toolset_with_two_skills("fn ping() { 1 }"))
            .await
            .expect("the base draft is stored");
        stack
            .base()
            .activate("k8s_ops")
            .expect("the base draft is activated");
        let overriding = toolset_with("fn pong() { 2 }", "Top body");
        stack
            .top()
            .put_draft(&overriding)
            .await
            .expect("the top draft is stored");
        stack
            .top()
            .activate("k8s_ops")
            .expect("the top draft is activated");
        assert_eq!(
            stack.get_active("k8s_ops").await.expect("the lookup succeeds"),
            Some(overriding),
            "the top toolset is returned whole, without the skill that only the base has"
        );
    }

    #[tokio::test]
    async fn layered_active_falls_back_to_base() {
        let stack = layered();
        let base_toolset = valid_toolset();
        stack
            .base()
            .put_draft(&base_toolset)
            .await
            .expect("the base draft is stored");
        stack
            .base()
            .activate("k8s_ops")
            .expect("the base draft is activated");
        assert_eq!(
            stack.get_active("k8s_ops").await.expect("the lookup succeeds"),
            Some(base_toolset),
            "the base answers when the top layer has no active version"
        );
        assert!(
            stack
                .get_active("nobody")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "an unknown name gives nothing in either layer"
        );
    }

    #[tokio::test]
    async fn layered_draft_top_wins_then_falls_back() {
        let stack = layered();
        let base_draft = valid_toolset();
        stack
            .base()
            .put_draft(&base_draft)
            .await
            .expect("the base draft is stored");
        assert_eq!(
            stack.get_draft("k8s_ops").await.expect("the lookup succeeds"),
            Some(base_draft),
            "the base draft answers when the top layer has none"
        );
        let top_draft = toolset_with("fn pong() { 2 }", "Top body");
        stack
            .top()
            .put_draft(&top_draft)
            .await
            .expect("the top draft is stored");
        assert_eq!(
            stack.get_draft("k8s_ops").await.expect("the lookup succeeds"),
            Some(top_draft),
            "the top draft wins over the base draft"
        );
    }

    #[tokio::test]
    async fn layered_put_draft_writes_top_only() {
        let stack = layered();
        let toolset = valid_toolset();
        stack
            .put_draft(&toolset)
            .await
            .expect("the put reaches the top layer");
        assert_eq!(
            stack
                .top()
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds"),
            Some(toolset.clone()),
            "the top layer holds the put draft"
        );
        assert!(
            stack
                .base()
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "the base layer receives no put"
        );
        assert_eq!(
            stack.get_draft("k8s_ops").await.expect("the lookup succeeds"),
            Some(toolset),
            "the layered store reads back the put draft"
        );
    }

    #[tokio::test]
    async fn layered_draft_stays_invisible_until_top_activates() {
        let stack = layered();
        stack
            .put_draft(&valid_toolset())
            .await
            .expect("the draft is stored");
        assert!(
            stack
                .get_active("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a draft put through the layers is not the active version"
        );
        stack
            .top()
            .activate("k8s_ops")
            .expect("the host activates the draft in the top layer");
        assert!(
            stack
                .get_active("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_some(),
            "the activated draft of the top layer is the active version"
        );
    }

    #[tokio::test]
    async fn layered_put_draft_refuses_invalid_in_top() {
        let stack = layered();
        let refused = stack.put_draft(&toolset_with("fn (", "body")).await;
        assert!(
            matches!(refused, Err(ToolsetsError::InvalidToolset { .. })),
            "the top layer validates the draft, got {refused:?}"
        );
        assert!(
            stack
                .top()
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a refused draft is not stored in the top layer"
        );
    }

    #[tokio::test]
    async fn layered_types_cascade() {
        let stack = layered();
        let base_cluster = ContextType::new("cluster".to_owned(), semver("1.0.0"), json!({"type": "object"}))
            .expect("a valid context type");
        let top_cluster = ContextType::new(
            "cluster".to_owned(),
            semver("2.0.0"),
            json!({"type": "object", "properties": {"region": {"type": "string"}}}),
        )
        .expect("a valid context type");
        let base_only = ContextType::new("lab".to_owned(), semver("1.0.0"), json!({"type": "object"}))
            .expect("a valid context type");
        stack
            .base()
            .put_type(&base_cluster)
            .await
            .expect("the base type is stored");
        stack
            .base()
            .put_type(&base_only)
            .await
            .expect("the base type is stored");
        stack
            .top()
            .put_type(&top_cluster)
            .await
            .expect("the top type is stored");
        assert_eq!(
            stack.get_type("cluster").await.expect("the lookup succeeds"),
            Some(top_cluster),
            "the top type wins whole over the base type of the same name"
        );
        assert_eq!(
            stack.get_type("lab").await.expect("the lookup succeeds"),
            Some(base_only),
            "a type only the base has is answered by the base"
        );
        assert!(
            stack
                .get_type("nobody")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "an unknown type gives nothing"
        );
        let fresh = ContextType::new("fresh".to_owned(), semver("1.0.0"), json!({"type": "object"}))
            .expect("a valid context type");
        stack
            .put_type(&fresh)
            .await
            .expect("the put reaches the top layer");
        assert!(
            stack
                .base()
                .get_type("fresh")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "put_type writes to the top layer only"
        );
    }

    #[tokio::test]
    async fn layered_contexts_cascade_whole() {
        let stack = layered();
        let base_site = ToolsetContext::new("site".to_owned(), ContextLevel::User)
            .expect("a context name")
            .with_parameter("cluster".to_owned(), json!("prod"))
            .expect("a plain parameter")
            .with_parameter("region".to_owned(), json!("eu"))
            .expect("a plain parameter");
        let top_site = ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter("cluster".to_owned(), json!("staging"))
            .expect("a plain parameter");
        let base_only = ToolsetContext::new("lab".to_owned(), ContextLevel::User).expect("a context name");
        stack
            .base()
            .put_context(&base_site)
            .await
            .expect("the base context is stored");
        stack
            .base()
            .put_context(&base_only)
            .await
            .expect("the base context is stored");
        stack
            .top()
            .put_context(&top_site)
            .await
            .expect("the top context is stored");
        assert_eq!(
            stack.get_context("site").await.expect("the lookup succeeds"),
            Some(top_site),
            "the top context wins whole: the base parameter `region` does not come through"
        );
        assert_eq!(
            stack.get_context("lab").await.expect("the lookup succeeds"),
            Some(base_only),
            "a context only the base has is answered by the base"
        );
        let fresh = ToolsetContext::new("fresh".to_owned(), ContextLevel::Session).expect("a context name");
        stack
            .put_context(&fresh)
            .await
            .expect("the put reaches the top layer");
        assert!(
            stack
                .base()
                .get_context("fresh")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "put_context writes to the top layer only"
        );
    }

    // Task #4 of store.sdd: a `{:?}` of the in-memory store names the stored objects and never shows the text of a module.
    #[tokio::test]
    async fn debug_elides_the_module_source() {
        let store = InMemoryToolsets::new(CAP);
        let secret = "sk-FAKE-SECRET-1234";
        store
            .put_draft(&toolset_with(
                &format!("let key = \"{secret}\";\nfn ping() {{ 1 }}"),
                "Body",
            ))
            .await
            .expect("the draft is stored");
        store.activate("k8s_ops").expect("the draft is activated");
        let shown = format!("{store:?}");
        assert!(
            shown.contains("k8s_ops"),
            "the name of the stored toolset is shown: {shown}"
        );
        assert!(
            !shown.contains(secret),
            "the text of the module never appears in a debug output: {shown}"
        );
    }

    // Task #6 of store.sdd: a skill whose description does not compile is refused by put_draft, as a template error.
    #[tokio::test]
    async fn broken_skill_description_refused_at_put_draft() {
        let store = InMemoryToolsets::new(CAP);
        let skill = Skill::new(
            "deploy".to_owned(),
            "{{#if open}}description never closed".to_owned(),
            "Run the deploy steps".to_owned(),
        )
        .expect("a skill with a non-blank description builds");
        let toolset = Toolset::new(
            "k8s_ops".to_owned(),
            "Kubernetes operations".to_owned(),
            semver("1.0.0"),
            "fn ping() { 1 }".to_owned(),
        )
        .expect("a valid toolset")
        .with_skill(skill)
        .expect("a plain skill");
        let refused = store.put_draft(&toolset).await;
        assert!(
            matches!(refused, Err(ToolsetsError::Template { .. })),
            "a skill description that does not compile must be refused with Template, got {refused:?}"
        );
        assert!(
            store
                .get_draft("k8s_ops")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a refused toolset is not stored"
        );
    }

    // Task #6 of store.sdd: the error of validate_draft names the toolset, never the source.
    #[tokio::test]
    async fn invalid_draft_error_names_the_toolset() {
        let store = InMemoryToolsets::new(CAP);
        let refused = store.put_draft(&toolset_with("fn (", "body")).await;
        assert!(
            matches!(&refused, Err(ToolsetsError::InvalidToolset { name, .. }) if name == "k8s_ops"),
            "the refusal must name the toolset k8s_ops, got {refused:?}"
        );
    }

    // Task #6 of store.sdd: activate without a draft gives InvalidToolset, never a panic.
    #[test]
    fn activate_without_draft_is_invalid_toolset() {
        let store = InMemoryToolsets::new(CAP);
        let refused = store.activate("k8s_ops");
        assert!(
            matches!(&refused, Err(ToolsetsError::InvalidToolset { name, .. }) if name == "k8s_ops"),
            "activating a name with no draft must fail with InvalidToolset naming it, got {refused:?}"
        );
    }

    /// Poisons `lock` by panicking while its guard is held, as a thread that fails in the middle of a write would.
    fn poison<T>(lock: &Mutex<T>) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock.lock();
            panic!("poisoning the lock on purpose");
        }));
        assert!(outcome.is_err(), "the closure must panic to poison the lock");
    }

    // Task #6 of store.sdd: a poisoned lock is reported as Poisoned by every operation, and Debug still prints.
    #[tokio::test]
    async fn poisoned_lock_is_reported_by_every_operation() {
        let store = InMemoryToolsets::new(CAP);
        store
            .put_draft(&valid_toolset())
            .await
            .expect("the draft is stored before the lock is poisoned");
        poison(&store.drafts);
        poison(&store.active);
        poison(&store.types);
        poison(&store.contexts);

        assert!(
            matches!(store.get_active("k8s_ops").await, Err(ToolsetsError::Poisoned)),
            "get_active must report Poisoned"
        );
        assert!(
            matches!(store.get_draft("k8s_ops").await, Err(ToolsetsError::Poisoned)),
            "get_draft must report Poisoned"
        );
        assert!(
            matches!(
                store.put_draft(&valid_toolset()).await,
                Err(ToolsetsError::Poisoned)
            ),
            "put_draft must report Poisoned"
        );
        assert!(
            matches!(store.activate("k8s_ops"), Err(ToolsetsError::Poisoned)),
            "activate must report Poisoned"
        );
        assert!(
            matches!(store.get_type("cluster").await, Err(ToolsetsError::Poisoned)),
            "get_type must report Poisoned"
        );
        assert!(
            matches!(
                store.put_type(&context_type()).await,
                Err(ToolsetsError::Poisoned)
            ),
            "put_type must report Poisoned"
        );
        assert!(
            matches!(store.get_context("site").await, Err(ToolsetsError::Poisoned)),
            "get_context must report Poisoned"
        );
        assert!(
            matches!(
                store.put_context(&toolset_context()).await,
                Err(ToolsetsError::Poisoned)
            ),
            "put_context must report Poisoned"
        );
        let shown = format!("{store:?}");
        assert!(
            shown.contains("poisoned lock"),
            "the debug output must show the poisoned maps instead of failing: {shown}"
        );
    }

    // Task #6 of store.sdd: the Debug output lists the names of the drafts, not only of the active toolsets.
    #[tokio::test]
    async fn debug_names_the_drafts() {
        let store = InMemoryToolsets::new(CAP);
        store
            .put_draft(&valid_toolset())
            .await
            .expect("the draft is stored");
        let shown = format!("{store:?}");
        assert!(
            shown.contains("drafts: Ok([\"k8s_ops\"])"),
            "the drafts field must name the stored draft k8s_ops: {shown}"
        );
    }
}
