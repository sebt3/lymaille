//! Toolset: the declaration of a toolset, that is its Rhai module source, its skills, the requirements it places on a toolset
//! context (types, parameters with defaults, its own MCP roles), its private dependencies and its version
//! (`agent-sessions/toolsets#FR-1`, `FR-2`, `FR-10`, `FR-14`). A toolset is plain data: nothing is compiled here, the
//! module is assembled elsewhere.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use vynil_core::Semver;

use super::toolset_context::value_looks_like_secret;
use super::version::semver_text;
use super::{Skill, ToolsetsError, ToolsetsResult, TypeRequirement};

/// Parameter declared by a toolset: its name, its JSON Schema, and a generic default value when one is declared. A
/// default never holds a value of an environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ParamDecl {
    name: String,
    schema: Value,
    default: Option<Value>,
}

impl ParamDecl {
    /// Builds a parameter declaration without default. The name is checked when the parameter is added to a toolset.
    #[must_use]
    pub const fn new(name: String, schema: Value) -> Self {
        Self {
            name,
            schema,
            default: None,
        }
    }

    /// Sets the generic default value of the parameter.
    #[must_use]
    pub fn with_default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self
    }

    /// Returns the name of the parameter.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the JSON Schema of the parameter.
    #[must_use]
    pub const fn schema(&self) -> &Value {
        &self.schema
    }

    /// Returns the default value of the parameter, when one is declared.
    #[must_use]
    pub const fn default(&self) -> Option<&Value> {
        self.default.as_ref()
    }
}

/// MCP role that a toolset needs for itself: its name, and whether it is required or optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RoleDecl {
    name: String,
    required: bool,
}

impl RoleDecl {
    /// Builds a role declaration. The name is checked when the role is added to a toolset.
    #[must_use]
    pub const fn new(name: String, required: bool) -> Self {
        Self { name, required }
    }

    /// Returns the name of the role.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Tells whether the role is required.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }
}

/// Declaration of a toolset.
///
/// It names a Rhai module by its identifier, and carries its source text, its skills, its requirements on a toolset
/// context, its parameters and roles, its dependencies and its version.
#[derive(Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Toolset {
    name: String,
    description: String,
    #[serde(with = "semver_text")]
    version: Semver,
    source: String,
    skills: BTreeMap<String, Skill>,
    requirements: BTreeMap<String, TypeRequirement>,
    params: BTreeMap<String, ParamDecl>,
    roles: BTreeMap<String, RoleDecl>,
    dependencies: BTreeSet<String>,
}

impl fmt::Debug for Toolset {
    /// Shows every field but the source, which is replaced by its size in bytes: a debug output never spreads the text of
    /// the module.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Toolset")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("version", &self.version.version.to_string())
            .field("source_bytes", &self.source.len())
            .field("skills", &self.skills)
            .field("requirements", &self.requirements)
            .field("params", &self.params)
            .field("roles", &self.roles)
            .field("dependencies", &self.dependencies)
            .finish()
    }
}

impl Toolset {
    /// Builds a toolset from its name, its description, its version and the source text of its Rhai module. The source is
    /// kept as text: nothing is compiled here.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when the name is not a valid Rhai identifier, or when the description is
    /// blank.
    pub fn new(name: String, description: String, version: Semver, source: String) -> ToolsetsResult<Self> {
        if !is_module_name(&name) {
            return Err(ToolsetsError::InvalidToolset {
                name,
                reason: "the name must be a valid Rhai identifier",
                detail: String::new(),
            });
        }
        if description.trim().is_empty() {
            return Err(ToolsetsError::InvalidToolset {
                name,
                reason: "the description must not be blank",
                detail: String::new(),
            });
        }
        Ok(Self {
            name,
            description,
            version,
            source,
            skills: BTreeMap::new(),
            requirements: BTreeMap::new(),
            params: BTreeMap::new(),
            roles: BTreeMap::new(),
            dependencies: BTreeSet::new(),
        })
    }

    /// Adds a skill attached to this toolset.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when a skill of the same name is already declared.
    pub fn with_skill(mut self, skill: Skill) -> ToolsetsResult<Self> {
        if self.skills.contains_key(skill.name()) {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a skill of this name is declared twice",
                detail: String::new(),
            });
        }
        let _previous = self.skills.insert(skill.name().to_owned(), skill);
        Ok(self)
    }

    /// Adds a requirement on a context type.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when a requirement on the same type is already declared.
    pub fn with_type_requirement(mut self, requirement: TypeRequirement) -> ToolsetsResult<Self> {
        if self.requirements.contains_key(requirement.name()) {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a requirement on this context type is declared twice",
                detail: String::new(),
            });
        }
        let _previous = self
            .requirements
            .insert(requirement.name().to_owned(), requirement);
        Ok(self)
    }

    /// Adds a parameter that the toolset reads from the toolset context.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when the parameter name is empty or already declared, or when its default
    /// value looks like a secret: a toolset holds no value of an environment.
    pub fn with_param(mut self, param: ParamDecl) -> ToolsetsResult<Self> {
        if param.name().is_empty() {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a parameter name is empty",
                detail: String::new(),
            });
        }
        if param.default().is_some_and(value_looks_like_secret) {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a default value looks like a secret: a toolset holds no value of an environment",
                detail: String::new(),
            });
        }
        if self.params.contains_key(param.name()) {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a parameter of this name is declared twice",
                detail: String::new(),
            });
        }
        let _previous = self.params.insert(param.name().to_owned(), param);
        Ok(self)
    }

    /// Adds an MCP role of the toolset itself.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when the role name is empty or already declared.
    pub fn with_role(mut self, role: RoleDecl) -> ToolsetsResult<Self> {
        if role.name().is_empty() {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a role name is empty",
                detail: String::new(),
            });
        }
        if self.roles.contains_key(role.name()) {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a role of this name is declared twice",
                detail: String::new(),
            });
        }
        let _previous = self.roles.insert(role.name().to_owned(), role);
        Ok(self)
    }

    /// Adds a private dependency: the name of a module that the toolset may call. The dependency is never granted to the
    /// model by this fact alone.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidToolset`] when the name is empty or already declared.
    pub fn with_dependency(mut self, name: String) -> ToolsetsResult<Self> {
        if name.is_empty() {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a dependency name is empty",
                detail: String::new(),
            });
        }
        if !self.dependencies.insert(name) {
            return Err(ToolsetsError::InvalidToolset {
                name: self.name,
                reason: "a dependency of this name is declared twice",
                detail: String::new(),
            });
        }
        Ok(self)
    }

    /// Returns the name of the toolset, which is also the name of its Rhai module.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the description of the toolset, for introspection.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the version of the toolset.
    #[must_use]
    pub const fn version(&self) -> &Semver {
        &self.version
    }

    /// Returns the source text of the Rhai module, as written.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the skills of the toolset, by name.
    #[must_use]
    pub const fn skills(&self) -> &BTreeMap<String, Skill> {
        &self.skills
    }

    /// Returns the requirements on context types, by type name.
    #[must_use]
    pub const fn type_requirements(&self) -> &BTreeMap<String, TypeRequirement> {
        &self.requirements
    }

    /// Returns the parameters of the toolset, by name.
    #[must_use]
    pub const fn params(&self) -> &BTreeMap<String, ParamDecl> {
        &self.params
    }

    /// Returns the MCP roles of the toolset, by name.
    #[must_use]
    pub const fn roles(&self) -> &BTreeMap<String, RoleDecl> {
        &self.roles
    }

    /// Returns the names of the private dependencies, in order.
    #[must_use]
    pub const fn dependencies(&self) -> &BTreeSet<String> {
        &self.dependencies
    }
}

/// Tells whether `name` is a valid Rhai identifier: ASCII letters, digits and `_`, not starting with a digit, and not a
/// reserved word. The same rule is in `scratchpad/grant.rs`; the scratchpad keeps its copy private, so it is repeated here.
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

impl<'de> Deserialize<'de> for Toolset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Record {
            name: String,
            description: String,
            #[serde(with = "semver_text")]
            version: Semver,
            source: String,
            skills: BTreeMap<String, Skill>,
            requirements: BTreeMap<String, TypeRequirement>,
            params: BTreeMap<String, ParamDecl>,
            roles: BTreeMap<String, RoleDecl>,
            dependencies: BTreeSet<String>,
        }
        let record = Record::deserialize(deserializer)?;
        let fail = |error: ToolsetsError| <D::Error as serde::de::Error>::custom(error);
        let mut built =
            Self::new(record.name, record.description, record.version, record.source).map_err(fail)?;
        for (_, skill) in record.skills {
            built = built.with_skill(skill).map_err(fail)?;
        }
        for (_, requirement) in record.requirements {
            built = built.with_type_requirement(requirement).map_err(fail)?;
        }
        for (_, param) in record.params {
            built = built.with_param(param).map_err(fail)?;
        }
        for (_, role) in record.roles {
            built = built.with_role(role).map_err(fail)?;
        }
        for name in record.dependencies {
            built = built.with_dependency(name).map_err(fail)?;
        }
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vynil_core::Semver;

    use super::{ParamDecl, RoleDecl, Toolset};
    use crate::toolsets::{Skill, ToolsetsError, TypeRequirement};

    fn version() -> Semver {
        Semver::parse("1.0.0").expect("a sample version must parse")
    }

    fn skill(name: &str) -> Skill {
        Skill::new(name.to_owned(), "a description".to_owned(), "the body".to_owned())
            .expect("a valid skill builds")
    }

    fn toolset(name: &str) -> Toolset {
        Toolset::new(
            name.to_owned(),
            "a toolset".to_owned(),
            version(),
            "fn hello() { 1 }".to_owned(),
        )
        .expect("a toolset with a valid name builds")
    }

    // Scenario: le toolset est une donnée simple
    #[test]
    fn toolset_is_plain_data() {
        // Not Rhai at all: the source must be kept as text, not compiled.
        let source = "this is { not rhai at all";
        let built = Toolset::new(
            "grafana".to_owned(),
            "Grafana queries".to_owned(),
            version(),
            source.to_owned(),
        )
        .expect("a toolset with a valid name builds")
        .with_skill(skill("grafana-queries"))
        .expect("the first skill is added")
        .with_skill(skill("grafana-alerts"))
        .expect("the second skill is added")
        .with_type_requirement(TypeRequirement::parse("cluster@1.0").expect("the sample requirement parses"))
        .expect("the requirement is added");
        assert_eq!(built.name(), "grafana", "the name is the one given");
        assert_eq!(
            built.description(),
            "Grafana queries",
            "the description is the one given"
        );
        assert_eq!(
            built.version().version.to_string(),
            "1.0.0",
            "the version is the one given"
        );
        assert_eq!(built.source(), source, "the source is kept as text, not compiled");
        assert_eq!(built.skills().len(), 2, "both skills are listed");
        assert!(
            built.skills().contains_key("grafana-alerts"),
            "a skill is found by name"
        );
        assert!(
            built.type_requirements().contains_key("cluster"),
            "the requirement is found by type name"
        );
    }

    // Scenario: noms en double refusés
    #[test]
    fn duplicate_skill_names_refused() {
        let result = toolset("grafana")
            .with_skill(skill("grafana-queries"))
            .expect("the first skill is added")
            .with_skill(skill("grafana-queries"));
        assert!(
            matches!(result, Err(ToolsetsError::InvalidToolset { .. })),
            "a skill declared twice must fail with InvalidToolset"
        );
    }

    #[test]
    fn duplicate_requirement_param_role_and_dependency_refused() {
        let base = toolset("grafana");
        let with_requirement = base
            .clone()
            .with_type_requirement(
                TypeRequirement::parse("cluster@1.0").expect("the sample requirement parses"),
            )
            .expect("the requirement is added");
        assert!(
            matches!(
                with_requirement.with_type_requirement(
                    TypeRequirement::parse("cluster@1.2").expect("the sample requirement parses")
                ),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a requirement on the same type declared twice is refused"
        );
        let with_param = base
            .clone()
            .with_param(ParamDecl::new("limit".to_owned(), json!({ "type": "integer" })))
            .expect("the parameter is added");
        assert!(
            matches!(
                with_param.with_param(ParamDecl::new("limit".to_owned(), json!({}))),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a parameter declared twice is refused"
        );
        let with_role = base
            .clone()
            .with_role(RoleDecl::new("k8s".to_owned(), true))
            .expect("the role is added");
        assert!(
            matches!(
                with_role.with_role(RoleDecl::new("k8s".to_owned(), false)),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a role declared twice is refused"
        );
        let with_dependency = base
            .with_dependency("helpers".to_owned())
            .expect("the dependency is added");
        assert!(
            matches!(
                with_dependency.with_dependency("helpers".to_owned()),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a dependency declared twice is refused"
        );
    }

    #[test]
    fn empty_param_role_and_dependency_names_refused() {
        let base = toolset("grafana");
        assert!(
            matches!(
                base.clone().with_param(ParamDecl::new(String::new(), json!({}))),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a parameter without a name is refused"
        );
        assert!(
            matches!(
                base.clone().with_role(RoleDecl::new(String::new(), true)),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a role without a name is refused"
        );
        assert!(
            matches!(
                base.with_dependency(String::new()),
                Err(ToolsetsError::InvalidToolset { .. })
            ),
            "a dependency without a name is refused"
        );
    }

    // Scenario: nom de module invalide refusé
    #[test]
    fn invalid_module_name_refused() {
        for name in ["a-b", "", "1abc"] {
            let result = Toolset::new(name.to_owned(), "a toolset".to_owned(), version(), String::new());
            assert!(
                matches!(result, Err(ToolsetsError::InvalidToolset { .. })),
                "the name `{name}` must fail with InvalidToolset"
            );
        }
    }

    // Task #2 of toolset.sdd (Must): a blank toolset description is refused.
    #[test]
    fn blank_description_refused() {
        for description in ["", "   ", "\t\n"] {
            let result = Toolset::new(
                "grafana".to_owned(),
                description.to_owned(),
                version(),
                String::new(),
            );
            assert!(
                matches!(result, Err(ToolsetsError::InvalidToolset { .. })),
                "the blank description {description:?} must fail with InvalidToolset"
            );
        }
    }

    // Task #2 of toolset.sdd (Must): a parameter default that looks like a secret is refused, as in toolset_context.
    #[test]
    fn secret_like_default_refused() {
        for default in [
            json!("sk-abcdef"),
            json!("https://user:pass@host/x"),
            json!({ "token": "Bearer abc" }),
        ] {
            let result = toolset("grafana").with_param(
                ParamDecl::new("endpoint".to_owned(), json!({ "type": "string" }))
                    .with_default(default.clone()),
            );
            assert!(
                matches!(result, Err(ToolsetsError::InvalidToolset { .. })),
                "the default {default} must fail with InvalidToolset"
            );
        }
    }

    #[test]
    fn generic_default_accepted() {
        let result = toolset("grafana").with_param(
            ParamDecl::new("limit".to_owned(), json!({ "type": "integer" })).with_default(json!(5)),
        );
        assert!(result.is_ok(), "a generic default is accepted");
    }

    #[test]
    fn valid_module_names_accepted() {
        for name in ["grafana", "_private", "k8s2"] {
            let result = Toolset::new(name.to_owned(), "a toolset".to_owned(), version(), String::new());
            assert!(result.is_ok(), "the name `{name}` is a valid Rhai identifier");
        }
    }

    // Task #3 of toolset.sdd: a reserved word of Rhai is refused as a module name, as the parser refuses `fn::f()`.
    #[test]
    fn reserved_word_refused_as_module_name() {
        for name in ["fn", "let", "while"] {
            let result = Toolset::new(name.to_owned(), "a toolset".to_owned(), version(), String::new());
            assert!(
                matches!(result, Err(ToolsetsError::InvalidToolset { .. })),
                "the reserved word `{name}` must fail with InvalidToolset"
            );
        }
    }

    #[test]
    fn params_roles_and_dependencies_are_read_back() {
        let built = toolset("grafana")
            .with_param(
                ParamDecl::new("limit".to_owned(), json!({ "type": "integer" })).with_default(json!(5)),
            )
            .expect("the parameter is added")
            .with_role(RoleDecl::new("k8s".to_owned(), true))
            .expect("the role is added")
            .with_dependency("helpers".to_owned())
            .expect("the dependency is added");
        let limit = built.params().get("limit").expect("the parameter is listed");
        assert_eq!(limit.default(), Some(&json!(5)), "the default is the one given");
        assert_eq!(
            limit.schema(),
            &json!({ "type": "integer" }),
            "the schema is the one given"
        );
        let role = built.roles().get("k8s").expect("the role is listed");
        assert!(role.required(), "the role is required, as given");
        assert!(
            built.dependencies().contains("helpers"),
            "the dependency is listed"
        );
    }

    #[test]
    fn serialization_round_trip_is_identical() {
        let built = toolset("grafana")
            .with_skill(skill("grafana-queries"))
            .expect("the skill is added")
            .with_param(ParamDecl::new("limit".to_owned(), json!({ "type": "integer" })))
            .expect("the parameter is added")
            .with_dependency("helpers".to_owned())
            .expect("the dependency is added");
        let text = serde_json::to_string(&built).expect("a toolset serializes");
        assert!(
            text.contains("\"1.0.0\""),
            "the version is written as a major.minor.patch text: {text}"
        );
        let back: Toolset = serde_json::from_str(&text).expect("a toolset deserializes");
        assert_eq!(back, built, "the round trip must be identical");
    }

    #[test]
    fn deserialization_refuses_an_invalid_module_name() {
        let text = r#"{"name":"a-b","description":"d","version":"1.0.0","source":"","skills":{},"requirements":{},"params":{},"roles":{},"dependencies":[]}"#;
        assert!(
            serde_json::from_str::<Toolset>(text).is_err(),
            "an invalid module name must fail to deserialize"
        );
    }

    // Task #4 of toolset.sdd: a `{:?}` shows the name, the version and the size of the source, never the text of the module.
    #[test]
    fn debug_elides_the_module_source() {
        let secret = "sk-FAKE-SECRET-1234";
        let source = format!("let key = '{secret}';\nfn ping() {{ 1 }}");
        let declared = Toolset::new(
            "k8s_ops".to_owned(),
            "Kubernetes operations".to_owned(),
            version(),
            source.clone(),
        )
        .expect("a valid toolset builds");
        assert!(
            source.contains(secret),
            "the precondition: the source holds the fake secret"
        );
        let shown = format!("{declared:?}");
        assert!(shown.contains("k8s_ops"), "the name is shown: {shown}");
        assert!(shown.contains("1.0.0"), "the version is shown: {shown}");
        assert!(
            shown.contains(&format!("source_bytes: {}", source.len())),
            "the size of the source is shown: {shown}"
        );
        assert!(
            !shown.contains(secret),
            "the text of the module never appears in a debug output: {shown}"
        );
    }
}
