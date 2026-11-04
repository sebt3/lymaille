//! Start point of a session, read from the type of its toolset context: the main agent that the type names by default and
//! the nature of the session, both checked against the catalogue of agents (`agent-sessions/toolsets#FR-21`, `AC-16`).

use crate::agents::{AgentDefinition, AgentKind, AgentStore};
use crate::session::SessionKind;

use super::{ContextType, ToolsetsError, ToolsetsResult};

/// The start point of a session: the main agent read from the catalogue, and the nature of the session.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct StartTarget {
    agent: AgentDefinition,
    kind: SessionKind,
}

impl StartTarget {
    /// Returns the definition of the agent, read from the catalogue under the name the type carries.
    #[must_use]
    pub const fn agent(&self) -> &AgentDefinition {
        &self.agent
    }

    /// Returns the nature of the session: the one the type sets, or else the one that follows from the kind of the agent.
    #[must_use]
    pub const fn kind(&self) -> SessionKind {
        self.kind
    }
}

/// Resolves the start point of a session from the type of its toolset context.
///
/// A type without a default agent gives nothing, and the catalogue is not read. Otherwise the agent is read once, under
/// the name the type carries, and checked: it must be a work or a configuration agent, and the nature the type sets, when
/// it sets one, must match the kind of the agent. No other input or output is performed.
///
/// # Errors
///
/// Returns [`ToolsetsError::InvalidContextType`], naming the type, when the default agent is absent from the catalogue,
/// when it is a sub-agent, or when the nature the type sets does not match the kind of the agent. Returns
/// [`ToolsetsError::Store`] with the text of the host error when the catalogue fails.
pub async fn resolve_start<A: AgentStore>(
    context_type: &ContextType,
    agents: &A,
) -> ToolsetsResult<Option<StartTarget>> {
    let Some(name) = context_type.default_agent() else {
        return Ok(None);
    };
    let definition = agents
        .get_agent(name)
        .await
        .map_err(|error| ToolsetsError::Store(error.to_string()))?
        .ok_or_else(|| refusal(context_type.name(), "the default agent is not in the catalogue"))?;
    let kind = match definition.kind() {
        AgentKind::Work => SessionKind::Work,
        AgentKind::Config => SessionKind::Config,
        AgentKind::Sub => {
            return Err(refusal(
                context_type.name(),
                "the default agent is a sub-agent, bound to the session that delegates to it",
            ));
        }
    };
    if let Some(declared) = context_type.session_kind()
        && declared != kind
    {
        return Err(refusal(
            context_type.name(),
            "the session kind of the type does not match the kind of the default agent",
        ));
    }
    let session_kind = context_type.session_kind().unwrap_or(kind);
    Ok(Some(StartTarget {
        agent: definition,
        kind: session_kind,
    }))
}

/// Builds the refusal of a start point, naming the type and giving the fixed reason.
fn refusal(name: &str, reason: &'static str) -> ToolsetsError {
    ToolsetsError::InvalidContextType {
        name: name.to_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use vynil_core::Semver;

    use super::{StartTarget, resolve_start};
    use crate::agents::{AgentDefinition, AgentKind, AgentStore, AgentsError, AgentsResult};
    use crate::session::SessionKind;
    use crate::toolsets::{ContextType, ToolsetsError};

    /// A catalogue that holds the given definitions and counts its reads.
    struct CountingCatalog {
        agents: Vec<AgentDefinition>,
        reads: AtomicUsize,
    }

    impl CountingCatalog {
        fn new(agents: Vec<AgentDefinition>) -> Self {
            Self {
                agents,
                reads: AtomicUsize::new(0),
            }
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl AgentStore for CountingCatalog {
        fn get_agent(&self, id: &str) -> impl Future<Output = AgentsResult<Option<AgentDefinition>>> + Send {
            let _previous = self.reads.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(self.agents.iter().find(|agent| agent.id() == id).cloned()))
        }

        fn put_agent(&self, _definition: &AgentDefinition) -> impl Future<Output = AgentsResult<()>> + Send {
            std::future::ready(Ok(()))
        }
    }

    /// A catalogue whose lookups always fail, as a host storage that is down would.
    struct FailingCatalog;

    impl AgentStore for FailingCatalog {
        fn get_agent(&self, _id: &str) -> impl Future<Output = AgentsResult<Option<AgentDefinition>>> + Send {
            std::future::ready(Err(AgentsError::Store("the host storage is down".to_owned())))
        }

        fn put_agent(&self, _definition: &AgentDefinition) -> impl Future<Output = AgentsResult<()>> + Send {
            std::future::ready(Ok(()))
        }
    }

    fn semver(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    /// A type `cluster` with the given default agent and session nature, when they are given.
    fn cluster(agent: Option<&str>, kind: Option<SessionKind>) -> ContextType {
        let mut built = ContextType::new("cluster".to_owned(), semver("1.0.0"), json!({ "type": "object" }))
            .expect("the sample type must build");
        if let Some(name) = agent {
            built = built
                .with_default_agent(name.to_owned())
                .expect("the sample agent must be accepted");
        }
        if let Some(kind) = kind {
            built = built
                .with_session_kind(kind)
                .expect("the sample session kind must be accepted");
        }
        built
    }

    fn agent(id: &str, kind: AgentKind) -> AgentDefinition {
        AgentDefinition::new(
            id.to_owned(),
            kind,
            "You are the assistant.".to_owned(),
            "strix-qwen".to_owned(),
        )
    }

    // Scenario: un type sans agent par défaut ne lit pas le catalogue
    #[tokio::test]
    async fn a_type_without_default_agent_does_not_read_the_catalogue() {
        let catalog = CountingCatalog::new(vec![agent("ops", AgentKind::Config)]);
        for typed in [cluster(None, None), cluster(None, Some(SessionKind::Config))] {
            let resolved = resolve_start(&typed, &catalog)
                .await
                .expect("a type without agent resolves");
            assert_eq!(resolved, None, "nothing is returned without a default agent");
        }
        assert_eq!(
            catalog.reads(),
            0,
            "the catalogue is not read without a default agent"
        );
    }

    // Scenario: l'agent et la nature sont rendus
    #[tokio::test]
    async fn the_agent_and_the_nature_are_returned() {
        let ops = agent("ops", AgentKind::Config);
        let catalog = CountingCatalog::new(vec![ops.clone()]);
        let typed = cluster(Some("ops"), Some(SessionKind::Config));
        let target: StartTarget = resolve_start(&typed, &catalog)
            .await
            .expect("the default agent resolves")
            .expect("a type with a default agent returns a target");
        assert_eq!(
            target.agent(),
            &ops,
            "the agent is the definition read under the name"
        );
        assert_eq!(
            target.kind(),
            SessionKind::Config,
            "the nature is the one the type sets"
        );
    }

    // Scenario: la nature se déduit du genre de l'agent
    #[tokio::test]
    async fn the_nature_is_deduced_from_the_kind_of_the_agent() {
        let catalog = CountingCatalog::new(vec![
            agent("dev", AgentKind::Work),
            agent("ops", AgentKind::Config),
        ]);
        let work = resolve_start(&cluster(Some("dev"), None), &catalog)
            .await
            .expect("the work agent resolves")
            .expect("a target is returned");
        assert_eq!(
            work.kind(),
            SessionKind::Work,
            "a work agent gives a work session"
        );
        let config = resolve_start(&cluster(Some("ops"), None), &catalog)
            .await
            .expect("the configuration agent resolves")
            .expect("a target is returned");
        assert_eq!(
            config.kind(),
            SessionKind::Config,
            "a configuration agent gives a configuration session"
        );
    }

    // Scenario: un agent inconnu est refusé
    #[tokio::test]
    async fn an_unknown_agent_is_refused() {
        let catalog = CountingCatalog::new(vec![agent("ops", AgentKind::Config)]);
        match resolve_start(&cluster(Some("ghost"), None), &catalog).await {
            Err(ToolsetsError::InvalidContextType { name, .. }) => {
                assert_eq!(name, "cluster", "the refusal names the type");
            }
            other => panic!("an unknown agent must be refused, got {other:?}"),
        }
    }

    // Scenario: un sous-agent est refusé
    #[tokio::test]
    async fn a_sub_agent_is_refused() {
        let catalog = CountingCatalog::new(vec![agent("helper", AgentKind::Sub)]);
        let refused = resolve_start(&cluster(Some("helper"), None), &catalog).await;
        assert!(
            matches!(refused, Err(ToolsetsError::InvalidContextType { .. })),
            "a sub-agent is refused as a default agent, got {refused:?}"
        );
    }

    // Scenario: une nature contradictoire est refusée
    #[tokio::test]
    async fn a_contradictory_nature_is_refused() {
        let catalog = CountingCatalog::new(vec![
            agent("ops", AgentKind::Config),
            agent("dev", AgentKind::Work),
        ]);
        let work_on_config = resolve_start(&cluster(Some("ops"), Some(SessionKind::Work)), &catalog).await;
        assert!(
            matches!(work_on_config, Err(ToolsetsError::InvalidContextType { .. })),
            "a work nature on a configuration agent is refused, got {work_on_config:?}"
        );
        let config_on_work = resolve_start(&cluster(Some("dev"), Some(SessionKind::Config)), &catalog).await;
        assert!(
            matches!(config_on_work, Err(ToolsetsError::InvalidContextType { .. })),
            "a configuration nature on a work agent is refused, got {config_on_work:?}"
        );
    }

    // Scenario: l'échec du catalogue est rendu tel quel
    #[tokio::test]
    async fn a_failing_catalogue_is_reported_as_is() {
        let refused = resolve_start(&cluster(Some("ops"), None), &FailingCatalog).await;
        match refused {
            Err(ToolsetsError::Store(text)) => {
                assert!(
                    text.contains("the host storage is down"),
                    "the host error text is carried: {text}"
                );
            }
            other => panic!("a failing catalogue must give a store error, got {other:?}"),
        }
    }

    #[test]
    fn the_resolution_future_is_send() {
        fn assert_send<T: Send>(_future: &T) {}
        let typed = cluster(Some("ops"), None);
        let catalog = CountingCatalog::new(vec![agent("ops", AgentKind::Config)]);
        assert_send(&resolve_start(&typed, &catalog));
    }
}
