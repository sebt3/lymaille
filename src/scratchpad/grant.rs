//! Abstract description of what the agent definition grants to the scratchpad: groups of the `vynil-core` common core,
//! Rhai modules, toolsets and skills. The scratchpad receives it from the host or from the toolsets domain; it knows
//! neither toolsets nor MCP modules. Nothing is granted by default.

use std::collections::BTreeSet;
use std::fmt;

use rhai::{Module, Shared};

/// Group of functions of the `vynil-core` common core that the definition may grant. The pure group is always registered
/// and is not listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum CoreGroup {
    /// Host functions (`get_env`, `log_*`).
    Host,
    /// Waiting functions (`sleep`), with a cancellation point.
    Sleep,
    /// File functions (`file_*`).
    Fs,
    /// Shell functions (`shell_run`).
    Shell,
}

/// A Rhai module granted to the scratchpad, callable as `name::function()`. Its content is code and bindings that the
/// host has already resolved; its `Debug` shows only the name.
#[derive(Clone)]
#[non_exhaustive]
pub struct GrantedModule {
    name: String,
    module: Shared<Module>,
}

impl fmt::Debug for GrantedModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrantedModule")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl GrantedModule {
    /// Builds a granted module. Returns nothing when the name is not a Rhai identifier: ASCII letters, digits and `_`,
    /// not starting with a digit, not a reserved word of Rhai, not empty.
    #[must_use]
    pub fn new(name: &str, module: Shared<Module>) -> Option<Self> {
        is_module_name(name).then(|| Self {
            name: name.to_owned(),
            module,
        })
    }

    /// Name of the module, as called from a script.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The Rhai module shared with the engine.
    #[must_use]
    pub const fn module(&self) -> &Shared<Module> {
        &self.module
    }
}

/// A toolset granted to the scratchpad. It describes only what introspection lists: its name and its description.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GrantedToolset {
    name: String,
    description: String,
}

impl GrantedToolset {
    /// Builds a granted toolset. Returns nothing when the name or the description is empty or blank.
    #[must_use]
    pub fn new(name: &str, description: &str) -> Option<Self> {
        (!is_blank(name) && !is_blank(description)).then(|| Self {
            name: name.to_owned(),
            description: description.to_owned(),
        })
    }

    /// Name of the toolset.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Description of the toolset.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// A skill granted to the scratchpad, optionally attached to a toolset by name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GrantedSkill {
    name: String,
    description: String,
    toolset: Option<String>,
}

impl GrantedSkill {
    /// Builds a granted skill. Returns nothing when the name or the description is empty or blank. The attached toolset is
    /// not checked against the granted toolsets.
    #[must_use]
    pub fn new(name: &str, description: &str, toolset: Option<&str>) -> Option<Self> {
        (!is_blank(name) && !is_blank(description)).then(|| Self {
            name: name.to_owned(),
            description: description.to_owned(),
            toolset: toolset.map(str::to_owned),
        })
    }

    /// Name of the skill.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Description of the skill.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Name of the toolset the skill is attached to, when it is.
    #[must_use]
    pub fn toolset(&self) -> Option<&str> {
        self.toolset.as_deref()
    }
}

/// What the agent definition grants. Built by the constructors and the `with_*` methods; each addition is refused when its
/// name is already taken by another item of the same category.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Grants {
    groups: BTreeSet<CoreGroup>,
    modules: Vec<GrantedModule>,
    toolsets: Vec<GrantedToolset>,
    skills: Vec<GrantedSkill>,
}

impl Grants {
    /// Empty description: it grants nothing but the pure group, which is always registered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants a group of the common core. Adding a group twice has no further effect.
    #[must_use]
    pub fn with_group(mut self, group: CoreGroup) -> Self {
        let _added = self.groups.insert(group);
        self
    }

    /// Grants a module. Returns nothing when another granted module has the same name.
    #[must_use]
    pub fn with_module(mut self, module: GrantedModule) -> Option<Self> {
        if self.modules.iter().any(|granted| granted.name == module.name) {
            return None;
        }
        self.modules.push(module);
        Some(self)
    }

    /// Grants a toolset. Returns nothing when another granted toolset has the same name.
    #[must_use]
    pub fn with_toolset(mut self, toolset: GrantedToolset) -> Option<Self> {
        if self.toolsets.iter().any(|granted| granted.name == toolset.name) {
            return None;
        }
        self.toolsets.push(toolset);
        Some(self)
    }

    /// Grants a skill. Returns nothing when another granted skill has the same name.
    #[must_use]
    pub fn with_skill(mut self, skill: GrantedSkill) -> Option<Self> {
        if self.skills.iter().any(|granted| granted.name == skill.name) {
            return None;
        }
        self.skills.push(skill);
        Some(self)
    }

    /// Granted groups, in order: host, sleep, files, shell.
    #[must_use]
    pub const fn groups(&self) -> &BTreeSet<CoreGroup> {
        &self.groups
    }

    /// Granted modules, in the order they were added.
    #[must_use]
    pub fn modules(&self) -> &[GrantedModule] {
        &self.modules
    }

    /// Granted toolsets, in the order they were added.
    #[must_use]
    pub fn toolsets(&self) -> &[GrantedToolset] {
        &self.toolsets
    }

    /// Granted skills, in the order they were added.
    #[must_use]
    pub fn skills(&self) -> &[GrantedSkill] {
        &self.skills
    }
}

/// Whether `name` is a valid name of a module: ASCII letters, digits and `_`, starting with a letter or `_`, and not a
/// reserved word or symbol of Rhai.
fn is_module_name(name: &str) -> bool {
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
    starts_well
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && rhai::is_valid_function_name(name)
}

/// Whether `text` is empty or made of white space only.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::{CoreGroup, GrantedModule, GrantedSkill, GrantedToolset, Grants};
    use rhai::{Module, Shared};

    fn module_named(name: &str) -> GrantedModule {
        GrantedModule::new(name, Shared::new(Module::new())).expect("a valid name")
    }

    #[test]
    fn nothing_is_granted_by_default() {
        let grants = Grants::new();
        assert!(grants.groups().is_empty(), "no group");
        assert!(grants.modules().is_empty(), "no module");
        assert!(grants.toolsets().is_empty(), "no toolset");
        assert!(grants.skills().is_empty(), "no skill");
    }

    #[test]
    fn groups_accumulate_in_a_set() {
        let grants = Grants::new()
            .with_group(CoreGroup::Host)
            .with_group(CoreGroup::Sleep)
            .with_group(CoreGroup::Host);
        let groups: Vec<CoreGroup> = grants.groups().iter().copied().collect();
        assert_eq!(groups, vec![CoreGroup::Host, CoreGroup::Sleep], "a set in order");
    }

    #[test]
    fn duplicate_names_refused() {
        let grants = Grants::new()
            .with_module(module_named("grafana"))
            .expect("first module accepted");
        let again = grants.clone().with_module(module_named("grafana"));
        assert!(again.is_none(), "a second module with the same name is refused");
        assert_eq!(grants.modules().len(), 1, "the first module is kept");
        assert_eq!(
            grants.modules().first().map(GrantedModule::name),
            Some("grafana"),
            "the first module"
        );
    }

    #[test]
    fn invalid_module_name_refused() {
        for name in ["1abc", "a-b", "", "fn", "let"] {
            assert!(
                GrantedModule::new(name, Shared::new(Module::new())).is_none(),
                "the name `{name}` must be refused"
            );
        }
    }

    #[test]
    fn valid_module_name_accepted() {
        let module = GrantedModule::new("grafana_prod", Shared::new(Module::new()));
        assert_eq!(
            module.as_ref().map(GrantedModule::name),
            Some("grafana_prod"),
            "accepted and named"
        );
    }

    #[test]
    fn blank_toolset_and_skill_refused() {
        assert!(
            GrantedToolset::new("", "described").is_none(),
            "empty toolset name"
        );
        assert!(
            GrantedToolset::new("   ", "described").is_none(),
            "blank toolset name"
        );
        assert!(
            GrantedToolset::new("grafana", "  ").is_none(),
            "blank toolset description"
        );
        assert!(
            GrantedSkill::new("skill", "   ", None).is_none(),
            "blank skill description"
        );
        assert!(
            GrantedSkill::new("", "described", Some("grafana")).is_none(),
            "empty skill name"
        );
        assert!(
            GrantedToolset::new("grafana", "described").is_some(),
            "a valid toolset is accepted"
        );
        assert!(
            GrantedSkill::new("skill", "described", None).is_some(),
            "a valid skill is accepted"
        );
    }

    #[test]
    fn debug_hides_the_module_content() {
        let mut module = Module::new();
        let _hash = rhai::FuncRegistration::new("secret_function")
            .set_into_module(&mut module, || "value".to_owned());
        let granted = GrantedModule::new("grafana", Shared::new(module)).expect("a valid name");
        let shown = format!("{granted:?}");
        assert!(shown.contains("grafana"), "the name is shown: {shown}");
        assert!(
            !shown.contains("secret_function"),
            "the content is hidden: {shown}"
        );
    }

    #[test]
    fn granted_skill_keeps_its_toolset() {
        let skill = GrantedSkill::new("skill", "described", Some("grafana")).expect("valid");
        assert_eq!(skill.toolset(), Some("grafana"), "attached toolset");
        assert_eq!(skill.description(), "described", "description");
    }
}
