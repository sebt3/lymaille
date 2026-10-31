//! Context type: a named, versioned schema of the parameters (JSON Schema) and of the MCP roles (required or optional) of
//! an environment, with inclusion of other types. It is the vocabulary shared by toolset authors
//! (`agent-sessions/toolsets#FR-9`, `FR-13`, `agent-sessions#DEC-11`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jsonschema::error::{ValidationError, ValidationErrorKind};
use jsonschema::paths::LocationSegment;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use vynil_core::Semver;

use super::toolset_context::value_looks_like_secret;
use super::version::semver_text;
use super::{Bump, ToolsetsError, ToolsetsResult, TypeRequirement};
use crate::secret::{DefaultSecretDetector, SecretDetector};

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
    /// The schema compiled once, when the type is built; it is not serialized, it is rebuilt from `schema` on reading.
    #[serde(skip)]
    compiled: Arc<CompiledSchema>,
    roles: Vec<RoleSpec>,
    includes: Vec<TypeRequirement>,
}

/// A parameter schema with its validator, compiled once when its type is built (decision of 2026-10-31, D-b): a check never
/// compiles again. The schema is kept beside the validator for the names it declares and for the equality of types.
#[derive(Debug)]
struct CompiledSchema {
    schema: Value,
    validator: jsonschema::Validator,
}

impl PartialEq for CompiledSchema {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
    }
}

impl Eq for CompiledSchema {}

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
    /// The compiled parameter schemas of the type and of every type it includes, each checked against the values.
    schemas: Vec<Arc<CompiledSchema>>,
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
    /// depth, with the validators compiled when the types were built.
    ///
    /// A `required` rule of the root that names one of the flat required parameters is not repeated here: that absence is
    /// the flat required parameters' concern, reported once. Any other `required` rule of the root, such as one under
    /// `$ref`, `allOf`, `dependentRequired` or `if`/`then`, is a violation without a parameter. A violation is named by the
    /// parameter and the rule only (`agent-sessions#CON-3`).
    pub(in crate::toolsets) fn violations(
        &self,
        values: &BTreeMap<String, Value>,
    ) -> BTreeSet<ValueViolation> {
        let instance = Value::Object(
            values
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<Map<String, Value>>(),
        );
        let declared = declared_names(&self.schemas);
        self.schemas
            .iter()
            .flat_map(|compiled| compiled.validator.iter_errors(&instance))
            .filter_map(|error| self.violation_of(&error, &declared))
            .collect()
    }

    /// The violation that `error` stands for, or `None` when it is the `required` rule of the root for a flat required
    /// parameter, which is already a gap of its own.
    fn violation_of(
        &self,
        error: &ValidationError<'_>,
        declared: &BTreeSet<String>,
    ) -> Option<ValueViolation> {
        if error.instance_path().is_empty()
            && let ValidationErrorKind::Required { property } = error.kind()
            && property
                .as_str()
                .is_some_and(|name| self.required_parameters.contains(name))
        {
            return None;
        }
        let parameter = error
            .instance_path()
            .iter()
            .next()
            .map(|segment| masked_name(&segment, declared));
        Some(ValueViolation {
            parameter,
            rule: error.kind().keyword().to_owned(),
        })
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
    /// Builds a context type from its name, its version and its parameter schema (a JSON Schema object). The schema is
    /// compiled here, once: a schema that does not compile, or whose serialized form is over 64 KiB, is refused.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContextType`] when the name is empty or not made of lowercase ASCII letters,
    /// digits and hyphens, when the schema is not a JSON Schema object, when its serialized form is over 64 KiB, when a
    /// parameter default looks like a secret, or when the schema does not compile (a reference to another document
    /// included). The reason never quotes the schema.
    pub fn new(name: String, version: Semver, schema: Value) -> ToolsetsResult<Self> {
        Self::new_with_detector(name, version, schema, &DefaultSecretDetector)
    }

    /// Builds a context type like [`ContextType::new`], but the defaults of the schema are judged by the detector of the
    /// host instead of the default one.
    ///
    /// # Errors
    ///
    /// The same as [`ContextType::new`].
    pub fn new_with_detector(
        name: String,
        version: Semver,
        schema: Value,
        detector: &dyn SecretDetector,
    ) -> ToolsetsResult<Self> {
        if !is_valid_name(&name) {
            return Err(invalid(
                &name,
                "the name must be non-empty lowercase ASCII letters, digits and hyphens",
            ));
        }
        let compiled = Arc::new(compile_schema(&name, schema, detector)?);
        Ok(Self {
            name,
            version,
            schema: compiled.schema.clone(),
            compiled,
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
        gathered.schemas.push(Arc::clone(&self.compiled));
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
    schemas: Vec<Arc<CompiledSchema>>,
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

/// Largest serialized size of a parameter schema, in bytes: 64 KiB (decision of 2026-10-31, D-b).
const SCHEMA_MAX_BYTES: usize = 64 * 1024;

/// Checks a parameter schema and compiles it once. The serialized schema is at most [`SCHEMA_MAX_BYTES`]; the parts that
/// the structural check reads are an object, a `required` list of names and a `properties` object, and no default of a
/// parameter looks like a secret. The compiler's message is dropped, since it can quote the schema.
fn compile_schema(
    name: &str,
    schema: Value,
    detector: &dyn SecretDetector,
) -> ToolsetsResult<CompiledSchema> {
    let size = serde_json::to_string(&schema).map_or(usize::MAX, |text| text.len());
    if size > SCHEMA_MAX_BYTES {
        return Err(invalid(
            name,
            "the parameter schema is larger than 64 KiB once serialized",
        ));
    }
    validate_shape(name, &schema, detector)?;
    // A reference to another document cannot be resolved (no network, no file), so it is refused here.
    let validator = jsonschema::validator_for(&schema)
        .map_err(|_refusal| invalid(name, "the parameter schema does not compile as a JSON Schema"))?;
    Ok(CompiledSchema { schema, validator })
}

/// Checks the parts of a parameter schema that the structural check reads: an object, a `required` list of names and
/// a `properties` object without a secret-like default. The rest of the schema is compiled as it is.
fn validate_shape(name: &str, schema: &Value, detector: &dyn SecretDetector) -> ToolsetsResult<()> {
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
            if property
                .get("default")
                .is_some_and(|value| value_looks_like_secret(value, detector))
            {
                return Err(invalid(
                    name,
                    "a parameter default looks like a secret: a schema holds references and plain values, never a secret",
                ));
            }
        }
    }
    Ok(())
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
fn declared_names(schemas: &[Arc<CompiledSchema>]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for compiled in schemas {
        collect_declared(&compiled.schema, &mut out);
    }
    out
}

/// Adds to `out` every property name declared under a `properties` keyword of `schema`, at any depth.
///
/// Known limit (decision of 2026-10-31, not changed): any key of an object named `properties` counts as a declared name, even
/// one under `enum`, `const` or `default`, where it is a value and not a declaration. Such a name is then rendered as itself
/// instead of `*`; a value is never rendered, only a name.
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
    use crate::secret::SecretDetector;
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

    // Scenario: inclure un type exige les deux ensembles de champs
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

    // Scenario: un champ optionnel est un changement mineur
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

    // Scenario: un champ requis est un changement majeur
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

    // Scenario: un champ optionnel devenu requis est un changement majeur
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

    // Scenario: défaut ressemblant à un secret refusé
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

    // Scenario: une suppression est un changement majeur
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

    // Scenario: cycle d'inclusion refusé
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

    // Scenario: schéma de plus de 64 Kio refusé sans son contenu
    #[test]
    fn schema_over_64_kib_is_refused_without_its_content() {
        let schema = json!({ "type": "object", "description": "d".repeat(70_000) });
        match ContextType::new("cluster".to_owned(), semver("1.0.0"), schema) {
            Err(error @ ToolsetsError::InvalidContextType { .. }) => assert!(
                !error.to_string().contains("dddd"),
                "the refusal never quotes the schema: {error}"
            ),
            other => panic!("an oversized schema must fail with InvalidContextType, got {other:?}"),
        }
    }

    // Scenario: schéma d'un octet au-delà de 64 Kio refusé
    #[test]
    fn schema_one_byte_over_64_kib_is_refused() {
        let empty = json!({ "type": "object", "description": "" });
        let base = serde_json::to_string(&empty).expect("a schema serializes").len();
        let over = json!({ "type": "object", "description": "d".repeat(65537usize.saturating_sub(base)) });
        assert_eq!(
            serde_json::to_string(&over).expect("a schema serializes").len(),
            65537,
            "the sample is one byte over 64 KiB once serialized"
        );
        assert!(
            matches!(
                ContextType::new("cluster".to_owned(), semver("1.0.0"), over),
                Err(ToolsetsError::InvalidContextType { .. })
            ),
            "a schema one byte over 64 KiB is refused"
        );
    }

    // Scenario: schéma de 64 Kio exactement accepté
    #[test]
    fn schema_of_exactly_64_kib_is_accepted() {
        let empty = json!({ "type": "object", "description": "" });
        let base = serde_json::to_string(&empty).expect("a schema serializes").len();
        let fits = json!({ "type": "object", "description": "d".repeat(65536usize.saturating_sub(base)) });
        assert_eq!(
            serde_json::to_string(&fits).expect("a schema serializes").len(),
            65_536,
            "the sample is exactly 64 KiB once serialized"
        );
        assert!(
            ContextType::new("cluster".to_owned(), semver("1.0.0"), fits).is_ok(),
            "a schema of exactly 64 KiB is accepted"
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

    // Scenario: schéma qui ne compile pas refusé
    #[test]
    fn schema_that_does_not_compile_is_refused() {
        let result = ContextType::new("cluster".to_owned(), semver("1.0.0"), json!({ "type": "foo" }));
        assert!(
            matches!(result, Err(ToolsetsError::InvalidContextType { .. })),
            "a schema that does not compile as JSON Schema must be refused, got {result:?}"
        );
    }

    // Scenario: référence distante refusée
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

    // Scenario: chaque valeur est validée contre chaque schéma aplati
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

    // Scenario: clé non déclarée rendue par un marqueur
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

    // Scenario: un détecteur de l'hôte juge les défauts d'un schéma
    #[test]
    fn host_detector_judges_schema_defaults() {
        struct TokDetector;
        impl SecretDetector for TokDetector {
            fn looks_like_secret(&self, text: &str) -> bool {
                text.starts_with("tok_")
            }
        }
        let schema = || {
            json!({
                "type": "object",
                "properties": { "api_key": { "type": "string", "default": "tok_abc" } },
            })
        };
        let refused =
            ContextType::new_with_detector("cluster".to_owned(), semver("1.0.0"), schema(), &TokDetector);
        assert!(
            matches!(refused, Err(ToolsetsError::InvalidContextType { .. })),
            "the host detector refuses a tok_ default"
        );
        let accepted = ContextType::new("cluster".to_owned(), semver("1.0.0"), schema());
        assert!(accepted.is_ok(), "the default heuristic accepts a tok_ default");
        let replaced = json!({
            "type": "object",
            "properties": { "api_key": { "type": "string", "default": "sk-abc" } },
        });
        let kept =
            ContextType::new_with_detector("cluster".to_owned(), semver("1.0.0"), replaced, &TokDetector);
        assert!(
            kept.is_ok(),
            "the host detector replaces the default: sk- is accepted"
        );
    }
}
