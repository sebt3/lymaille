//! Toolset context: a flat set of named parameters and role bindings, with a section of overrides per toolset. It is a
//! named entity of one level, and it holds no secret: parameters and references only (`agent-sessions/toolsets#FR-3`,
//! `FR-5`, `FR-17`, `agent-sessions#DEC-5`, `agent-sessions#CON-3`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{ToolsetsError, ToolsetsResult};
use crate::secret::looks_like_secret;

/// Level at which a toolset context lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextLevel {
    /// The user level.
    User,
    /// The project level.
    Project,
    /// The session level.
    Session,
}

/// Named toolset context. It holds parameters and references only: a value that looks like a secret is refused.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ToolsetContext {
    name: String,
    level: ContextLevel,
    parameters: BTreeMap<String, Value>,
    roles: BTreeMap<String, String>,
    overrides: BTreeMap<String, BTreeMap<String, Value>>,
}

/// Values resolved for a session.
///
/// It holds the parameters of each toolset, the parameters shared with the rest of the context, and the sorted names of
/// the bound roles. The references of the roles do not appear here.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ResolvedContext {
    toolsets: BTreeMap<String, BTreeMap<String, Value>>,
    shared: BTreeMap<String, Value>,
    roles: Vec<String>,
}

impl ResolvedContext {
    /// Returns the resolved parameters of each toolset, by toolset name then parameter name.
    #[must_use]
    pub const fn toolsets(&self) -> &BTreeMap<String, BTreeMap<String, Value>> {
        &self.toolsets
    }

    /// Returns the parameters of the context that belong to no toolset in particular.
    #[must_use]
    pub const fn shared(&self) -> &BTreeMap<String, Value> {
        &self.shared
    }

    /// Returns the sorted names of the bound roles.
    #[must_use]
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
}

impl ToolsetContext {
    /// Builds an empty context.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContext`] when the name is empty.
    pub const fn new(name: String, level: ContextLevel) -> ToolsetsResult<Self> {
        if name.is_empty() {
            return Err(ToolsetsError::InvalidContext {
                name,
                reason: "the context name is empty",
            });
        }
        Ok(Self {
            name,
            level,
            parameters: BTreeMap::new(),
            roles: BTreeMap::new(),
            overrides: BTreeMap::new(),
        })
    }

    /// Adds a parameter, replacing an earlier value of the same name.
    ///
    /// Returns the name of the context. Read by the storage, which keys the context by its name.
    pub(in crate::toolsets) fn name(&self) -> &str {
        &self.name
    }

    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContext`] when the name is empty or when the value looks like a secret.
    pub fn with_parameter(mut self, name: String, value: Value) -> ToolsetsResult<Self> {
        check_entry(&name, &value)?;
        let _previous = self.parameters.insert(name, value);
        Ok(self)
    }

    /// Binds a role to a reference that the host resolves.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContext`] when the role name is empty, or when the reference looks like a secret.
    pub fn with_role(mut self, role: String, reference: String) -> ToolsetsResult<Self> {
        if role.is_empty() {
            return Err(ToolsetsError::InvalidContext {
                name: role,
                reason: "the role name is empty",
            });
        }
        if looks_like_secret(&reference) {
            return Err(ToolsetsError::InvalidContext {
                name: role,
                reason: "the reference looks like a secret: a context holds references the host resolves, never a secret",
            });
        }
        let _previous = self.roles.insert(role, reference);
        Ok(self)
    }

    /// Sets an override of one parameter for one toolset.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContext`] when the toolset or parameter name is empty, or when the value looks
    /// like a secret.
    pub fn with_override(mut self, toolset: String, name: String, value: Value) -> ToolsetsResult<Self> {
        if toolset.is_empty() {
            return Err(ToolsetsError::InvalidContext {
                name: toolset,
                reason: "the toolset name of an override is empty",
            });
        }
        check_entry(&name, &value)?;
        let _previous = self.overrides.entry(toolset).or_default().insert(name, value);
        Ok(self)
    }

    /// Resolves the context against the default values declared by the toolsets of the agent, given as
    /// `(toolset, parameter, value)`. For a toolset parameter, the override wins over the context parameter, which wins
    /// over the default. Two defaults for the same toolset and parameter are refused, never settled silently.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidContext`] when a toolset or parameter name is empty, when a default value looks
    /// like a secret, or when two defaults declare the same parameter of the same toolset. The name of a duplicate error
    /// is `toolset.parameter`.
    pub fn resolve<I>(&self, defaults: I) -> ToolsetsResult<ResolvedContext>
    where
        I: IntoIterator<Item = (String, String, Value)>,
    {
        let mut defaults_by_toolset: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        for (toolset, name, value) in defaults {
            if toolset.is_empty() {
                return Err(ToolsetsError::InvalidContext {
                    name: toolset,
                    reason: "the toolset name of a default is empty",
                });
            }
            check_entry(&name, &value)?;
            let table = defaults_by_toolset.entry(toolset.clone()).or_default();
            if table.contains_key(&name) {
                return Err(ToolsetsError::InvalidContext {
                    name: format!("{toolset}.{name}"),
                    reason: "two defaults declare the same parameter of the same toolset",
                });
            }
            let _previous = table.insert(name, value);
        }
        let toolset_names: BTreeSet<String> = defaults_by_toolset
            .keys()
            .chain(self.overrides.keys())
            .cloned()
            .collect();
        let mut toolsets = BTreeMap::new();
        for toolset in toolset_names {
            // Lowest priority first: the default, then the context parameter, then the override of the toolset.
            let mut table = defaults_by_toolset.get(&toolset).cloned().unwrap_or_default();
            for (name, value) in &self.parameters {
                let _previous = table.insert(name.clone(), value.clone());
            }
            let overrides = self.overrides.get(&toolset).into_iter().flatten();
            for (name, value) in overrides {
                let _previous = table.insert(name.clone(), value.clone());
            }
            let _previous = toolsets.insert(toolset, table);
        }
        let roles: Vec<String> = self.roles.keys().cloned().collect();
        Ok(ResolvedContext {
            toolsets,
            shared: self.parameters.clone(),
            roles,
        })
    }
}

impl<'de> Deserialize<'de> for ToolsetContext {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Record {
            name: String,
            level: ContextLevel,
            parameters: BTreeMap<String, Value>,
            roles: BTreeMap<String, String>,
            overrides: BTreeMap<String, BTreeMap<String, Value>>,
        }
        let record = Record::deserialize(deserializer)?;
        let fail = |error: ToolsetsError| <D::Error as serde::de::Error>::custom(error);
        let mut built = Self::new(record.name, record.level).map_err(fail)?;
        for (name, value) in record.parameters {
            built = built.with_parameter(name, value).map_err(fail)?;
        }
        for (role, reference) in record.roles {
            built = built.with_role(role, reference).map_err(fail)?;
        }
        for (toolset, entries) in record.overrides {
            for (name, value) in entries {
                built = built.with_override(toolset.clone(), name, value).map_err(fail)?;
            }
        }
        Ok(built)
    }
}

/// Checks one parameter or override entry: the name is not empty and the value does not look like a secret. The error
/// names the parameter and never carries the value.
fn check_entry(name: &str, value: &Value) -> ToolsetsResult<()> {
    if name.is_empty() {
        return Err(ToolsetsError::InvalidContext {
            name: name.to_owned(),
            reason: "the parameter name is empty",
        });
    }
    if value_looks_like_secret(value) {
        return Err(ToolsetsError::InvalidContext {
            name: name.to_owned(),
            reason: "the value looks like a secret: a context holds references and parameters, never a secret",
        });
    }
    Ok(())
}

/// Tells whether a JSON value holds, at any depth in its strings, a text that looks like a secret.
pub(in crate::toolsets) fn value_looks_like_secret(value: &Value) -> bool {
    match value {
        Value::String(text) => looks_like_secret(text),
        Value::Array(items) => items.iter().any(value_looks_like_secret),
        Value::Object(entries) => entries.values().any(value_looks_like_secret),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{ContextLevel, ToolsetContext};
    use crate::toolsets::ToolsetsError;

    /// The defaults of one toolset parameter, in the form `resolve` takes them.
    fn default_of(toolset: &str, name: &str, value: Value) -> (String, String, Value) {
        (toolset.to_owned(), name.to_owned(), value)
    }

    fn value_of(resolved: &super::ResolvedContext, toolset: &str, name: &str) -> Option<Value> {
        resolved
            .toolsets()
            .get(toolset)
            .and_then(|parameters| parameters.get(name))
            .cloned()
    }

    // Scenario: override beats context beats default
    #[test]
    fn override_beats_context_beats_default() {
        let defaults = || vec![default_of("tool", "limit", json!(5))];

        let with_all = ToolsetContext::new("ctx".to_owned(), ContextLevel::Project)
            .expect("a named context builds")
            .with_parameter("limit".to_owned(), json!(7))
            .expect("a plain value is accepted")
            .with_override("tool".to_owned(), "limit".to_owned(), json!(9))
            .expect("a plain override is accepted");
        let resolved_all = with_all.resolve(defaults()).expect("the context resolves");
        assert_eq!(
            value_of(&resolved_all, "tool", "limit"),
            Some(json!(9)),
            "the override wins for that toolset"
        );

        let without_override = ToolsetContext::new("ctx".to_owned(), ContextLevel::Project)
            .expect("a named context builds")
            .with_parameter("limit".to_owned(), json!(7))
            .expect("a plain value is accepted");
        let resolved_override = without_override
            .resolve(defaults())
            .expect("the context resolves");
        assert_eq!(
            value_of(&resolved_override, "tool", "limit"),
            Some(json!(7)),
            "without the override, the context parameter wins over the default"
        );

        let without_either =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::Project).expect("a named context builds");
        let resolved_either = without_either.resolve(defaults()).expect("the context resolves");
        assert_eq!(
            value_of(&resolved_either, "tool", "limit"),
            Some(json!(5)),
            "without either, the default of the toolset is used"
        );
    }

    // Scenario: only role names come out
    #[test]
    fn only_role_names_come_out() {
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::User)
            .expect("a named context builds")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("a role binding is accepted");
        let resolved = context.resolve(Vec::new()).expect("the context resolves");
        assert_eq!(
            resolved.roles(),
            ["k8s".to_owned()].as_slice(),
            "the resolved context lists the role name"
        );
        let text = format!("{resolved:?}");
        assert!(
            !text.contains("prod-k8s"),
            "the reference of the role must never come out: {text}"
        );
        let serialized = serde_json::to_string(&resolved).expect("a resolved context serializes");
        assert!(
            !serialized.contains("prod-k8s"),
            "the serialized resolved context must not carry the reference: {serialized}"
        );
    }

    // Scenario: secret-like value refused
    #[test]
    fn secret_like_value_refused() {
        let result = ToolsetContext::new("ctx".to_owned(), ContextLevel::User)
            .expect("a named context builds")
            .with_parameter("api_key".to_owned(), json!("sk-abcdef"));
        match result {
            Err(error @ ToolsetsError::InvalidContext { .. }) => {
                let text = error.to_string();
                assert!(
                    text.contains("api_key"),
                    "the message names the parameter: {text}"
                );
                assert!(
                    !text.contains("sk-abcdef"),
                    "the message never carries the value: {text}"
                );
            }
            other => panic!("a secret-like value must fail with InvalidContext, got {other:?}"),
        }
    }

    // Scenario: resolved copy is independent
    #[test]
    fn resolved_copy_is_independent() {
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::Session)
            .expect("a named context builds")
            .with_parameter("region".to_owned(), json!("eu"))
            .expect("a plain value is accepted");
        let mut first = context.resolve(Vec::new()).expect("the context resolves");
        let _previous = first.shared.insert("region".to_owned(), json!("modified"));
        let second = context.resolve(Vec::new()).expect("the context resolves again");
        assert_eq!(
            second.shared().get("region"),
            Some(&json!("eu")),
            "a second resolution still returns the original value"
        );
    }

    #[test]
    fn empty_names_refused() {
        assert!(
            matches!(
                ToolsetContext::new(String::new(), ContextLevel::User),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "an empty context name is refused"
        );
        let context =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::User).expect("a named context builds");
        assert!(
            matches!(
                context.clone().with_parameter(String::new(), json!("eu")),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "an empty parameter name is refused"
        );
        assert!(
            matches!(
                context.clone().with_role(String::new(), "prod-k8s".to_owned()),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "an empty role name is refused"
        );
        assert!(
            matches!(
                context.with_override("tool".to_owned(), String::new(), json!(1)),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "an empty override parameter name is refused"
        );
    }

    #[test]
    fn credentials_in_url_and_bearer_refused_even_nested() {
        let context =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::User).expect("a named context builds");
        assert!(
            matches!(
                context
                    .clone()
                    .with_parameter("endpoint".to_owned(), json!("https://user:pw@host/v1")),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "credentials in clear in a URL are refused"
        );
        assert!(
            matches!(
                context.with_parameter("headers".to_owned(), json!(["ok", { "auth": "Bearer abc" }])),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "a bearer token nested in an array and an object is refused"
        );
    }

    #[test]
    fn defaults_are_checked_too() {
        let context =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::User).expect("a named context builds");
        assert!(
            matches!(
                context.resolve(vec![default_of("tool", "token", json!("SK-abc"))]),
                Err(ToolsetsError::InvalidContext { .. })
            ),
            "a default value that looks like a secret is refused"
        );
    }

    // Must: a role reference that looks like a secret is refused, like a parameter value.
    #[test]
    fn secret_like_role_reference_refused() {
        let context =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::User).expect("a named context builds");
        match context.with_role("k8s".to_owned(), "https://user:pw@cluster.example/".to_owned()) {
            Err(error @ ToolsetsError::InvalidContext { .. }) => {
                let text = error.to_string();
                assert!(text.contains("k8s"), "the message names the role: {text}");
                assert!(
                    !text.contains("pw@"),
                    "the message never carries the reference: {text}"
                );
            }
            other => panic!("a secret-like role reference must fail with InvalidContext, got {other:?}"),
        }
    }

    // Must: two defaults for the same (toolset, parameter) are refused, never settled silently.
    #[test]
    fn duplicate_default_refused() {
        let context =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::User).expect("a named context builds");
        let result = context.resolve(vec![
            default_of("tool", "limit", json!(5)),
            default_of("tool", "limit", json!(6)),
        ]);
        assert!(
            matches!(result, Err(ToolsetsError::InvalidContext { .. })),
            "a duplicated default must fail with InvalidContext"
        );
    }

    #[test]
    fn defaults_of_distinct_toolsets_are_not_duplicates() {
        let context =
            ToolsetContext::new("ctx".to_owned(), ContextLevel::User).expect("a named context builds");
        let result = context.resolve(vec![
            default_of("tool", "limit", json!(5)),
            default_of("other", "limit", json!(6)),
        ]);
        assert!(
            result.is_ok(),
            "the same parameter name in two toolsets is not a duplicate"
        );
    }

    #[test]
    fn serialization_round_trip_is_identical() {
        let context = ToolsetContext::new("ctx".to_owned(), ContextLevel::Project)
            .expect("a named context builds")
            .with_parameter("region".to_owned(), json!("eu"))
            .expect("a plain value is accepted")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("a role binding is accepted")
            .with_override("tool".to_owned(), "limit".to_owned(), json!(9))
            .expect("a plain override is accepted");
        let text = serde_json::to_string(&context).expect("a context serializes");
        let back: ToolsetContext = serde_json::from_str(&text).expect("a context deserializes");
        assert_eq!(back, context, "the round trip must be identical");
    }

    #[test]
    fn deserialization_refuses_a_secret() {
        let text =
            r#"{"name":"ctx","level":"user","parameters":{"api_key":"sk-abcdef"},"roles":{},"overrides":{}}"#;
        assert!(
            serde_json::from_str::<ToolsetContext>(text).is_err(),
            "a secret-like value must fail to deserialize"
        );
    }
}
