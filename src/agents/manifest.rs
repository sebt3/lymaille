//! Project manifest: the serializable list of the named entities a project groups, and the check of its names against
//! what the host stores (`agent-sessions/agents#FR-4`). It holds names only: no definition, no address, no secret. The
//! library reads and writes no file; the host stores the manifest as it wants.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::secret::{DefaultSecretDetector, SecretDetector as _};

/// Message of the refusal of a manifest read from its serialized form. It names no value.
const REFUSED: &str = "invalid manifest: a name is empty or looks like a secret";

/// Category of a main agent, as a gap names it.
const MAIN_AGENT: &str = "main_agent";
/// Category of a sub-agent, as a gap names it.
const SUBAGENT: &str = "subagent";
/// Category of a toolset, as a gap names it.
const TOOLSET: &str = "toolset";
/// Category of a context type, as a gap names it.
const CONTEXT_TYPE: &str = "context_type";
/// Category of a context, as a gap names it.
const CONTEXT: &str = "context";
/// Category of a provider, as a gap names it.
const PROVIDER: &str = "provider";
/// Category of a model profile, as a gap names it.
const PROFILE: &str = "profile";

/// Serializable list of the named entities of a project. Each category holds sorted names without duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ProjectManifest {
    name: String,
    main_agents: BTreeSet<String>,
    subagents: BTreeSet<String>,
    toolsets: BTreeSet<String>,
    context_types: BTreeSet<String>,
    contexts: BTreeSet<String>,
    providers: BTreeSet<String>,
    profiles: BTreeSet<String>,
}

/// View of what the host stores, given to [`ProjectManifest::check`]: one list of names per category.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManifestView {
    /// Names of the main agents found in the store.
    pub main_agents: Vec<String>,
    /// Names of the sub-agents found in the store.
    pub subagents: Vec<String>,
    /// Names of the toolsets found in the store.
    pub toolsets: Vec<String>,
    /// Names of the context types found in the store.
    pub context_types: Vec<String>,
    /// Names of the contexts found in the store.
    pub contexts: Vec<String>,
    /// Names of the providers found in the store.
    pub providers: Vec<String>,
    /// Names of the model profiles found in the store.
    pub profiles: Vec<String>,
}

/// An entity of the manifest that the store does not hold. It names the category and the entity, never a content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManifestGap {
    category: &'static str,
    name: String,
}

impl ProjectManifest {
    /// Builds an empty manifest named `name`. Returns nothing when the name is blank or looks like a secret.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        is_name(name).then(|| Self {
            name: name.to_owned(),
            main_agents: BTreeSet::new(),
            subagents: BTreeSet::new(),
            toolsets: BTreeSet::new(),
            context_types: BTreeSet::new(),
            contexts: BTreeSet::new(),
            providers: BTreeSet::new(),
            profiles: BTreeSet::new(),
        })
    }

    /// Adds a main agent. Returns nothing when the name is blank or looks like a secret; a duplicate is ignored.
    #[must_use]
    pub fn with_main_agent(mut self, name: &str) -> Option<Self> {
        add(&mut self.main_agents, name)?;
        Some(self)
    }

    /// Adds a sub-agent, with the same rules as [`ProjectManifest::with_main_agent`].
    #[must_use]
    pub fn with_subagent(mut self, name: &str) -> Option<Self> {
        add(&mut self.subagents, name)?;
        Some(self)
    }

    /// Adds a toolset, with the same rules as [`ProjectManifest::with_main_agent`].
    #[must_use]
    pub fn with_toolset(mut self, name: &str) -> Option<Self> {
        add(&mut self.toolsets, name)?;
        Some(self)
    }

    /// Adds a context type, with the same rules as [`ProjectManifest::with_main_agent`].
    #[must_use]
    pub fn with_context_type(mut self, name: &str) -> Option<Self> {
        add(&mut self.context_types, name)?;
        Some(self)
    }

    /// Adds a context, with the same rules as [`ProjectManifest::with_main_agent`].
    #[must_use]
    pub fn with_context(mut self, name: &str) -> Option<Self> {
        add(&mut self.contexts, name)?;
        Some(self)
    }

    /// Adds a provider, with the same rules as [`ProjectManifest::with_main_agent`].
    #[must_use]
    pub fn with_provider(mut self, name: &str) -> Option<Self> {
        add(&mut self.providers, name)?;
        Some(self)
    }

    /// Adds a model profile, with the same rules as [`ProjectManifest::with_main_agent`].
    #[must_use]
    pub fn with_profile(mut self, name: &str) -> Option<Self> {
        add(&mut self.profiles, name)?;
        Some(self)
    }

    /// Returns the name of the project.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the main agents, sorted.
    #[must_use]
    pub fn main_agents(&self) -> Vec<&str> {
        sorted(&self.main_agents)
    }

    /// Returns the sub-agents, sorted.
    #[must_use]
    pub fn subagents(&self) -> Vec<&str> {
        sorted(&self.subagents)
    }

    /// Returns the toolsets, sorted.
    #[must_use]
    pub fn toolsets(&self) -> Vec<&str> {
        sorted(&self.toolsets)
    }

    /// Returns the context types, sorted.
    #[must_use]
    pub fn context_types(&self) -> Vec<&str> {
        sorted(&self.context_types)
    }

    /// Returns the contexts, sorted.
    #[must_use]
    pub fn contexts(&self) -> Vec<&str> {
        sorted(&self.contexts)
    }

    /// Returns the providers, sorted.
    #[must_use]
    pub fn providers(&self) -> Vec<&str> {
        sorted(&self.providers)
    }

    /// Returns the model profiles, sorted.
    #[must_use]
    pub fn profiles(&self) -> Vec<&str> {
        sorted(&self.profiles)
    }

    /// Lists the entities of the manifest that `view` does not hold. Empty when the manifest is consistent with the store.
    #[must_use]
    pub fn check(&self, view: &ManifestView) -> Vec<ManifestGap> {
        let mut gaps = Vec::new();
        push_gaps(&mut gaps, MAIN_AGENT, &self.main_agents, &view.main_agents);
        push_gaps(&mut gaps, SUBAGENT, &self.subagents, &view.subagents);
        push_gaps(&mut gaps, TOOLSET, &self.toolsets, &view.toolsets);
        push_gaps(&mut gaps, CONTEXT_TYPE, &self.context_types, &view.context_types);
        push_gaps(&mut gaps, CONTEXT, &self.contexts, &view.contexts);
        push_gaps(&mut gaps, PROVIDER, &self.providers, &view.providers);
        push_gaps(&mut gaps, PROFILE, &self.profiles, &view.profiles);
        gaps
    }
}

impl ManifestGap {
    /// Returns the category of the missing entity, for example `toolset`.
    #[must_use]
    pub const fn category(&self) -> &'static str {
        self.category
    }

    /// Returns the name of the missing entity.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for ManifestGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "missing from the store: {} `{}`", self.category, self.name)
    }
}

/// Serialized form of a manifest, before the refusals of the constructors apply.
#[derive(Deserialize)]
struct Record {
    name: String,
    #[serde(default)]
    main_agents: Vec<String>,
    #[serde(default)]
    subagents: Vec<String>,
    #[serde(default)]
    toolsets: Vec<String>,
    #[serde(default)]
    context_types: Vec<String>,
    #[serde(default)]
    contexts: Vec<String>,
    #[serde(default)]
    providers: Vec<String>,
    #[serde(default)]
    profiles: Vec<String>,
}

impl<'de> Deserialize<'de> for ProjectManifest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = Record::deserialize(deserializer)?;
        from_record(&record).ok_or_else(|| <D::Error as serde::de::Error>::custom(REFUSED))
    }
}

/// Appends to `gaps` one gap per name of `declared` that `stored` does not hold, in the category `category`.
fn push_gaps(
    gaps: &mut Vec<ManifestGap>,
    category: &'static str,
    declared: &BTreeSet<String>,
    stored: &[String],
) {
    gaps.extend(
        declared
            .iter()
            .filter(|name| !stored.contains(name))
            .map(|name| ManifestGap {
                category,
                name: name.to_owned(),
            }),
    );
}

/// Rebuilds a manifest from its serialized form, applying the same refusals as the constructors.
fn from_record(record: &Record) -> Option<ProjectManifest> {
    let mut manifest = ProjectManifest::new(&record.name)?;
    add_all(&mut manifest.main_agents, &record.main_agents)?;
    add_all(&mut manifest.subagents, &record.subagents)?;
    add_all(&mut manifest.toolsets, &record.toolsets)?;
    add_all(&mut manifest.context_types, &record.context_types)?;
    add_all(&mut manifest.contexts, &record.contexts)?;
    add_all(&mut manifest.providers, &record.providers)?;
    add_all(&mut manifest.profiles, &record.profiles)?;
    Some(manifest)
}

/// Adds every name of `values` to `names`, or refuses the whole list at the first refused name.
fn add_all(names: &mut BTreeSet<String>, values: &[String]) -> Option<()> {
    values.iter().try_for_each(|value| add(names, value))
}

/// Adds `name` to `names` when it is accepted. Returns nothing when it is refused.
fn add(names: &mut BTreeSet<String>, name: &str) -> Option<()> {
    is_name(name).then(|| names.extend([name.to_owned()]))
}

/// Tells whether `text` may be a name of the manifest: not blank, and not looking like a secret for the default detector.
/// The detector judges the trimmed text, so that a leading blank cannot hide a prefix.
fn is_name(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && !DefaultSecretDetector.looks_like_secret(trimmed)
}

/// Returns the names of a set, in their sorted order.
fn sorted(names: &BTreeSet<String>) -> Vec<&str> {
    names.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::{ManifestView, ProjectManifest};

    // Scenario: un manifeste cohérent n'a aucun écart
    #[test]
    fn consistent_manifest_has_no_gap() {
        let manifest = ProjectManifest::new("lymaille-demo")
            .and_then(|m| m.with_main_agent("planner"))
            .and_then(|m| m.with_toolset("rhai-ops"))
            .and_then(|m| m.with_context("session-ctx"))
            .expect("plain names build a manifest");
        let view = ManifestView {
            main_agents: vec!["planner".to_owned()],
            toolsets: vec!["rhai-ops".to_owned()],
            contexts: vec!["session-ctx".to_owned()],
            ..ManifestView::default()
        };
        assert!(
            manifest.check(&view).is_empty(),
            "a manifest whose names are all stored must have no gap"
        );
    }

    // Scenario: une entité absente est un écart nommé
    #[test]
    fn missing_entity_is_a_named_gap() {
        let manifest = ProjectManifest::new("lymaille-demo")
            .and_then(|m| m.with_toolset("fs-tools"))
            .expect("plain names build a manifest");
        let gaps = manifest.check(&ManifestView::default());
        assert_eq!(gaps.len(), 1, "exactly one gap for the one missing toolset");
        let gap = gaps.first().expect("one gap was checked above");
        assert_eq!(gap.category(), "toolset", "the gap names the category");
        assert_eq!(gap.name(), "fs-tools", "the gap names the entity");
        assert_eq!(
            gap.to_string(),
            "missing from the store: toolset `fs-tools`",
            "the gap displays its category and name, and nothing else"
        );
    }

    // Scenario: noms triés, sans doublon, sans nom vide
    #[test]
    fn names_are_sorted_unique_and_not_blank() {
        let manifest = ProjectManifest::new("lymaille-demo")
            .and_then(|m| m.with_subagent("b"))
            .and_then(|m| m.with_subagent("a"))
            .and_then(|m| m.with_subagent("a"))
            .expect("plain names build a manifest");
        assert_eq!(manifest.subagents(), vec!["a", "b"], "sorted, without duplicate");
        assert!(manifest.with_subagent("").is_none(), "a blank name gives nothing");
    }

    // Scenario: aller-retour sérialisé
    #[test]
    fn serialized_round_trip_is_identical() {
        let manifest = ProjectManifest::new("lymaille-demo")
            .and_then(|m| m.with_main_agent("planner"))
            .and_then(|m| m.with_subagent("reviewer"))
            .and_then(|m| m.with_toolset("rhai-ops"))
            .and_then(|m| m.with_context_type("ticket"))
            .and_then(|m| m.with_context("session-ctx"))
            .and_then(|m| m.with_provider("vllm-strix"))
            .and_then(|m| m.with_profile("strix-qwen"))
            .expect("plain names build a manifest");
        let text = serde_json::to_string(&manifest).expect("a manifest serializes");
        let back: ProjectManifest = serde_json::from_str(&text).expect("a manifest deserializes");
        assert_eq!(back, manifest, "the round trip must be identical");
    }

    // Scenario: aller-retour sérialisé (suite) : un nom ressemblant à un secret est refusé
    #[test]
    fn secret_like_name_is_refused() {
        assert!(
            ProjectManifest::new("lymaille-demo").is_some(),
            "an ordinary project name builds a manifest"
        );
        assert!(
            ProjectManifest::new("sk-FAKE-SECRET-1234").is_none(),
            "a secret-like project name gives nothing"
        );
        let refused =
            ProjectManifest::new("lymaille-demo").and_then(|m| m.with_provider("sk-FAKE-SECRET-1234"));
        assert!(refused.is_none(), "a secret-like provider name gives nothing");
        let document = r#"{"name":"lymaille-demo","main_agents":[],"subagents":[],"toolsets":[],
            "context_types":[],"contexts":[],"providers":["Bearer abc"],"profiles":[]}"#;
        assert!(
            serde_json::from_str::<ProjectManifest>(document).is_err(),
            "a document holding a secret-like name must be refused on deserialization"
        );
    }
}
