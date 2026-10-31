//! Structural compatibility of a toolset context with the context types that the toolsets of an agent require, and the
//! rules of agent definitions that concern toolsets (`agent-sessions/toolsets#FR-4`, `FR-8`, `agent-sessions#DEC-11`,
//! `AC-3`, `AC-4`, `AC-8`). The check is structural: the context satisfies a type when the flattened required parameters
//! are present and the required roles are bound, with no declaration of conformity.

use std::fmt;

use serde_json::Value;

use crate::agents::{AgentDefinition, AgentKind};

use super::{
    ContextType, ResolvedContext, Toolset, ToolsetContext, ToolsetsError, ToolsetsResult, TypeRequirement,
};

/// Rule of the toolset-creation module: it is reserved to a configuration agent, whatever the other definitions say.
const CREATION_NATURE_RULE: &str = "the toolset-creation module is reserved to a configuration agent (nature Config): \
     a work agent or a sub-agent cannot hold it";

/// One gap between what a toolset requires and what the toolset context provides.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Gap {
    toolset: String,
    context_type: Option<String>,
    kind: GapKind,
}

/// Nature of a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GapKind {
    /// The agent names a toolset that is not available.
    UnknownToolset,
    /// A context type required by the toolset is not found.
    UnknownType,
    /// The context type is in a version that does not satisfy the requirement.
    VersionTooOld {
        /// Minimal version of the requirement, as `major.minor.patch`.
        required: String,
        /// Version of the context type found, as `major.minor.patch`.
        found: String,
    },
    /// A required parameter of a context type is absent from the context.
    MissingParameter {
        /// Name of the missing parameter.
        name: String,
    },
    /// A required role is not bound in the context.
    MissingRole {
        /// Name of the missing role.
        name: String,
    },
    /// A value of the context breaks a rule of the parameter schema of the context type. Only the parameter and the rule
    /// are named, never the value.
    InvalidValue {
        /// Name of the parameter concerned, `*` for a key that no schema declares, or `None` when the rule concerns the set
        /// of parameters as a whole.
        parameter: Option<String>,
        /// Keyword of the violated rule, such as `type` or `enum`.
        rule: String,
    },
}

impl Gap {
    /// Returns the toolset that requires what is missing, or the name of the unknown toolset.
    #[must_use]
    pub fn toolset(&self) -> &str {
        &self.toolset
    }

    /// Returns the context type concerned, or `None` for a role that the toolset declares for itself.
    #[must_use]
    pub fn context_type(&self) -> Option<&str> {
        self.context_type.as_deref()
    }

    /// Returns the nature of the gap.
    #[must_use]
    pub const fn kind(&self) -> &GapKind {
        &self.kind
    }
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toolset = self.toolset.as_str();
        let context_type = self.context_type.as_deref().unwrap_or("");
        match &self.kind {
            GapKind::UnknownToolset => write!(f, "toolset `{toolset}` is not available"),
            GapKind::UnknownType => write!(
                f,
                "toolset `{toolset}` requires context type `{context_type}`, which is not found"
            ),
            GapKind::VersionTooOld { required, found } => write!(
                f,
                "toolset `{toolset}` requires context type `{context_type}` in {required} or later, found {found}"
            ),
            GapKind::MissingParameter { name } => write!(
                f,
                "toolset `{toolset}` requires parameter `{name}` of context type `{context_type}`"
            ),
            GapKind::MissingRole { name } => match &self.context_type {
                Some(found) => write!(
                    f,
                    "toolset `{toolset}` requires role `{name}` of context type `{found}`"
                ),
                None => write!(f, "toolset `{toolset}` requires its own role `{name}`"),
            },
            GapKind::InvalidValue {
                parameter: Some(parameter),
                rule,
            } => write!(
                f,
                "toolset `{toolset}` requires parameter `{parameter}` of context type `{context_type}` to satisfy rule `{rule}`"
            ),
            GapKind::InvalidValue {
                parameter: None,
                rule,
            } => write!(
                f,
                "toolset `{toolset}` requires the parameters of context type `{context_type}` to satisfy rule `{rule}`"
            ),
        }
    }
}

/// Why a sub-agent was removed from the delegable list.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemovalReason {
    /// The main agent lists the sub-agent, but no definition is found for it.
    DefinitionMissing,
    /// A toolset of the sub-agent is not available.
    ToolsetMissing,
    /// The toolsets of the sub-agent are not compatible with the context. The list holds the gaps; it is empty when the
    /// check itself failed because a required context type is inconsistent (fail closed).
    Incompatible(Vec<Gap>),
}

/// A sub-agent removed from the delegable list, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemovedSubagent {
    id: String,
    reason: RemovalReason,
}

impl RemovedSubagent {
    /// Returns the identifier of the removed sub-agent.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns why the sub-agent was removed.
    #[must_use]
    pub const fn reason(&self) -> &RemovalReason {
        &self.reason
    }
}

/// Result of [`delegable_subagents`]: the sub-agents that can be delegated to, and the ones that were removed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Delegation {
    kept: Vec<String>,
    removed: Vec<RemovedSubagent>,
}

impl Delegation {
    /// Returns the identifiers of the compatible sub-agents, in the order of the main definition.
    #[must_use]
    pub fn kept(&self) -> &[String] {
        &self.kept
    }

    /// Returns the removed sub-agents with their reason, in the order of the main definition.
    #[must_use]
    pub fn removed(&self) -> &[RemovedSubagent] {
        &self.removed
    }
}

/// Returns every gap of a toolset context against the types and roles that the toolsets require.
///
/// The check is structural. The list is empty when the context is compatible.
///
/// # Errors
///
/// Returns the error of [`ContextType::flatten`] when a required type is itself inconsistent.
pub fn check_context<F>(
    context: &ToolsetContext,
    toolsets: &[Toolset],
    lookup: &F,
) -> ToolsetsResult<Vec<Gap>>
where
    F: Fn(&str) -> Option<ContextType>,
{
    gaps_of(context, toolsets, lookup)
}

/// Gaps of the toolsets `toolsets` against `context`, in the order of the toolsets.
///
/// The context is read through its resolved copy: its parameters and the names of its bound roles.
fn gaps_of<'items, I, F>(context: &ToolsetContext, toolsets: I, lookup: &F) -> ToolsetsResult<Vec<Gap>>
where
    I: IntoIterator<Item = &'items Toolset>,
    F: Fn(&str) -> Option<ContextType>,
{
    let resolved = context.resolve(Vec::<(String, String, Value)>::new())?;
    let mut gaps = Vec::new();
    for toolset in toolsets {
        for (type_name, requirement) in toolset.type_requirements() {
            gaps.extend(type_gaps(toolset, type_name, requirement, &resolved, lookup)?);
        }
        gaps.extend(own_role_gaps(toolset, &resolved));
    }
    Ok(gaps)
}

/// Gaps of one requirement of a toolset on a context type: the type is found, its version satisfies the requirement, its
/// flattened required parameters and roles are present in the context, and every value present satisfies the schemas of
/// the type and of the types it includes.
fn type_gaps<F>(
    toolset: &Toolset,
    type_name: &str,
    requirement: &TypeRequirement,
    resolved: &ResolvedContext,
    lookup: &F,
) -> ToolsetsResult<Vec<Gap>>
where
    F: Fn(&str) -> Option<ContextType>,
{
    let gap = |kind: GapKind| Gap {
        toolset: toolset.name().to_owned(),
        context_type: Some(type_name.to_owned()),
        kind,
    };
    let Some(context_type) = lookup(type_name) else {
        return Ok(vec![gap(GapKind::UnknownType)]);
    };
    let mut gaps = Vec::new();
    if !requirement.is_satisfied_by(context_type.version()) {
        gaps.push(gap(GapKind::VersionTooOld {
            required: requirement.minimum().version.to_string(),
            found: context_type.version().version.to_string(),
        }));
    }
    let flat = context_type.flatten(lookup)?;
    gaps.extend(
        flat.required_parameters()
            .iter()
            .filter(|parameter| !resolved.shared().contains_key(*parameter))
            .map(|parameter| {
                gap(GapKind::MissingParameter {
                    name: parameter.clone(),
                })
            }),
    );
    gaps.extend(
        flat.required_roles()
            .iter()
            .filter(|role| !resolved.roles().contains(role))
            .map(|role| gap(GapKind::MissingRole { name: role.clone() })),
    );
    gaps.extend(
        flat.violations(type_name, resolved.shared())?
            .into_iter()
            .map(|violation| {
                gap(GapKind::InvalidValue {
                    parameter: violation.parameter().map(str::to_owned),
                    rule: violation.rule().to_owned(),
                })
            }),
    );
    Ok(gaps)
}

/// Gaps of the roles that a toolset requires for itself and that the context does not bind.
fn own_role_gaps(toolset: &Toolset, resolved: &ResolvedContext) -> Vec<Gap> {
    toolset
        .roles()
        .values()
        .filter(|role| role.required() && !resolved.roles().iter().any(|bound| bound == role.name()))
        .map(|role| Gap {
            toolset: toolset.name().to_owned(),
            context_type: None,
            kind: GapKind::MissingRole {
                name: role.name().to_owned(),
            },
        })
        .collect()
}

/// Splits the names of toolsets into the toolsets found in `table` and the names that are not found.
fn select<'table>(names: &[String], table: &'table [Toolset]) -> (Vec<&'table Toolset>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        match table.iter().find(|toolset| toolset.name() == name.as_str()) {
            Some(toolset) => found.push(toolset),
            None => missing.push(name.clone()),
        }
    }
    (found, missing)
}

/// Computes the delegable sub-agents of a work agent (`Work`) in this session, in the order of its definition.
///
/// A work agent whose toolsets have gaps against `context` fails with [`ToolsetsError::Incompatible`]. A sub-agent with a
/// gap, a toolset that is not available, or no definition in `subagents`, is removed from the list without error and
/// reported in [`Delegation::removed`] with its [`RemovalReason`].
///
/// # Errors
///
/// Returns [`ToolsetsError::Incompatible`] for the work agent, carrying the gaps as texts.
pub fn delegable_subagents<F>(
    main: &AgentDefinition,
    subagents: &[AgentDefinition],
    toolsets: &[Toolset],
    context: &ToolsetContext,
    lookup: &F,
) -> ToolsetsResult<Delegation>
where
    F: Fn(&str) -> Option<ContextType>,
{
    let (found, missing) = select(main.toolsets(), toolsets);
    let mut issues: Vec<Gap> = missing
        .into_iter()
        .map(|name| Gap {
            toolset: name,
            context_type: None,
            kind: GapKind::UnknownToolset,
        })
        .collect();
    issues.extend(gaps_of(context, found, lookup)?);
    if !issues.is_empty() {
        return Err(ToolsetsError::Incompatible {
            issues: issues.iter().map(ToString::to_string).collect(),
        });
    }
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    for id in main.delegable() {
        let removal = |reason: RemovalReason| RemovedSubagent {
            id: id.clone(),
            reason,
        };
        // A sub-agent with no definition, an unavailable toolset, or any gap is removed without error, with its reason.
        let Some(sub) = subagents.iter().find(|candidate| candidate.id() == id.as_str()) else {
            removed.push(removal(RemovalReason::DefinitionMissing));
            continue;
        };
        let (sub_found, sub_missing) = select(sub.toolsets(), toolsets);
        if !sub_missing.is_empty() {
            removed.push(removal(RemovalReason::ToolsetMissing));
            continue;
        }
        match gaps_of(context, sub_found, lookup) {
            Ok(gaps) if gaps.is_empty() => kept.push(id.clone()),
            Ok(gaps) => removed.push(removal(RemovalReason::Incompatible(gaps))),
            // Fail closed: an inconsistent type removes the sub-agent, with no gap to name.
            Err(_) => removed.push(removal(RemovalReason::Incompatible(Vec::new()))),
        }
    }
    Ok(Delegation { kept, removed })
}

/// Checks the rule of the toolset-creation module: only a configuration agent may hold it, among `agent` and `subagents`.
///
/// # Errors
///
/// Returns [`ToolsetsError::Forbidden`] when the agent, or one of `subagents`, is granted `creation_module` against the
/// rule. The message names the rule.
pub fn check_creation_rule(
    agent: &AgentDefinition,
    subagents: &[AgentDefinition],
    creation_module: &str,
) -> ToolsetsResult<()> {
    let grants = |definition: &AgentDefinition| {
        definition
            .modules()
            .iter()
            .any(|module| module.as_str() == creation_module)
    };
    // Only a configuration agent may hold the module: the nature decides, not the delegation.
    let refused = |definition: &AgentDefinition| grants(definition) && definition.kind() != AgentKind::Config;
    if refused(agent) || subagents.iter().any(refused) {
        return Err(ToolsetsError::Forbidden {
            rule: CREATION_NATURE_RULE,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};
    use vynil_core::Semver;

    use super::{GapKind, RemovalReason, check_context, check_creation_rule, delegable_subagents};
    use crate::agents::{AgentDefinition, AgentKind};
    use crate::toolsets::{
        ContextLevel, ContextType, RoleDecl, RoleSpec, Toolset, ToolsetContext, ToolsetsError,
        TypeRequirement,
    };

    const CREATION_MODULE: &str = "toolset_authoring";

    fn version(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    fn cluster_schema() -> Value {
        json!({
            "type": "object",
            "required": ["namespace"],
            "properties": { "namespace": { "type": "string" } }
        })
    }

    /// A `cluster` type in `text`, requiring the parameter `namespace` and the role `k8s`.
    fn cluster(text: &str) -> ContextType {
        ContextType::new("cluster".to_owned(), version(text), cluster_schema())
            .expect("the sample type builds")
            .with_role(RoleSpec::new("k8s".to_owned(), true))
            .expect("the role is added")
    }

    /// A lookup over the given types, keyed by the name each type was built with.
    fn lookup_of(types: Vec<(&str, ContextType)>) -> impl Fn(&str) -> Option<ContextType> {
        let table: BTreeMap<String, ContextType> = types
            .into_iter()
            .map(|(name, found)| (name.to_owned(), found))
            .collect();
        move |name: &str| table.get(name).cloned()
    }

    /// A context that satisfies `cluster`: the parameter `namespace` and the role `k8s`.
    fn complete_context() -> ToolsetContext {
        ToolsetContext::new("prod".to_owned(), ContextLevel::Project)
            .expect("the context builds")
            .with_parameter("namespace".to_owned(), json!("apps"))
            .expect("the parameter is added")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("the role is bound")
    }

    fn empty_context() -> ToolsetContext {
        ToolsetContext::new("empty".to_owned(), ContextLevel::Session).expect("the context builds")
    }

    fn grafana_requiring_cluster(text: &str) -> Toolset {
        Toolset::new(
            "grafana".to_owned(),
            "Grafana queries".to_owned(),
            version("1.0.0"),
            "fn query() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_type_requirement(TypeRequirement::parse(text).expect("the requirement parses"))
        .expect("the requirement is added")
    }

    fn main_agent(delegable: Vec<String>) -> AgentDefinition {
        AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Work,
            "You are the main agent.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_delegable(delegable)
    }

    fn sub_agent(id: &str, toolsets: Vec<String>) -> AgentDefinition {
        AgentDefinition::new(
            id.to_owned(),
            AgentKind::Sub,
            "You are a sub-agent.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_toolsets(toolsets)
    }

    // Scenario: un sous-agent retiré est signalé avec sa raison
    #[test]
    fn a_removed_sub_agent_is_reported_with_its_reason() {
        let role_toolset = Toolset::new(
            "k8s_tools".to_owned(),
            "Kubernetes tools".to_owned(),
            version("1.0.0"),
            "fn pods() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_role(RoleDecl::new("k8s".to_owned(), true))
        .expect("the role is declared");
        let main = main_agent(vec![
            "lost".to_owned(),
            "ghost".to_owned(),
            "needy".to_owned(),
            "plain".to_owned(),
        ]);
        let subagents = vec![
            sub_agent("ghost", vec!["not-a-toolset".to_owned()]),
            sub_agent("needy", vec!["k8s_tools".to_owned()]),
            sub_agent("plain", Vec::new()),
        ];
        let toolsets = vec![role_toolset];
        let delegation = delegable_subagents(
            &main,
            &subagents,
            &toolsets,
            &empty_context(),
            &lookup_of(Vec::new()),
        )
        .expect("a removed sub-agent raises no error");
        assert_eq!(
            delegation.kept(),
            ["plain".to_owned()],
            "the compatible sub-agent is kept"
        );
        let removed = delegation.removed();
        assert_eq!(removed.len(), 3, "the three removals are listed: {removed:?}");
        let reason_of = |id: &str| {
            removed
                .iter()
                .find(|entry| entry.id() == id)
                .map(|entry| entry.reason().clone())
        };
        assert_eq!(reason_of("lost"), Some(RemovalReason::DefinitionMissing));
        assert_eq!(reason_of("ghost"), Some(RemovalReason::ToolsetMissing));
        match reason_of("needy") {
            Some(RemovalReason::Incompatible(gaps)) => assert!(
                gaps.iter().any(|gap| gap.kind()
                    == &GapKind::MissingRole {
                        name: "k8s".to_owned()
                    }),
                "the gaps name the missing role: {gaps:?}"
            ),
            other => panic!("expected Incompatible, got {other:?}"),
        }
        assert_eq!(
            removed.iter().map(super::RemovedSubagent::id).collect::<Vec<_>>(),
            ["lost", "ghost", "needy"],
            "the removals follow the order of the definition"
        );
    }

    // Scenario: un rôle manquant retire un sous-agent
    #[test]
    fn missing_role_removes_a_sub_agent() {
        let role_toolset = Toolset::new(
            "k8s_tools".to_owned(),
            "Kubernetes tools".to_owned(),
            version("1.0.0"),
            "fn pods() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_role(RoleDecl::new("k8s".to_owned(), true))
        .expect("the role is declared");
        let main = main_agent(vec!["worker".to_owned()]);
        let subagents = vec![sub_agent("worker", vec!["k8s_tools".to_owned()])];
        let lookup = lookup_of(vec![("cluster", cluster("1.0.0"))]);
        let toolsets = vec![role_toolset];

        let with_role = ToolsetContext::new("with".to_owned(), ContextLevel::Session)
            .expect("the context builds")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("the role is bound");
        let with_role_result = delegable_subagents(&main, &subagents, &toolsets, &with_role, &lookup);
        assert_eq!(
            with_role_result.expect("the context has the role").kept(),
            ["worker".to_owned()],
            "the sub-agent is listed when the role is bound"
        );

        let without_role = delegable_subagents(&main, &subagents, &toolsets, &empty_context(), &lookup);
        assert_eq!(
            without_role.expect("a sub-agent gap raises no error").kept(),
            Vec::<String>::new(),
            "without the role the sub-agent is absent and no error is raised"
        );
    }

    // Scenario: l'agent principal ne démarre pas
    #[test]
    fn main_agent_fails_to_start_with_every_gap() {
        let main = main_agent(Vec::new()).with_toolsets(vec!["grafana".to_owned()]);
        let toolsets = vec![grafana_requiring_cluster("cluster@1.0")];
        let lookup = lookup_of(vec![("cluster", cluster("1.0.0"))]);
        let result = delegable_subagents(&main, &[], &toolsets, &empty_context(), &lookup);
        match result {
            Err(ToolsetsError::Incompatible { issues }) => {
                assert_eq!(
                    issues.len(),
                    2,
                    "the parameter and the role are both listed: {issues:?}"
                );
                assert!(
                    issues.iter().any(|issue| issue.contains("namespace")),
                    "{issues:?}"
                );
                assert!(issues.iter().any(|issue| issue.contains("k8s")), "{issues:?}");
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn check_context_lists_each_missing_parameter_and_role() {
        let toolsets = vec![grafana_requiring_cluster("cluster@1.0")];
        let lookup = lookup_of(vec![("cluster", cluster("1.0.0"))]);
        let gaps = check_context(&empty_context(), &toolsets, &lookup).expect("the types are consistent");
        assert_eq!(gaps.len(), 2, "one gap per missing parameter and role: {gaps:?}");
        assert!(
            gaps.iter().any(|gap| gap.kind()
                == &GapKind::MissingParameter {
                    name: "namespace".to_owned()
                }),
            "the missing parameter is a gap: {gaps:?}"
        );
        assert!(
            gaps.iter().any(|gap| gap.kind()
                == &GapKind::MissingRole {
                    name: "k8s".to_owned()
                }),
            "the missing role is a gap: {gaps:?}"
        );
    }

    #[test]
    fn complete_context_has_no_gap() {
        let toolsets = vec![grafana_requiring_cluster("cluster@1.2")];
        let lookup = lookup_of(vec![("cluster", cluster("1.3.0"))]);
        let gaps = check_context(&complete_context(), &toolsets, &lookup).expect("the types are consistent");
        assert!(
            gaps.is_empty(),
            "a context that satisfies the types has no gap: {gaps:?}"
        );
    }

    // Scenario: une version de type incompatible est un écart
    #[test]
    fn incompatible_type_version_is_a_gap() {
        let toolsets = vec![grafana_requiring_cluster("cluster@1.2")];
        let lookup = lookup_of(vec![("cluster", cluster("1.1.0"))]);
        let gaps = check_context(&complete_context(), &toolsets, &lookup).expect("the types are consistent");
        assert_eq!(gaps.len(), 1, "the version is the only gap: {gaps:?}");
        match gaps.first().map(|gap| gap.kind().clone()) {
            Some(GapKind::VersionTooOld { required, found }) => {
                assert_eq!(found, "1.1.0", "the gap names the version found");
                assert_eq!(required, "1.2.0", "the gap names the required version");
            }
            other => panic!("expected VersionTooOld, got {other:?}"),
        }
    }

    #[test]
    fn unknown_type_is_a_gap() {
        let toolsets = vec![grafana_requiring_cluster("cluster@1.2")];
        let lookup = lookup_of(Vec::new());
        let gaps = check_context(&complete_context(), &toolsets, &lookup).expect("the lookup is consistent");
        assert!(
            gaps.iter().any(|gap| gap.kind() == &GapKind::UnknownType),
            "a type that is not found is a gap: {gaps:?}"
        );
    }

    #[test]
    fn toolset_own_required_role_counts_as_a_type_role() {
        let toolset = Toolset::new(
            "k8s_tools".to_owned(),
            "Kubernetes tools".to_owned(),
            version("1.0.0"),
            "fn pods() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_role(RoleDecl::new("registry".to_owned(), true))
        .expect("the role is declared");
        let gaps =
            check_context(&empty_context(), &[toolset], &lookup_of(Vec::new())).expect("no type to flatten");
        assert!(
            gaps.iter().any(|gap| gap.kind()
                == &GapKind::MissingRole {
                    name: "registry".to_owned()
                }),
            "a required role of the toolset itself is a gap when unbound: {gaps:?}"
        );
    }

    // Scenario: module de création refusé pour un sous-agent
    #[test]
    fn creation_module_refused_for_a_sub_agent() {
        let sub = AgentDefinition::new(
            "author".to_owned(),
            AgentKind::Sub,
            "You write toolsets.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_modules(vec![CREATION_MODULE.to_owned()]);
        let result = check_creation_rule(&sub, &[], CREATION_MODULE);
        match result {
            Err(ToolsetsError::Forbidden { rule }) => {
                assert!(
                    rule.contains("configuration agent"),
                    "the message names the nature rule: {rule}"
                );
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    // Scenario: creation module refused with delegable sub-agents
    // Obsolete wording (compat.sdd, 2026-10-29): the rule now depends on the nature `Work`, not on delegation. This
    // test keeps the Scenario's setup (a work agent with delegable sub-agents) and checks the nature rule.
    #[test]
    fn creation_module_refused_with_delegable_sub_agents() {
        let main = main_agent(vec!["worker".to_owned()]).with_modules(vec![CREATION_MODULE.to_owned()]);
        let subagents = vec![sub_agent("worker", Vec::new())];
        let result = check_creation_rule(&main, &subagents, CREATION_MODULE);
        match result {
            Err(ToolsetsError::Forbidden { rule }) => {
                assert!(
                    rule.contains("configuration agent"),
                    "the message names the nature rule: {rule}"
                );
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn creation_module_refused_for_a_work_agent_without_delegation() {
        let work = main_agent(Vec::new()).with_modules(vec![CREATION_MODULE.to_owned()]);
        let result = check_creation_rule(&work, &[], CREATION_MODULE);
        assert!(
            matches!(&result, Err(ToolsetsError::Forbidden { rule }) if rule.contains("configuration agent")),
            "a work agent is refused by its nature, delegation aside, got {result:?}"
        );
    }

    #[test]
    fn creation_module_granted_to_a_configuration_agent_is_accepted() {
        let config = AgentDefinition::new(
            "setup".to_owned(),
            AgentKind::Config,
            "You create toolsets.".to_owned(),
            "strix-qwen".to_owned(),
        )
        .with_modules(vec![CREATION_MODULE.to_owned()]);
        assert!(
            check_creation_rule(&config, &[], CREATION_MODULE).is_ok(),
            "a configuration agent may hold the creation module"
        );
    }

    #[test]
    fn delegable_order_follows_the_definition() {
        let main = main_agent(vec!["beta".to_owned(), "alpha".to_owned()]);
        let subagents = vec![sub_agent("alpha", Vec::new()), sub_agent("beta", Vec::new())];
        let delegable = delegable_subagents(&main, &subagents, &[], &empty_context(), &lookup_of(Vec::new()));
        assert_eq!(
            delegable.expect("no gap").kept(),
            ["beta".to_owned(), "alpha".to_owned()],
            "the sub-agents keep the order of the main definition"
        );
    }

    #[test]
    fn sub_agent_with_unknown_toolset_or_without_definition_is_removed() {
        let main = main_agent(vec!["ghost".to_owned(), "lost".to_owned(), "plain".to_owned()]);
        let subagents = vec![
            sub_agent("ghost", vec!["not-a-toolset".to_owned()]),
            sub_agent("plain", Vec::new()),
        ];
        let delegable = delegable_subagents(&main, &subagents, &[], &empty_context(), &lookup_of(Vec::new()));
        assert_eq!(
            delegable.expect("removal is silent").kept(),
            ["plain".to_owned()],
            "a sub-agent with an unknown toolset and a delegable id without definition are removed; a valid one stays"
        );
    }

    #[test]
    fn main_agent_with_unknown_toolset_fails() {
        let main = main_agent(Vec::new()).with_toolsets(vec!["missing".to_owned()]);
        let result = delegable_subagents(&main, &[], &[], &empty_context(), &lookup_of(Vec::new()));
        assert!(
            matches!(result, Err(ToolsetsError::Incompatible { .. })),
            "a main agent naming an unavailable toolset fails to start"
        );
    }

    // Task #4 of compat.sdd: a sub-agent whose type is inconsistent (inclusion cycle) fails closed: it is removed from the
    // delegable list, without error.
    #[test]
    fn sub_agent_with_an_inconsistent_type_is_removed_without_error() {
        let first = ContextType::new("first".to_owned(), version("1.0.0"), json!({ "type": "object" }))
            .expect("the sample type builds")
            .with_include(TypeRequirement::parse("second@1.0").expect("the requirement parses"));
        let second = ContextType::new("second".to_owned(), version("1.0.0"), json!({ "type": "object" }))
            .expect("the sample type builds")
            .with_include(TypeRequirement::parse("first@1.0").expect("the requirement parses"));
        let lookup = lookup_of(vec![("first", first), ("second", second)]);
        let toolsets = vec![grafana_requiring_cluster("first@1.0")];
        let main = main_agent(vec!["worker".to_owned()]);
        let subagents = vec![sub_agent("worker", vec!["grafana".to_owned()])];
        let result = delegable_subagents(&main, &subagents, &toolsets, &empty_context(), &lookup);
        assert_eq!(
            result
                .as_ref()
                .expect("a sub-agent with a cyclic type raises no error")
                .kept(),
            Vec::<String>::new(),
            "the sub-agent with an inconsistent type is absent"
        );
        let delegation = result.expect("a sub-agent with a cyclic type raises no error");
        let removed = delegation.removed();
        assert_eq!(removed.len(), 1, "exactly one sub-agent is reported removed");
        assert_eq!(removed.first().map(super::RemovedSubagent::id), Some("worker"));
        assert_eq!(
            removed.first().map(super::RemovedSubagent::reason),
            Some(&RemovalReason::Incompatible(Vec::new())),
            "the removal is signalled as incompatible, without gaps"
        );
    }

    // Task #4 of compat.sdd: the table of sub-agents is checked even when the main agent does not hold the module.
    #[test]
    fn creation_module_refused_when_a_listed_sub_agent_holds_it() {
        let main = main_agent(vec!["worker".to_owned()]);
        let subagents = vec![sub_agent("worker", Vec::new()).with_modules(vec![CREATION_MODULE.to_owned()])];
        let result = check_creation_rule(&main, &subagents, CREATION_MODULE);
        match result {
            Err(ToolsetsError::Forbidden { rule }) => {
                assert!(
                    rule.contains("configuration agent"),
                    "the message names the nature rule: {rule}"
                );
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    // Task #4 of compat.sdd: an optional role of the toolset itself, left unbound, is not a gap.
    #[test]
    fn optional_own_role_unbound_is_not_a_gap() {
        let toolset = Toolset::new(
            "k8s_tools".to_owned(),
            "Kubernetes tools".to_owned(),
            version("1.0.0"),
            "fn pods() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_role(RoleDecl::new("registry".to_owned(), false))
        .expect("the role is declared");
        let gaps =
            check_context(&empty_context(), &[toolset], &lookup_of(Vec::new())).expect("no type to flatten");
        assert!(gaps.is_empty(), "an optional unbound role is no gap: {gaps:?}");
    }

    /// A context bound to the role `k8s`, with the one parameter given.
    fn context_with(name: &str, value: Value) -> ToolsetContext {
        ToolsetContext::new("ctx".to_owned(), ContextLevel::Session)
            .expect("the context builds")
            .with_parameter(name.to_owned(), value)
            .expect("the parameter is added")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("the role is bound")
    }

    /// The gaps of `context` against a type named `cluster` built from `schema`, as texts.
    fn texts_against(schema: Value, context: &ToolsetContext) -> Vec<String> {
        let typed =
            ContextType::new("cluster".to_owned(), version("1.0.0"), schema).expect("the sample type builds");
        let lookup = lookup_of(vec![("cluster", typed)]);
        let gaps = check_context(context, &[grafana_requiring_cluster("cluster@1.0")], &lookup)
            .expect("the types are consistent");
        gaps.iter().map(ToString::to_string).collect()
    }

    // Scenario: valeur de mauvais type est un écart
    #[test]
    fn value_of_the_wrong_type_is_a_gap() {
        let toolsets = vec![grafana_requiring_cluster("cluster@1.0")];
        let lookup = lookup_of(vec![("cluster", cluster("1.0.0"))]);
        let context = context_with("namespace", json!(["valeur-fautive-42"]));
        let gaps = check_context(&context, &toolsets, &lookup).expect("the types are consistent");
        let texts: Vec<String> = gaps.iter().map(ToString::to_string).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("namespace") && text.contains("`type`")),
            "the wrong type is a gap naming the parameter and the rule: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("valeur-fautive-42")),
            "no gap carries the value: {texts:?}"
        );
        assert!(
            !format!("{gaps:?}").contains("valeur-fautive-42"),
            "the debug form carries no value either: {gaps:?}"
        );
    }

    // Scenario: valeur hors énumération est un écart
    #[test]
    fn value_outside_an_enumeration_is_a_gap() {
        let schema = json!({
            "type": "object",
            "properties": { "env": { "type": "string", "enum": ["dev", "prod"] } }
        });
        let texts = texts_against(schema, &context_with("env", json!("staging-valeur")));
        assert!(
            texts
                .iter()
                .any(|text| text.contains("env") && text.contains("`enum`")),
            "the value outside the enumeration is a gap: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("staging-valeur")),
            "the value never comes out: {texts:?}"
        );
    }

    // Scenario: paramètre optionnel validé comme les autres
    #[test]
    fn optional_parameter_is_validated_too() {
        let schema = json!({ "type": "object", "properties": { "zone": { "type": "integer" } } });
        let texts = texts_against(schema, &context_with("zone", json!("eu")));
        assert!(
            texts
                .iter()
                .any(|text| text.contains("zone") && text.contains("`type`")),
            "an optional parameter with a wrong value is a gap: {texts:?}"
        );
    }

    // Scenario: propriété non déclarée interdite, nom masqué
    #[test]
    fn undeclared_property_refused_without_its_name() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "additionalProperties": false
        });
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::Session)
            .expect("the context builds")
            .with_parameter("a".to_owned(), json!("ok"))
            .expect("the parameter is added")
            .with_parameter("leaked-key".to_owned(), json!("leaked-valeur-7"))
            .expect("the parameter is added");
        let texts = texts_against(schema, &context);
        assert!(
            texts.iter().any(|text| text.contains("`additionalProperties`")),
            "the forbidden extra property is a gap: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("leaked-key") && !text.contains("leaked-valeur-7")),
            "neither the undeclared name nor its value comes out: {texts:?}"
        );
    }

    // Scenario: valeur fautive ressemblant à un secret jamais rendue
    #[test]
    fn secret_value_never_comes_out_of_a_gap() {
        let schema = json!({ "type": "object", "properties": { "api_token": { "type": "integer" } } });
        let context = context_with("api_token", json!("p4ss-reelle-XYZ"));
        let texts = texts_against(schema, &context);
        assert!(
            texts
                .iter()
                .any(|text| text.contains("api_token") && text.contains("`type`")),
            "the gap names the parameter and the rule: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.contains("p4ss-reelle-XYZ")),
            "the secret value never comes out: {texts:?}"
        );
    }

    // Scenario: valeur requise absente, un seul écart
    #[test]
    fn absent_required_value_is_a_single_gap() {
        let toolsets = vec![grafana_requiring_cluster("cluster@1.0")];
        let lookup = lookup_of(vec![("cluster", cluster("1.0.0"))]);
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::Session)
            .expect("the context builds")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("the role is bound");
        let gaps = check_context(&context, &toolsets, &lookup).expect("the types are consistent");
        assert_eq!(
            gaps.len(),
            1,
            "the absent parameter is one gap, with no required-rule duplicate: {gaps:?}"
        );
        assert!(
            gaps.first()
                .is_some_and(|gap| gap.to_string().contains("requires parameter `namespace`")),
            "the single gap names the missing parameter: {gaps:?}"
        );
    }

    // Scenario: sous-agent dont une valeur viole le schéma est retiré sans erreur
    #[test]
    fn sub_agent_with_a_value_breaking_its_schema_is_removed_without_error() {
        let main = main_agent(vec!["worker".to_owned()]);
        let subagents = vec![sub_agent("worker", vec!["grafana".to_owned()])];
        let toolsets = vec![grafana_requiring_cluster("cluster@1.0")];
        let lookup = lookup_of(vec![("cluster", cluster("1.0.0"))]);
        let context = context_with("namespace", json!(["valeur-fautive-42"]));
        let delegation = delegable_subagents(&main, &subagents, &toolsets, &context, &lookup)
            .expect("a removal raises no error");
        assert!(
            delegation.kept().is_empty(),
            "the sub-agent is not delegable: {delegation:?}"
        );
        match delegation
            .removed()
            .first()
            .map(|removal| removal.reason().clone())
        {
            Some(RemovalReason::Incompatible(gaps)) => assert!(
                gaps.iter().any(|gap| gap.to_string().contains("namespace")),
                "the removal names the gap of the value: {gaps:?}"
            ),
            other => panic!("expected Incompatible with a value gap, got {other:?}"),
        }
    }
}
