//! Abstract description of what the `ToolsetContext` presents to the scratchpad: the toolsets' Rhai modules, their
//! descriptions, and the skills. The scratchpad receives it from the `toolsets` domain; it knows neither toolsets nor MCP
//! modules. Nothing is presented by default.

use std::fmt;

use rhai::{Module, Shared};

/// A Rhai module presented to the scratchpad, callable as `name::function()`. Its content is code and bindings that the
/// host has already resolved; its `Debug` shows only the name.
#[derive(Clone)]
#[non_exhaustive]
pub struct PresentedModule {
    name: String,
    module: Shared<Module>,
}

impl fmt::Debug for PresentedModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedModule")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PresentedModule {
    /// Builds a presented module. Returns nothing when the name is not a Rhai identifier: ASCII letters, digits and `_`,
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

/// A toolset presented to the scratchpad. It describes only what introspection lists: its name and its description.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PresentedToolset {
    name: String,
    description: String,
}

impl PresentedToolset {
    /// Builds a presented toolset. Returns nothing when the name or the description is empty or blank.
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

/// A skill presented to the scratchpad, optionally attached to a toolset by name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PresentedSkill {
    name: String,
    description: String,
    toolset: Option<String>,
}

impl PresentedSkill {
    /// Builds a presented skill. Returns nothing when the name or the description is empty or blank. The attached toolset is
    /// not checked against the presented toolsets.
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

/// What the `ToolsetContext` presents. Built by the constructors and the `with_*` methods; each addition is refused when its
/// name is already taken by another item of the same category.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Presentation {
    modules: Vec<PresentedModule>,
    toolsets: Vec<PresentedToolset>,
    skills: Vec<PresentedSkill>,
}

impl Presentation {
    /// Empty description: it presents nothing, the core group being always registered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Presents a module. Returns nothing when another presented module has the same name. Only the `toolsets` domain
    /// (`assemble`) produces presented modules; the host adds none.
    #[must_use]
    pub(crate) fn with_module(mut self, module: PresentedModule) -> Option<Self> {
        if self.modules.iter().any(|presented| presented.name == module.name) {
            return None;
        }
        self.modules.push(module);
        Some(self)
    }

    /// Presents a toolset. Returns nothing when another presented toolset has the same name.
    #[must_use]
    pub fn with_toolset(mut self, toolset: PresentedToolset) -> Option<Self> {
        if self
            .toolsets
            .iter()
            .any(|presented| presented.name == toolset.name)
        {
            return None;
        }
        self.toolsets.push(toolset);
        Some(self)
    }

    /// Presents a skill. Returns nothing when another presented skill has the same name.
    #[must_use]
    pub fn with_skill(mut self, skill: PresentedSkill) -> Option<Self> {
        if self.skills.iter().any(|presented| presented.name == skill.name) {
            return None;
        }
        self.skills.push(skill);
        Some(self)
    }

    /// Presented modules, in the order they were added.
    #[must_use]
    pub fn modules(&self) -> &[PresentedModule] {
        &self.modules
    }

    /// Presented toolsets, in the order they were added.
    #[must_use]
    pub fn toolsets(&self) -> &[PresentedToolset] {
        &self.toolsets
    }

    /// Presented skills, in the order they were added.
    #[must_use]
    pub fn skills(&self) -> &[PresentedSkill] {
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
    use super::{Presentation, PresentedModule, PresentedSkill, PresentedToolset};
    use rhai::{Module, Shared};

    fn module_named(name: &str) -> PresentedModule {
        PresentedModule::new(name, Shared::new(Module::new())).expect("a valid name")
    }

    #[test]
    fn nothing_is_presented_by_default() {
        let presentation = Presentation::new();
        assert!(presentation.modules().is_empty(), "no module");
        assert!(presentation.toolsets().is_empty(), "no toolset");
        assert!(presentation.skills().is_empty(), "no skill");
    }

    #[test]
    fn duplicate_names_refused() {
        let presentation = Presentation::new()
            .with_module(module_named("grafana"))
            .expect("first module accepted");
        let again = presentation.clone().with_module(module_named("grafana"));
        assert!(again.is_none(), "a second module with the same name is refused");
        assert_eq!(presentation.modules().len(), 1, "the first module is kept");
        assert_eq!(
            presentation.modules().first().map(PresentedModule::name),
            Some("grafana"),
            "the first module"
        );
    }

    #[test]
    fn duplicate_toolsets_and_skills_refused() {
        let toolset = |description: &str| PresentedToolset::new("grafana", description).expect("valid");
        let presentation = Presentation::new()
            .with_toolset(toolset("first"))
            .expect("first toolset accepted");
        let again = presentation.clone().with_toolset(toolset("second"));
        assert!(again.is_none(), "a second toolset with the same name is refused");
        assert_eq!(presentation.toolsets().len(), 1, "the first toolset is kept");
        assert_eq!(
            presentation.toolsets().first().map(PresentedToolset::description),
            Some("first"),
            "the first toolset"
        );
        let skill = |description: &str| PresentedSkill::new("loki-debug", description, None).expect("valid");
        let with_skill = Presentation::new()
            .with_skill(skill("first"))
            .expect("first skill accepted");
        let again_skill = with_skill.clone().with_skill(skill("second"));
        assert!(
            again_skill.is_none(),
            "a second skill with the same name is refused"
        );
        assert_eq!(with_skill.skills().len(), 1, "the first skill is kept");
        assert_eq!(
            with_skill.skills().first().map(PresentedSkill::description),
            Some("first"),
            "the first skill"
        );
    }

    #[test]
    fn presentation_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PresentedModule>();
        assert_send_sync::<PresentedToolset>();
        assert_send_sync::<PresentedSkill>();
        assert_send_sync::<Presentation>();
    }

    #[test]
    fn reserved_words_refused_as_module_names_as_the_parser_does() {
        let engine = rhai::Engine::new();
        for name in [
            "global", "this", "const", "import", "export", "private", "loop", "while", "if", "switch",
            "return", "throw", "try", "true", "false", "fn", "let", "grafana",
        ] {
            let accepted = PresentedModule::new(name, Shared::new(Module::new())).is_some();
            let parses = engine.compile(format!("{name}::f()")).is_ok();
            assert_eq!(
                accepted, parses,
                "`{name}`: the presented name check and the Rhai parser of `{name}::f()` must agree"
            );
        }
    }

    #[test]
    fn invalid_module_name_refused() {
        for name in ["1abc", "a-b", "", "fn", "let"] {
            assert!(
                PresentedModule::new(name, Shared::new(Module::new())).is_none(),
                "the name `{name}` must be refused"
            );
        }
    }

    #[test]
    fn valid_module_name_accepted() {
        let module = PresentedModule::new("grafana_prod", Shared::new(Module::new()));
        assert_eq!(
            module.as_ref().map(PresentedModule::name),
            Some("grafana_prod"),
            "accepted and named"
        );
    }

    #[test]
    fn blank_toolset_and_skill_refused() {
        assert!(
            PresentedToolset::new("", "described").is_none(),
            "empty toolset name"
        );
        assert!(
            PresentedToolset::new("   ", "described").is_none(),
            "blank toolset name"
        );
        assert!(
            PresentedToolset::new("grafana", "  ").is_none(),
            "blank toolset description"
        );
        assert!(
            PresentedSkill::new("skill", "   ", None).is_none(),
            "blank skill description"
        );
        assert!(
            PresentedSkill::new("", "described", Some("grafana")).is_none(),
            "empty skill name"
        );
        assert!(
            PresentedToolset::new("grafana", "described").is_some(),
            "a valid toolset is accepted"
        );
        assert!(
            PresentedSkill::new("skill", "described", None).is_some(),
            "a valid skill is accepted"
        );
    }

    #[test]
    fn debug_hides_the_module_content() {
        let mut module = Module::new();
        let _hash = rhai::FuncRegistration::new("secret_function")
            .set_into_module(&mut module, || "value".to_owned());
        let presented = PresentedModule::new("grafana", Shared::new(module)).expect("a valid name");
        let shown = format!("{presented:?}");
        assert!(shown.contains("grafana"), "the name is shown: {shown}");
        assert!(
            !shown.contains("secret_function"),
            "the content is hidden: {shown}"
        );
    }

    #[test]
    fn presented_skill_keeps_its_toolset() {
        let skill = PresentedSkill::new("skill", "described", Some("grafana")).expect("valid");
        assert_eq!(skill.toolset(), Some("grafana"), "attached toolset");
        assert_eq!(skill.description(), "described", "description");
    }
}
