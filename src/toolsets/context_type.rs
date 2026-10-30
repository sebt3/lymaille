//! Context type: a named, versioned schema of the parameters (JSON Schema) and of the MCP roles (required or optional) of
//! an environment, with inclusion of other types. It is the vocabulary shared by toolset authors
//! (`agent-sessions/toolsets#FR-9`, `FR-13`, `agent-sessions#DEC-11`).

use std::collections::{BTreeMap, BTreeSet};

use jsonschema::error::{ValidationError, ValidationErrorKind};
use jsonschema::paths::LocationSegment;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use vynil_core::Semver;

use super::toolset_context::value_looks_like_secret;
use super::version::semver_text;
use super::{Bump, ToolsetsError, ToolsetsResult, TypeRequirement};

/// A role of a context type: its name, and whether it is required or optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RoleSpec {
    name: String,
    required: bool,
}

impl RoleSpec {
    /// Builds a role spec. The name is checked when the role is added to a context type.
    #[must_use]
    pub const fn new(name: String, required: bool) -> Self {
        Self { name, required }
    }
}

/// Named, versioned context type. It is an entity of the user or project level, referred to by its name from a toolset.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ContextType {
    name: String,
    #[serde(with = "semver_text")]
    version: Semver,
    schema: Value,
    roles: Vec<RoleSpec>,
    includes: Vec<TypeRequirement>,
}

/// Flat view of a context type and of the types it includes.
///
/// Required parameters are the names listed by the `required` lists of the schemas; an optional parameter is a property
/// that no schema requires. A required role wins over an optional role of the same name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct FlatContextType {
    required_parameters: BTreeSet<String>,
    optional_parameters: BTreeSet<String>,
    required_roles: BTreeSet<String>,
    optional_roles: BTreeSet<String>,
    /// The parameter schemas of the type and of every type it includes, each checked against the values.
    schemas: Vec<Value>,
}

/// A rule of a parameter schema that a value of a toolset context breaks. It names the parameter and the rule only: never
/// the value. A parameter that no schema declares is the marker `*`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::toolsets) struct ValueViolation {
    parameter: Option<String>,
    rule: String,
}

impl ValueViolation {
    /// Returns the parameter concerned, or `None` when the rule concerns the set of parameters as a whole.
    pub(in crate::toolsets) fn parameter(&self) -> Option<&str> {
        self.parameter.as_deref()
    }

    /// Returns the keyword of the violated rule, such as `type` or `enum`.
    pub(in crate::toolsets) fn rule(&self) -> &str {
        &self.rule
    }
}

impl FlatContextType {
    /// Validates the values of a context against every parameter schema of the type and of the types it includes, at any
    /// depth. `type_name` names the type in a refusal.
    ///
    /// The `required` list of the root is not repeated here: the absence of a required parameter is the flat required
    /// parameters' concern. A violation is named by the parameter and the rule only (`agent-sessions#CON-3`).
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContextType`] when a schema no longer compiles as a JSON Schema.
    pub(in crate::toolsets) fn violations(
        &self,
        type_name: &str,
        values: &BTreeMap<String, Value>,
    ) -> ToolsetsResult<BTreeSet<ValueViolation>> {
        let instance = Value::Object(
            values
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<Map<String, Value>>(),
        );
        let declared = declared_names(&self.schemas);
        let mut found = BTreeSet::new();
        for schema in &self.schemas {
            let validator = jsonschema::validator_for(schema).map_err(|_refusal| {
                invalid(
                    type_name,
                    "the parameter schema does not compile as a JSON Schema",
                )
            })?;
            let errors = validator
                .iter_errors(&instance)
                .filter(|error| !is_root_required(error));
            for error in errors {
                let parameter = error
                    .instance_path()
                    .iter()
                    .next()
                    .map(|segment| masked_name(&segment, &declared));
                let _added = found.insert(ValueViolation {
                    parameter,
                    rule: error.kind().keyword().to_owned(),
                });
            }
        }
        Ok(found)
    }

    /// Returns the names of the required parameters, in order.
    #[must_use]
    pub const fn required_parameters(&self) -> &BTreeSet<String> {
        &self.required_parameters
    }

    /// Returns the names of the optional parameters, in order.
    #[must_use]
    pub const fn optional_parameters(&self) -> &BTreeSet<String> {
        &self.optional_parameters
    }

    /// Returns the names of the required roles, in order.
    #[must_use]
    pub const fn required_roles(&self) -> &BTreeSet<String> {
        &self.required_roles
    }

    /// Returns the names of the optional roles, in order.
    #[must_use]
    pub const fn optional_roles(&self) -> &BTreeSet<String> {
        &self.optional_roles
    }
}

impl ContextType {
    /// Builds a context type from its name, its version and its parameter schema (a JSON Schema object).
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContextType`] when the name is empty or not made of lowercase ASCII letters,
    /// digits and hyphens, or when the schema is not a JSON Schema object.
    pub fn new(name: String, version: Semver, schema: Value) -> ToolsetsResult<Self> {
        if !is_valid_name(&name) {
            return Err(invalid(
                &name,
                "the name must be non-empty lowercase ASCII letters, digits and hyphens",
            ));
        }
        validate_schema(&name, &schema)?;
        Ok(Self {
            name,
            version,
            schema,
            roles: Vec::new(),
            includes: Vec::new(),
        })
    }

    /// Returns the name of the type. Read by the storage, which keys the type by its name.
    pub(in crate::toolsets) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the version of the type. Read by the compatibility check of the toolsets.
    pub(in crate::toolsets) const fn version(&self) -> &Semver {
        &self.version
    }

    /// Adds a role.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContextType`] when the role name is empty or when a role of this name is already
    /// declared.
    pub fn with_role(mut self, role: RoleSpec) -> ToolsetsResult<Self> {
        if role.name.is_empty() {
            return Err(invalid(&self.name, "a role name is empty"));
        }
        if self.roles.iter().any(|existing| existing.name == role.name) {
            return Err(invalid(&self.name, "a role is declared twice"));
        }
        self.roles.push(role);
        Ok(self)
    }

    /// Adds an inclusion of another type. The inclusion is checked when the type is flattened.
    #[must_use]
    pub fn with_include(mut self, requirement: TypeRequirement) -> Self {
        self.includes.push(requirement);
        self
    }

    /// Flattens the type: its own parameters and roles, and those of every type it includes, transitively. `lookup`
    /// finds a type by its name.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContextType`] when an included type is not found, when its version does not
    /// satisfy the requirement, or when the inclusions form a cycle.
    pub fn flatten<F>(&self, lookup: &F) -> ToolsetsResult<FlatContextType>
    where
        F: Fn(&str) -> Option<Self>,
    {
        let mut gathered = Gathered::default();
        let mut path = vec![self.name.clone()];
        self.gather(lookup, &mut path, &mut gathered)?;
        Ok(gathered.finish())
    }

    /// Computes the nature of the change from `previous`, an earlier version of the same type, on the fields and roles
    /// of its own schema. Inclusions are not compared.
    ///
    /// A field or a role added as required, any field or role removed, or a field or a role that becomes required, is a
    /// major change. A field or a role added as optional, or one that becomes optional, is a minor change.
    #[must_use]
    pub fn bump_since(&self, previous: &Self) -> Bump {
        let parameters_bump = change(&parameters(&self.schema), &parameters(&previous.schema));
        let roles_bump = change(&self.role_map(), &previous.role_map());
        parameters_bump.max(roles_bump)
    }

    /// Maps each role of the type to whether it is required.
    fn role_map(&self) -> BTreeMap<String, bool> {
        self.roles
            .iter()
            .map(|role| (role.name.clone(), role.required))
            .collect()
    }

    /// Adds the parameters and roles of this type, then those of the types it includes, to `gathered`. `path` holds the
    /// names of the types being flattened, to detect a cycle.
    fn gather<F>(&self, lookup: &F, path: &mut Vec<String>, gathered: &mut Gathered) -> ToolsetsResult<()>
    where
        F: Fn(&str) -> Option<Self>,
    {
        gathered.schemas.push(self.schema.clone());
        for (name, required) in parameters(&self.schema) {
            if required {
                insert_name(&mut gathered.required_parameters, &name);
            }
            insert_name(&mut gathered.all_parameters, &name);
        }
        for role in &self.roles {
            insert_name(&mut gathered.all_roles, &role.name);
            if role.required {
                insert_name(&mut gathered.required_roles, &role.name);
            }
        }
        for requirement in &self.includes {
            let included_name = requirement.name();
            if path.iter().any(|visited| visited.as_str() == included_name) {
                return Err(invalid(included_name, "the inclusions form a cycle"));
            }
            let included = lookup(included_name)
                .filter(|candidate| candidate.name.as_str() == included_name)
                .ok_or_else(|| invalid(included_name, "the included type is not found"))?;
            if !requirement.is_satisfied_by(&included.version) {
                return Err(invalid(
                    included_name,
                    "the included type does not satisfy the required version",
                ));
            }
            path.push(included.name.clone());
            included.gather(lookup, path, gathered)?;
            let _popped = path.pop();
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ContextType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Record {
            name: String,
            #[serde(with = "semver_text")]
            version: Semver,
            schema: Value,
            roles: Vec<RoleSpec>,
            includes: Vec<TypeRequirement>,
        }
        let record = Record::deserialize(deserializer)?;
        let mut built = Self::new(record.name, record.version, record.schema)
            .map_err(<D::Error as serde::de::Error>::custom)?;
        for role in record.roles {
            built = built
                .with_role(role)
                .map_err(<D::Error as serde::de::Error>::custom)?;
        }
        for requirement in record.includes {
            built = built.with_include(requirement);
        }
        Ok(built)
    }
}

/// Accumulator of the flattening: every name seen, and the names seen as required.
#[derive(Debug, Default)]
struct Gathered {
    required_parameters: BTreeSet<String>,
    all_parameters: BTreeSet<String>,
    required_roles: BTreeSet<String>,
    all_roles: BTreeSet<String>,
    schemas: Vec<Value>,
}

impl Gathered {
    /// Turns the accumulator into the flat view: a name required anywhere is required, not optional.
    fn finish(self) -> FlatContextType {
        FlatContextType {
            optional_parameters: self
                .all_parameters
                .difference(&self.required_parameters)
                .cloned()
                .collect(),
            optional_roles: self.all_roles.difference(&self.required_roles).cloned().collect(),
            required_parameters: self.required_parameters,
            required_roles: self.required_roles,
            schemas: self.schemas,
        }
    }
}

/// Builds the refusal of a context type, naming the type and giving the fixed reason.
fn invalid(name: &str, reason: &'static str) -> ToolsetsError {
    ToolsetsError::InvalidContextType {
        name: name.to_owned(),
        reason,
    }
}

/// Tells whether `name` is a valid type name: non-empty, lowercase ASCII letters, digits and hyphens.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// Checks the parts of a parameter schema that the structural check reads: an object, a `required` list of names and
/// a `properties` object. The rest of the schema is kept as it is.
fn validate_schema(name: &str, schema: &Value) -> ToolsetsResult<()> {
    let Some(object) = schema.as_object() else {
        return Err(invalid(name, "the parameter schema is not a JSON Schema object"));
    };
    if let Some(required) = object.get("required") {
        let names = required
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string));
        if !names {
            return Err(invalid(
                name,
                "the `required` list of the schema is not a list of names",
            ));
        }
    }
    if let Some(properties) = object.get("properties")
        && !properties.is_object()
    {
        return Err(invalid(name, "the `properties` of the schema is not an object"));
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for property in properties.values() {
            if property.get("default").is_some_and(value_looks_like_secret) {
                return Err(invalid(
                    name,
                    "a parameter default looks like a secret: a schema holds references and plain values, never a secret",
                ));
            }
        }
    }
    // The schema is compiled once here, so a refusal is never left to the check. The compiler's message is dropped: it can
    // quote the schema. A reference to another document cannot be resolved (no network, no file), so it is refused here.
    let _compiled = jsonschema::validator_for(schema)
        .map_err(|_refusal| invalid(name, "the parameter schema does not compile as a JSON Schema"))?;
    Ok(())
}

/// Tells whether a violation is a `required` rule of the root: the absence of a required parameter is already a gap of the
/// flat required parameters, so it is not repeated.
fn is_root_required(error: &ValidationError<'_>) -> bool {
    error.instance_path().is_empty() && matches!(error.kind(), ValidationErrorKind::Required { .. })
}

/// Names a segment of a violation path: a declared property by its name, any other key by the marker `*`.
fn masked_name(segment: &LocationSegment<'_>, declared: &BTreeSet<String>) -> String {
    match segment {
        LocationSegment::Property(name) => {
            let text: &str = name.as_ref();
            if declared.contains(text) {
                text.to_owned()
            } else {
                "*".to_owned()
            }
        }
        LocationSegment::Index(_) => "*".to_owned(),
    }
}

/// Collects the names of every property declared under a `properties` keyword of the schemas, at any depth.
fn declared_names(schemas: &[Value]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for schema in schemas {
        collect_declared(schema, &mut out);
    }
    out
}

/// Adds to `out` every property name declared under a `properties` keyword of `schema`, at any depth.
fn collect_declared(schema: &Value, out: &mut BTreeSet<String>) {
    match schema {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "properties"
                    && let Value::Object(properties) = child
                {
                    out.extend(properties.keys().cloned());
                }
                collect_declared(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_declared(item, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Lists the names of the `required` array of a schema. A schema is validated at construction, so a non-string entry
/// cannot reach this point and is skipped.
fn required_names(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Maps each parameter of a schema to whether a schema requires it: the names of `properties` and of `required`.
fn parameters(schema: &Value) -> BTreeMap<String, bool> {
    let required = required_names(schema);
    let mut map = BTreeMap::new();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for name in properties.keys() {
            let _previous = map.insert(name.clone(), required.contains(name));
        }
    }
    for name in required {
        let _previous = map.insert(name, true);
    }
    map
}

/// Adds `name` to `set`. The set keeps the names in order and never twice.
fn insert_name(set: &mut BTreeSet<String>, name: &str) {
    let _added = set.insert(name.to_owned());
}

/// Nature of the change between two maps of name to requiredness: a name added as required, a name that becomes required,
/// or any name removed, is a major change; a name added as optional, or a name that becomes optional, is a minor change.
fn change(now: &BTreeMap<String, bool>, before: &BTreeMap<String, bool>) -> Bump {
    let mut bump = Bump::None;
    for (name, required) in now {
        let step = match before.get(name) {
            None if *required => Bump::Major,
            None => Bump::Minor,
            Some(was_required) if *required && !*was_required => Bump::Major,
            Some(was_required) if !*required && *was_required => Bump::Minor,
            Some(_) => Bump::None,
        };
        bump = bump.max(step);
    }
    if before.keys().any(|name| !now.contains_key(name)) {
        bump = bump.max(Bump::Major);
    }
    bump
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use vynil_core::Semver;

    use super::{Bump, ContextType, RoleSpec};
    use crate::toolsets::{
        ContextLevel, Toolset, ToolsetContext, ToolsetsError, TypeRequirement, check_context,
    };

    fn semver(text: &str) -> Semver {
        Semver::parse(text).expect("a sample version must parse")
    }

    /// Schema with the given required and optional parameter names.
    fn schema(required: &[&str], optional: &[&str]) -> Value {
        let mut properties = serde_json::Map::new();
        for name in required.iter().chain(optional) {
            let _previous = properties.insert((*name).to_owned(), json!({ "type": "string" }));
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }

    fn cluster_1_0() -> ContextType {
        ContextType::new("cluster".to_owned(), semver("1.0.0"), schema(&["namespace"], &[]))
            .expect("the sample type must build")
    }

    fn lookup_in(types: Vec<ContextType>) -> impl Fn(&str) -> Option<ContextType> {
        move |name: &str| types.iter().find(|candidate| candidate.name == name).cloned()
    }

    // Scenario: including a type requires both field sets
    #[test]
    fn including_a_type_requires_both_field_sets() {
        let k8s_prod = ContextType::new("k8s-prod".to_owned(), semver("1.0.0"), schema(&["region"], &[]))
            .expect("the sample type must build")
            .with_include(TypeRequirement::parse("cluster@1.0").expect("the sample requirement must parse"));
        let lookup = lookup_in(vec![cluster_1_0()]);
        let flat = k8s_prod.flatten(&lookup).expect("the inclusion is satisfied");
        let expected: std::collections::BTreeSet<String> =
            ["namespace", "region"].into_iter().map(str::to_owned).collect();
        assert_eq!(
            flat.required_parameters(),
            &expected,
            "the required parameters are the ones of both types"
        );
    }

    #[test]
    fn required_role_wins_over_optional_role_of_the_same_name() {
        let base = cluster_1_0()
            .with_role(RoleSpec::new("k8s".to_owned(), false))
            .expect("a role is added");
        let top = ContextType::new("top".to_owned(), semver("1.0.0"), schema(&[], &[]))
            .expect("the sample type must build")
            .with_role(RoleSpec::new("k8s".to_owned(), true))
            .expect("a role is added")
            .with_include(TypeRequirement::parse("cluster@1.0").expect("the sample requirement must parse"));
        let lookup = lookup_in(vec![base]);
        let flat = top.flatten(&lookup).expect("the inclusion is satisfied");
        assert!(
            flat.required_roles().contains("k8s"),
            "a role required in one type and optional in the other is required"
        );
        assert!(
            !flat.optional_roles().contains("k8s"),
            "a required role is not also listed as optional"
        );
    }

    // Scenario: optional field is a minor change
    #[test]
    fn optional_field_is_a_minor_change() {
        let previous = cluster_1_0();
        let next = ContextType::new(
            "cluster".to_owned(),
            semver("1.1.0"),
            schema(&["namespace"], &["zone"]),
        )
        .expect("the sample type must build");
        assert_eq!(
            next.bump_since(&previous),
            Bump::Minor,
            "an optional field is a minor change"
        );
    }

    // Scenario: required field is a major change
    #[test]
    fn required_field_is_a_major_change() {
        let previous = cluster_1_0();
        let next = ContextType::new(
            "cluster".to_owned(),
            semver("2.0.0"),
            schema(&["namespace", "zone"], &[]),
        )
        .expect("the sample type must build");
        assert_eq!(
            next.bump_since(&previous),
            Bump::Major,
            "a required field is a major change"
        );
    }

    // Scenario: optional becoming required is a major change
    #[test]
    fn optional_becoming_required_is_a_major_change() {
        let previous = ContextType::new(
            "cluster".to_owned(),
            semver("1.0.0"),
            schema(&["namespace"], &["zone"]),
        )
        .expect("the sample type must build");
        let next = ContextType::new(
            "cluster".to_owned(),
            semver("2.0.0"),
            schema(&["namespace", "zone"], &[]),
        )
        .expect("the sample type must build");
        assert_eq!(
            next.bump_since(&previous),
            Bump::Major,
            "a field that goes from optional to required is a major change"
        );
    }

    #[test]
    fn required_becoming_optional_is_a_minor_change() {
        let previous = ContextType::new(
            "cluster".to_owned(),
            semver("1.0.0"),
            schema(&["namespace", "zone"], &[]),
        )
        .expect("the sample type must build");
        let next = ContextType::new(
            "cluster".to_owned(),
            semver("1.1.0"),
            schema(&["namespace"], &["zone"]),
        )
        .expect("the sample type must build");
        assert_eq!(
            next.bump_since(&previous),
            Bump::Minor,
            "a field that goes from required to optional is a minor change"
        );
    }

    #[test]
    fn role_becoming_required_is_a_major_change() {
        let previous = cluster_1_0()
            .with_role(RoleSpec::new("gpu".to_owned(), false))
            .expect("a role is added");
        let next = cluster_1_0()
            .with_role(RoleSpec::new("gpu".to_owned(), true))
            .expect("a role is added");
        assert_eq!(
            next.bump_since(&previous),
            Bump::Major,
            "a role that goes from optional to required is a major change"
        );
    }

    // Scenario: secret-like default refused
    #[test]
    fn secret_like_default_refused() {
        let schema = json!({
            "type": "object",
            "properties": { "api_key": { "type": "string", "default": "sk-abcdef" } },
        });
        let result = ContextType::new("cluster".to_owned(), semver("1.0.0"), schema);
        match result {
            Err(error @ ToolsetsError::InvalidContextType { .. }) => {
                assert!(
                    !error.to_string().contains("sk-abcdef"),
                    "the message never carries the default value: {error}"
                );
            }
            other => panic!("a secret-like default must fail with InvalidContextType, got {other:?}"),
        }
    }

    #[test]
    fn plain_default_accepted() {
        let schema = json!({
            "type": "object",
            "properties": { "region": { "type": "string", "default": "eu" } },
        });
        assert!(
            ContextType::new("cluster".to_owned(), semver("1.0.0"), schema).is_ok(),
            "a plain default is accepted"
        );
    }

    // Scenario: removal is a major change
    #[test]
    fn removal_is_a_major_change() {
        let previous = cluster_1_0()
            .with_role(RoleSpec::new("k8s".to_owned(), true))
            .expect("a role is added")
            .with_role(RoleSpec::new("gpu".to_owned(), false))
            .expect("a role is added");
        let next = ContextType::new("cluster".to_owned(), semver("2.0.0"), schema(&["namespace"], &[]))
            .expect("the sample type must build")
            .with_role(RoleSpec::new("k8s".to_owned(), true))
            .expect("a role is added");
        assert_eq!(
            next.bump_since(&previous),
            Bump::Major,
            "removing a role is a major change"
        );
    }

    // Scenario: inclusion cycle refused
    #[test]
    fn inclusion_cycle_refused() {
        let first = ContextType::new("first".to_owned(), semver("1.0.0"), schema(&[], &[]))
            .expect("the sample type must build")
            .with_include(TypeRequirement::parse("second@1.0").expect("the sample requirement must parse"));
        let second = ContextType::new("second".to_owned(), semver("1.0.0"), schema(&[], &[]))
            .expect("the sample type must build")
            .with_include(TypeRequirement::parse("first@1.0").expect("the sample requirement must parse"));
        let lookup = lookup_in(vec![first.clone(), second]);
        let result = first.flatten(&lookup);
        assert!(
            matches!(result, Err(ToolsetsError::InvalidContextType { .. })),
            "a cycle must fail with InvalidContextType"
        );
    }

    #[test]
    fn missing_or_too_old_inclusion_refused() {
        let top = ContextType::new("top".to_owned(), semver("1.0.0"), schema(&[], &[]))
            .expect("the sample type must build")
            .with_include(TypeRequirement::parse("cluster@1.2").expect("the sample requirement must parse"));
        let too_old = lookup_in(vec![cluster_1_0()]);
        assert!(
            matches!(
                top.flatten(&too_old),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "an included version 1.0 does not satisfy cluster@1.2"
        );
        let missing = lookup_in(Vec::new());
        assert!(
            matches!(
                top.flatten(&missing),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "a missing included type is refused"
        );
    }

    #[test]
    fn invalid_name_and_schema_refused() {
        assert!(
            matches!(
                ContextType::new(String::new(), semver("1.0.0"), schema(&[], &[])),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "an empty name is refused"
        );
        assert!(
            matches!(
                ContextType::new("Cluster".to_owned(), semver("1.0.0"), schema(&[], &[])),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "an uppercase name is refused"
        );
        assert!(
            matches!(
                ContextType::new("cluster".to_owned(), semver("1.0.0"), json!("not an object")),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "a schema that is not an object is refused"
        );
    }

    #[test]
    fn empty_and_duplicate_roles_refused() {
        let base = cluster_1_0();
        assert!(
            matches!(
                base.clone().with_role(RoleSpec::new(String::new(), true)),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "an empty role name is refused"
        );
        let once = base
            .with_role(RoleSpec::new("k8s".to_owned(), true))
            .expect("the first role is added");
        assert!(
            matches!(
                once.with_role(RoleSpec::new("k8s".to_owned(), false)),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "a role declared twice is refused"
        );
    }

    #[test]
    fn serialization_round_trip_is_identical() {
        let built = cluster_1_0()
            .with_role(RoleSpec::new("k8s".to_owned(), true))
            .expect("a role is added");
        let text = serde_json::to_string(&built).expect("a context type serializes");
        assert!(
            text.contains("\"1.0.0\""),
            "the version is written as a major.minor.patch text: {text}"
        );
        let back: ContextType = serde_json::from_str(&text).expect("a context type deserializes");
        assert_eq!(back, built, "the round trip must be identical");
    }

    #[test]
    fn deserialization_refuses_what_the_constructor_refuses() {
        let text =
            r#"{"name":"Cluster","version":"1.0.0","schema":{"type":"object"},"roles":[],"includes":[]}"#;
        assert!(
            serde_json::from_str::<ContextType>(text).is_err(),
            "an uppercase name must fail to deserialize"
        );
    }

    /// A toolset `grafana` that requires `requirement`, built with the public constructors.
    fn grafana_requiring(requirement: &str) -> Toolset {
        Toolset::new(
            "grafana".to_owned(),
            "Grafana queries".to_owned(),
            semver("1.0.0"),
            "fn query() { 1 }".to_owned(),
        )
        .expect("the toolset builds")
        .with_type_requirement(TypeRequirement::parse(requirement).expect("the requirement parses"))
        .expect("the requirement is added")
    }

    // Scenario: schema that does not compile is refused
    #[test]
    fn schema_that_does_not_compile_is_refused() {
        let result = ContextType::new("cluster".to_owned(), semver("1.0.0"), json!({ "type": "foo" }));
        assert!(
            matches!(result, Err(ToolsetsError::InvalidContextType { .. })),
            "a schema that does not compile as JSON Schema must be refused, got {result:?}"
        );
    }

    // Scenario: remote reference is refused
    #[test]
    fn remote_and_file_references_are_refused() {
        for target in [
            "https://example.com/schema.json",
            "file:///nonexistent-lymaille-schema.json",
        ] {
            let schema = json!({ "type": "object", "properties": { "region": { "$ref": target } } });
            match ContextType::new("cluster".to_owned(), semver("1.0.0"), schema) {
                Err(error @ ToolsetsError::InvalidContextType { .. }) => assert!(
                    !error.to_string().contains("example.com") && !error.to_string().contains("nonexistent"),
                    "the refusal never quotes the reference: {error}"
                ),
                other => {
                    panic!("a reference to another document must be refused, got {other:?} for {target}")
                }
            }
        }
    }

    // Scenario: every value is validated against every flattened schema
    #[test]
    fn every_value_is_validated_against_every_flattened_schema() {
        let k8s_prod = ContextType::new(
            "k8s-prod".to_owned(),
            semver("1.0.0"),
            json!({ "type": "object", "properties": { "region": { "type": "integer" } } }),
        )
        .expect("the sample type must build")
        .with_include(TypeRequirement::parse("cluster@1.0").expect("the sample requirement must parse"));
        let lookup = lookup_in(vec![cluster_1_0(), k8s_prod]);
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::Project)
            .expect("a named context builds")
            .with_parameter("namespace".to_owned(), json!(["valeur-fautive-42"]))
            .expect("a parameter is added")
            .with_parameter("region".to_owned(), json!("eu"))
            .expect("a parameter is added");
        let gaps = check_context(&context, &[grafana_requiring("k8s-prod@1.0")], &lookup)
            .expect("the types are consistent");
        let texts: Vec<String> = gaps.iter().map(ToString::to_string).collect();
        for parameter in ["namespace", "region"] {
            assert!(
                texts
                    .iter()
                    .any(|text| text.contains(parameter) && text.contains("`type`")),
                "the wrong type of `{parameter}` is a gap, inherited or own: {texts:?}"
            );
        }
        assert!(
            texts.iter().all(|text| !text.contains("valeur-fautive-42")),
            "no gap carries the value: {texts:?}"
        );
    }

    // Scenario: undeclared key is rendered as a marker
    #[test]
    fn undeclared_key_is_rendered_as_a_marker() {
        let open = ContextType::new(
            "open".to_owned(),
            semver("1.0.0"),
            json!({ "type": "object", "additionalProperties": { "type": "integer" } }),
        )
        .expect("the sample type must build");
        let lookup = lookup_in(vec![open]);
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::Session)
            .expect("a named context builds")
            .with_parameter("leaked-key".to_owned(), json!("texte"))
            .expect("a parameter is added");
        let gaps = check_context(&context, &[grafana_requiring("open@1.0")], &lookup)
            .expect("the types are consistent");
        let texts: Vec<String> = gaps.iter().map(ToString::to_string).collect();
        assert!(
            texts
                .iter()
                .any(|text| text.contains("`*`") && text.contains("`type`")),
            "the undeclared key is the marker `*`: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .all(|text| !text.contains("leaked-key") && !text.contains("texte")),
            "neither the key nor the value comes out: {texts:?}"
        );
    }

    #[test]
    fn flat_type_of_a_type_without_inclusion_is_its_own() {
        let flat = cluster_1_0()
            .flatten(&|_| None)
            .expect("a type without inclusion flattens");
        assert_eq!(flat.required_parameters().len(), 1, "one required parameter");
        assert!(
            flat.required_parameters().contains("namespace"),
            "the own required parameter is listed"
        );
    }
}
