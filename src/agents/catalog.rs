//! Storage traits of the catalog (agents, providers, model profiles) that the host implements, and an in-memory store for
//! tests (`agent-sessions#CON-1`, `agent-sessions/agents#FR-4`, `FR-6`, `FR-7`).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    AgentDefinition, AgentsError, AgentsResult, DetectedModel, ModelProfile, Provider, ResolvedProfile,
};

/// Storage of agent definitions, implemented by the host. Lookups and stores are asynchronous; the returned futures are
/// `Send`. The trait is used through a generic parameter only, never as a trait object.
///
/// The trait has no listing and no removal: they belong to the host.
pub trait AgentStore: Send + Sync {
    /// Reads the definition of the agent `id`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::Store`] when the host storage fails.
    fn get_agent(&self, id: &str) -> impl Future<Output = AgentsResult<Option<AgentDefinition>>> + Send;

    /// Stores `definition` under its identifier, replacing any earlier definition with the same identifier.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::InvalidDefinition`] when the definition is refused by
    /// [`AgentDefinition::validate`], and [`AgentsError::Store`] when the host storage fails.
    fn put_agent(&self, definition: &AgentDefinition) -> impl Future<Output = AgentsResult<()>> + Send;
}

/// Storage of model profiles, implemented by the host.
///
/// The trait is used through a generic parameter only, never as a trait object.
pub trait ProfileStore: Send + Sync {
    /// Reads the profile named `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::Store`] when the host storage fails.
    fn get_profile(&self, name: &str) -> impl Future<Output = AgentsResult<Option<ModelProfile>>> + Send;

    /// Stores `profile` under its name, replacing any earlier profile with the same name.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::Store`] when the host storage fails.
    fn put_profile(&self, profile: &ModelProfile) -> impl Future<Output = AgentsResult<()>> + Send;

    /// Resolves the profile named `name` against what the provider detects. The provider is not contacted: the detection
    /// is given by the caller.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::UnknownProfile`] when no profile has this name, [`AgentsError::ContextSizeUnknown`] when
    /// neither the profile nor the detection gives the context size, and [`AgentsError::Store`] when the host storage
    /// fails.
    fn resolve_profile(
        &self,
        name: &str,
        detected: Option<DetectedModel>,
    ) -> impl Future<Output = AgentsResult<ResolvedProfile>> + Send {
        async move {
            let profile = self
                .get_profile(name)
                .await?
                .ok_or_else(|| AgentsError::UnknownProfile {
                    name: name.to_owned(),
                })?;
            profile.resolve(detected)
        }
    }
}

/// Storage of inference providers, implemented by the host.
///
/// The trait is used through a generic parameter only, never as a trait object.
pub trait ProviderStore: Send + Sync {
    /// Reads the provider named `name`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::Store`] when the host storage fails.
    fn get_provider(&self, name: &str) -> impl Future<Output = AgentsResult<Option<Provider>>> + Send;

    /// Stores `provider` under its name, replacing any earlier provider with the same name.
    ///
    /// # Errors
    ///
    /// Returns [`AgentsError::Store`] when the host storage fails.
    fn put_provider(&self, provider: &Provider) -> impl Future<Output = AgentsResult<()>> + Send;
}

/// In-memory catalog. It keeps the JSON text of each object, never the live object, so a value read back goes through the
/// same format as a value read from a real storage. It performs no I/O.
///
/// [`AgentStore::put_agent`] validates the definition before storing it. A poisoned lock is recovered: each entry is
/// written whole, so the maps stay consistent.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct InMemoryCatalog {
    agents: Mutex<BTreeMap<String, String>>,
    profiles: Mutex<BTreeMap<String, String>>,
    providers: Mutex<BTreeMap<String, String>>,
}

impl InMemoryCatalog {
    /// Builds an empty catalog.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            agents: Mutex::new(BTreeMap::new()),
            profiles: Mutex::new(BTreeMap::new()),
            providers: Mutex::new(BTreeMap::new()),
        }
    }

    /// Locks one of the maps of JSON texts. A poisoned lock is recovered: every entry is written whole.
    fn lock(map: &Mutex<BTreeMap<String, String>>) -> MutexGuard<'_, BTreeMap<String, String>> {
        map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads the object whose JSON text is stored under `key`, or `None` when there is none.
    fn read<T: DeserializeOwned>(
        map: &Mutex<BTreeMap<String, String>>,
        key: &str,
    ) -> AgentsResult<Option<T>> {
        let text = Self::lock(map).get(key).cloned();
        text.map(|text| serde_json::from_str(&text).map_err(|error| AgentsError::Store(error.to_string())))
            .transpose()
    }

    /// Stores the JSON text of `value` under `key`, replacing the previous text.
    fn write<T: Serialize>(map: &Mutex<BTreeMap<String, String>>, key: &str, value: &T) -> AgentsResult<()> {
        let text = serde_json::to_string(value).map_err(|error| AgentsError::Store(error.to_string()))?;
        let _replaced = Self::lock(map).insert(key.to_owned(), text);
        Ok(())
    }
}

impl AgentStore for InMemoryCatalog {
    fn get_agent(&self, id: &str) -> impl Future<Output = AgentsResult<Option<AgentDefinition>>> + Send {
        std::future::ready(Self::read(&self.agents, id))
    }

    fn put_agent(&self, definition: &AgentDefinition) -> impl Future<Output = AgentsResult<()>> + Send {
        std::future::ready(
            definition
                .validate()
                .and_then(|()| Self::write(&self.agents, definition.id(), definition)),
        )
    }
}

impl ProfileStore for InMemoryCatalog {
    fn get_profile(&self, name: &str) -> impl Future<Output = AgentsResult<Option<ModelProfile>>> + Send {
        std::future::ready(Self::read(&self.profiles, name))
    }

    fn put_profile(&self, profile: &ModelProfile) -> impl Future<Output = AgentsResult<()>> + Send {
        std::future::ready(Self::write(&self.profiles, profile.name(), profile))
    }
}

impl ProviderStore for InMemoryCatalog {
    fn get_provider(&self, name: &str) -> impl Future<Output = AgentsResult<Option<Provider>>> + Send {
        std::future::ready(Self::read(&self.providers, name))
    }

    fn put_provider(&self, provider: &Provider) -> impl Future<Output = AgentsResult<()>> + Send {
        std::future::ready(Self::write(&self.providers, provider.name(), provider))
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentStore as _, InMemoryCatalog, ProfileStore as _, ProviderStore as _};
    use crate::agents::{
        AgentDefinition, AgentKind, AgentsError, DetectedModel, ModelProfile, Provider, ProviderLevel,
    };

    fn agent() -> AgentDefinition {
        AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Main,
            "You are the assistant.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_toolsets(vec!["k8s".to_owned()])
    }

    fn profile(name: &str) -> ModelProfile {
        ModelProfile::new(
            name.to_owned(),
            "vllm-strix".to_owned(),
            "qwen3.8-flash-next".to_owned(),
        )
    }

    fn provider() -> Provider {
        Provider::new(
            "vllm-strix".to_owned(),
            "openai".to_owned(),
            "vllm-strix-addr".to_owned(),
            Some("vllm-key".to_owned()),
            ProviderLevel::User,
        )
        .expect("plain references make a provider")
    }

    #[tokio::test]
    async fn put_then_get_restores_each_entity() {
        let catalog = InMemoryCatalog::new();
        let definition = agent();
        catalog
            .put_agent(&definition)
            .await
            .expect("a valid agent must be stored");
        let found_agent = catalog.get_agent("main").await.expect("the lookup must succeed");
        assert_eq!(
            found_agent,
            Some(definition),
            "the agent read back must equal the one put"
        );

        let stored_profile = profile("strix-qwen");
        catalog
            .put_profile(&stored_profile)
            .await
            .expect("the profile must be stored");
        let found_profile = catalog
            .get_profile("strix-qwen")
            .await
            .expect("the lookup must succeed");
        assert_eq!(
            found_profile,
            Some(stored_profile),
            "the profile read back must equal the one put"
        );

        let stored_provider = provider();
        catalog
            .put_provider(&stored_provider)
            .await
            .expect("the provider must be stored");
        let found_provider = catalog
            .get_provider("vllm-strix")
            .await
            .expect("the lookup must succeed");
        assert_eq!(
            found_provider,
            Some(stored_provider),
            "the provider read back must equal the one put"
        );
    }

    #[tokio::test]
    async fn unknown_names_give_nothing() {
        let catalog = InMemoryCatalog::new();
        assert!(
            matches!(catalog.get_agent("nobody").await, Ok(None)),
            "an unknown agent must give nothing and no error"
        );
        assert!(
            matches!(catalog.get_profile("nobody").await, Ok(None)),
            "an unknown profile must give nothing and no error"
        );
        assert!(
            matches!(catalog.get_provider("nobody").await, Ok(None)),
            "an unknown provider must give nothing and no error"
        );
    }

    #[tokio::test]
    async fn put_replaces() {
        let catalog = InMemoryCatalog::new();
        let first = profile("strix-qwen").with_context_size(8192);
        let second = profile("strix-qwen").with_context_size(262_144);
        catalog
            .put_profile(&first)
            .await
            .expect("the first profile must be stored");
        catalog
            .put_profile(&second)
            .await
            .expect("the second profile must be stored");
        let found = catalog
            .get_profile("strix-qwen")
            .await
            .expect("the lookup must succeed");
        assert_eq!(
            found,
            Some(second),
            "the last profile put under a name must replace the earlier one"
        );
    }

    #[tokio::test]
    async fn invalid_agent_refused_at_put() {
        let catalog = InMemoryCatalog::new();
        let sub = AgentDefinition::new(
            "reviewer".to_owned(),
            AgentKind::Sub,
            "Review the change.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_delegable(vec!["other".to_owned()]);
        let refused = catalog.put_agent(&sub).await;
        assert!(
            matches!(refused, Err(AgentsError::InvalidDefinition { .. })),
            "a sub-agent declaring delegable sub-agents must be refused with InvalidDefinition, got {refused:?}"
        );
        let stored = catalog
            .get_agent("reviewer")
            .await
            .expect("the lookup must succeed");
        assert!(
            stored.is_none(),
            "a refused agent must not be stored, got {stored:?}"
        );
    }

    #[tokio::test]
    async fn resolve_through_the_store() {
        let catalog = InMemoryCatalog::new();
        catalog
            .put_profile(&profile("strix-qwen").with_context_size(262_144))
            .await
            .expect("the profile must be stored");
        let detected = DetectedModel::new(Some(8192), None, None);
        let resolved = catalog
            .resolve_profile("strix-qwen", Some(detected))
            .await
            .expect("the stored profile must resolve");
        assert_eq!(
            resolved.context_size(),
            262_144,
            "the stored override must win over the detected 8192"
        );
        let unknown = catalog.resolve_profile("missing", Some(detected)).await;
        assert!(
            matches!(&unknown, Err(AgentsError::UnknownProfile { name }) if name == "missing"),
            "an unknown profile name must fail with UnknownProfile naming it, got {unknown:?}"
        );
    }
}
