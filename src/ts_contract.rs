// Contract tests of the TypeScript declarations produced by `ts-rs`: the in-memory declaration of every root
// definition type describes the same keys and the same optionality as the serde JSON of the same value
// (`agent-sessions#NFR-4`, `src/ts_contract.sdd`). The module is compiled under `cfg(all(test, feature = "ts-rs"))`
// only. It produces no file: declarations are read through `TS::decl` and `TS::inline`, never through an export.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::num::NonZeroU64;
    use std::time::Duration;

    use serde_json::{Value, json};
    use ts_rs::{Config, TS};
    use vynil_core::Semver;

    use crate::{
        AgentDefinition, AgentKind, ContextLevel, ContextType, ModelProfile, ParamDecl, Provider,
        ProviderLevel, ReplayMode, RoleDecl, RoleSpec, SamplingParams, SandboxLimits, SessionKind, Skill,
        Toolset, ToolsetContext, TypeRequirement,
    };

    /// The plain `ts-rs` configuration: its large-integer default is `bigint`, so a 64-bit field the drifts forgot
    /// to declare `number` would still show as `bigint` here and be caught.
    fn config() -> Config {
        Config::new()
    }

    /// Which keys a value must write: every declared key, or exactly the keys serde never omits.
    #[derive(Clone, Copy)]
    enum Presence {
        Complete,
        Minimal,
    }

    /// A field parsed out of a `{ ... }` TypeScript object declaration.
    struct DeclaredField {
        name: String,
        optional: bool,
        ty: String,
    }

    /// The shape a TypeScript type expression describes.
    enum Shape {
        /// A `{ ... }` object declaration, compared by key with the JSON object.
        Object(String),
        /// A `{ [key in string]: V }` table: free keys, values sharing one expression.
        Map(String),
        /// An `Array<T>`: items sharing one expression.
        Array(String),
        /// A primitive, an enumeration or a free-form JSON value: nothing to compare by key.
        Leaf,
    }

    /// The inline declaration of a named contract type, or `None` for any other expression.
    fn inline_of(name: &str) -> Option<String> {
        let config = config();
        let inlined = match name {
            "AgentDefinition" => <AgentDefinition as TS>::inline(&config),
            "AgentKind" => <AgentKind as TS>::inline(&config),
            "SandboxLimits" => <SandboxLimits as TS>::inline(&config),
            "SamplingParams" => <SamplingParams as TS>::inline(&config),
            "ModelProfile" => <ModelProfile as TS>::inline(&config),
            "Provider" => <Provider as TS>::inline(&config),
            "ProviderLevel" => <ProviderLevel as TS>::inline(&config),
            "Toolset" => <Toolset as TS>::inline(&config),
            "Skill" => <Skill as TS>::inline(&config),
            "TypeRequirement" => <TypeRequirement as TS>::inline(&config),
            "ParamDecl" => <ParamDecl as TS>::inline(&config),
            "RoleDecl" => <RoleDecl as TS>::inline(&config),
            "ToolsetContext" => <ToolsetContext as TS>::inline(&config),
            "ContextLevel" => <ContextLevel as TS>::inline(&config),
            "ContextType" => <ContextType as TS>::inline(&config),
            "RoleSpec" => <RoleSpec as TS>::inline(&config),
            "SessionKind" => <SessionKind as TS>::inline(&config),
            "ReplayMode" => <ReplayMode as TS>::inline(&config),
            _ => return None,
        };
        Some(inlined)
    }

    /// A contract type with its declaration and its inline form, both read in memory.
    type Declaration = (&'static str, fn(&Config) -> String, fn(&Config) -> String);

    /// The name, declaration and inline form of every contract type, in memory only.
    fn declarations() -> Vec<(&'static str, String, String)> {
        let config = config();
        let types: Vec<Declaration> = vec![
            (
                "AgentDefinition",
                <AgentDefinition as TS>::decl,
                <AgentDefinition as TS>::inline,
            ),
            ("AgentKind", <AgentKind as TS>::decl, <AgentKind as TS>::inline),
            (
                "SandboxLimits",
                <SandboxLimits as TS>::decl,
                <SandboxLimits as TS>::inline,
            ),
            (
                "SamplingParams",
                <SamplingParams as TS>::decl,
                <SamplingParams as TS>::inline,
            ),
            (
                "ModelProfile",
                <ModelProfile as TS>::decl,
                <ModelProfile as TS>::inline,
            ),
            ("Provider", <Provider as TS>::decl, <Provider as TS>::inline),
            (
                "ProviderLevel",
                <ProviderLevel as TS>::decl,
                <ProviderLevel as TS>::inline,
            ),
            ("Toolset", <Toolset as TS>::decl, <Toolset as TS>::inline),
            ("Skill", <Skill as TS>::decl, <Skill as TS>::inline),
            (
                "TypeRequirement",
                <TypeRequirement as TS>::decl,
                <TypeRequirement as TS>::inline,
            ),
            ("ParamDecl", <ParamDecl as TS>::decl, <ParamDecl as TS>::inline),
            ("RoleDecl", <RoleDecl as TS>::decl, <RoleDecl as TS>::inline),
            (
                "ToolsetContext",
                <ToolsetContext as TS>::decl,
                <ToolsetContext as TS>::inline,
            ),
            (
                "ContextLevel",
                <ContextLevel as TS>::decl,
                <ContextLevel as TS>::inline,
            ),
            (
                "ContextType",
                <ContextType as TS>::decl,
                <ContextType as TS>::inline,
            ),
            ("RoleSpec", <RoleSpec as TS>::decl, <RoleSpec as TS>::inline),
            (
                "SessionKind",
                <SessionKind as TS>::decl,
                <SessionKind as TS>::inline,
            ),
            ("ReplayMode", <ReplayMode as TS>::decl, <ReplayMode as TS>::inline),
        ];
        types
            .into_iter()
            .map(|(name, decl, inline)| (name, decl(&config), inline(&config)))
            .collect()
    }

    /// The Rust value of a fixture, as the JSON serde writes it.
    fn written(value: impl serde::Serialize) -> Value {
        serde_json::to_value(value).expect("the fixture serializes")
    }

    /// The six root types with a value whose every optional field and collection is set. The nested types
    /// `Skill`, `TypeRequirement`, `ParamDecl`, `RoleDecl`, `RoleSpec`, `ContextLevel`, `SamplingParams`,
    /// `ReplayMode`, `AgentKind`, `SandboxLimits`, `ProviderLevel` and `SessionKind` are each reached through one
    /// of them.
    fn complete_root_values() -> Vec<(&'static str, Value)> {
        vec![
            ("AgentDefinition", written(full_definition())),
            ("ModelProfile", written(full_model_profile())),
            ("Provider", written(full_provider())),
            ("Toolset", written(full_toolset())),
            ("ContextType", written(full_context_type())),
            ("ToolsetContext", written(full_context())),
        ]
    }

    /// The six root types with a value carrying no optional field at all.
    fn minimal_root_values() -> Vec<(&'static str, Value)> {
        vec![
            ("AgentDefinition", written(minimal_definition())),
            ("ModelProfile", written(minimal_model_profile())),
            ("Provider", written(minimal_provider())),
            ("Toolset", written(minimal_toolset())),
            ("ContextType", written(minimal_context_type())),
            ("ToolsetContext", written(minimal_context())),
        ]
    }

    fn check_roots(mode: Presence) {
        let values = match mode {
            Presence::Complete => complete_root_values(),
            Presence::Minimal => minimal_root_values(),
        };
        for (name, value) in &values {
            check_value(value, name, name, mode);
        }
    }

    /// Recursively compares a JSON value with the declaration of its type, at `path`.
    fn check_value(json: &Value, expression: &str, path: &str, mode: Presence) {
        match resolve(expression) {
            Shape::Object(object_decl) => check_object(json, &object_decl, path, mode),
            Shape::Map(value_decl) => check_table_values(json, &value_decl, path, mode),
            Shape::Array(item_decl) => check_array_items(json, &item_decl, path, mode),
            Shape::Leaf => {}
        }
    }

    fn check_object(json: &Value, object_decl: &str, path: &str, mode: Presence) {
        assert!(
            json.is_object(),
            "{path}: the declaration is an object but the JSON value is {json}"
        );
        let fields = parse_fields(object_decl);
        let json_keys: BTreeSet<&str> = json
            .as_object()
            .into_iter()
            .flat_map(|entries| entries.keys().map(String::as_str))
            .collect();
        let declared_keys: BTreeSet<&str> = fields.iter().map(|field| field.name.as_str()).collect();
        let required_keys: BTreeSet<&str> = fields
            .iter()
            .filter(|field| !field.optional)
            .map(|field| field.name.as_str())
            .collect();
        match mode {
            Presence::Complete => assert_eq!(
                json_keys, declared_keys,
                "{path}: a complete value must write every declared key"
            ),
            Presence::Minimal => assert_eq!(
                json_keys, required_keys,
                "{path}: a minimal value must omit exactly the keys the declaration marks optional"
            ),
        }
        for field in &fields {
            if let Some(value) = json.get(&field.name) {
                let child = format!("{path}.{}", field.name);
                assert!(
                    !value.is_null() || allows_null(&field.ty),
                    "{child}: the JSON writes null but the declaration is `{}`",
                    field.ty
                );
                check_value(value, &field.ty, &child, mode);
            }
        }
    }

    fn check_table_values(json: &Value, value_decl: &str, path: &str, mode: Presence) {
        assert!(
            json.is_object(),
            "{path}: the declaration is a table but the JSON value is {json}"
        );
        json.as_object().into_iter().for_each(|entries| {
            for (key, value) in entries {
                check_value(value, value_decl, &format!("{path}.{key}"), mode);
            }
        });
    }

    fn check_array_items(json: &Value, item_decl: &str, path: &str, mode: Presence) {
        assert!(
            json.is_array(),
            "{path}: the declaration is an array but the JSON value is {json}"
        );
        json.as_array().into_iter().for_each(|items| {
            for (index, item) in items.iter().enumerate() {
                check_value(item, item_decl, &format!("{path}[{index}]"), mode);
            }
        });
    }

    /// Reduces a type expression, replacing named types by their declaration.
    fn resolve(expression: &str) -> Shape {
        let expression = strip_null(expression.trim());
        if expression.starts_with('{') {
            return map_value(expression).map_or_else(|| Shape::Object(expression.to_owned()), Shape::Map);
        }
        if let Some(item) = expression
            .strip_prefix("Array<")
            .and_then(|rest| rest.strip_suffix('>'))
        {
            return Shape::Array(item.to_owned());
        }
        inline_of(expression).map_or(Shape::Leaf, |inlined| resolve(&inlined))
    }

    /// Removes a trailing ` | null` of a type expression.
    fn strip_null(expression: &str) -> &str {
        expression.strip_suffix(" | null").unwrap_or(expression)
    }

    /// Tells whether a type expression accepts `null`.
    fn allows_null(expression: &str) -> bool {
        expression.split('|').any(|part| part.trim() == "null")
    }

    /// The value expression of a `{ [key in string]: V }` table, or `None` for a plain object.
    fn map_value(object_decl: &str) -> Option<String> {
        let (_, after_marker) = object_decl.split_once("[key in string]")?;
        let (_, value) = after_marker.split_once(':')?;
        Some(value.trim().strip_suffix('}')?.trim().to_owned())
    }

    /// Parses the fields of a `{ ... }` object declaration, dropping the documentation comments.
    fn parse_fields(object_decl: &str) -> Vec<DeclaredField> {
        let body = strip_docs(object_decl);
        let inner = body.trim().trim_start_matches('{').trim_end_matches('}');
        split_top_level(inner)
            .iter()
            .filter_map(|piece| {
                let (lhs, ty) = piece.trim().split_once(':')?;
                let lhs = lhs.trim();
                Some(DeclaredField {
                    name: lhs.trim_end_matches('?').trim().to_owned(),
                    optional: lhs.ends_with('?'),
                    ty: ty.trim().to_owned(),
                })
            })
            .collect()
    }

    /// Splits on commas that no bracket contains.
    fn split_top_level(text: &str) -> Vec<String> {
        let mut pieces = Vec::new();
        let mut current = String::new();
        let mut depth: i32 = 0;
        for character in text.chars() {
            match character {
                '{' | '[' | '<' | '(' => depth = depth.saturating_add(1),
                '}' | ']' | '>' | ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    pieces.push(std::mem::take(&mut current));
                    continue;
                }
                _ => {}
            }
            current.push(character);
        }
        pieces.push(current);
        pieces
    }

    /// Removes every `/** ... */` block of a declaration.
    fn strip_docs(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some((before, tail)) = rest.split_once("/**") {
            out.push_str(before);
            let (_, after) = tail.split_once("*/").unwrap_or(("", tail));
            rest = after;
        }
        out.push_str(rest);
        out
    }

    // --- fixtures, built by the public constructors ---

    fn semver(text: &str) -> Semver {
        Semver::opt_parse(text).expect("the fixture version parses")
    }

    fn sampling() -> SamplingParams {
        SamplingParams::default()
            .with_temperature(0.7)
            .with_top_p(0.9)
            .with_top_k(40)
            .with_extra("seed".to_owned(), json!(42))
    }

    fn full_model_profile() -> ModelProfile {
        ModelProfile::new("strix-qwen".to_owned(), "strix".to_owned(), "qwen".to_owned())
            .with_context_size(262_144)
            .with_max_response(8_192)
            .with_tools(true)
            .with_sampling(sampling())
            .with_tokenization_hint(NonZeroU64::new(2_500).expect("a nonzero hint"))
            .with_concurrency(NonZeroU64::new(4).expect("a nonzero concurrency"))
            .with_replay(ReplayMode::IntraTurn)
    }

    fn minimal_model_profile() -> ModelProfile {
        ModelProfile::new("p".to_owned(), "prov".to_owned(), "m".to_owned())
    }

    fn sandbox() -> SandboxLimits {
        SandboxLimits::new(Duration::from_secs(30), 10_000)
            .with_max_string_bytes(1_000)
            .with_max_array_len(500)
            .with_max_map_len(200)
    }

    fn full_definition() -> AgentDefinition {
        // The complete value fills every optional field and every collection, as the contract requires. The
        // Work/output-schema combination is refused by `AgentDefinition::validate`, a domain rule of the agents spec:
        // this contract is about the serialized shape versus the declaration, and the public constructor accepts it.
        AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Work,
            "You are the assistant.".to_owned(),
        )
        .with_profile("strix-qwen".to_owned())
        .with_sampling(sampling())
        .with_modules(vec!["files".to_owned()])
        .with_toolsets(vec!["k8s".to_owned()])
        .with_sandbox(sandbox())
        .with_context_strategy("summarize".to_owned())
        .with_delegable(vec!["reviewer".to_owned()])
        .with_output_schema(json!({"type": "object"}))
    }

    fn minimal_definition() -> AgentDefinition {
        AgentDefinition::new(
            "main".to_owned(),
            AgentKind::Work,
            "You are the assistant.".to_owned(),
        )
    }

    fn full_provider() -> Provider {
        Provider::new(
            "strix".to_owned(),
            "openai".to_owned(),
            "ref:strix-address".to_owned(),
            Some("ref:strix-auth".to_owned()),
            ProviderLevel::Shared,
        )
        .expect("the fixture provider is built")
    }

    fn minimal_provider() -> Provider {
        Provider::new(
            "prov".to_owned(),
            "openai".to_owned(),
            "ref:address".to_owned(),
            None,
            ProviderLevel::User,
        )
        .expect("the fixture provider is built")
    }

    fn skill() -> Skill {
        Skill::new(
            "my-skill".to_owned(),
            "Does things.".to_owned(),
            "Do it.".to_owned(),
        )
        .expect("the fixture skill is built")
        .with_license("BSD-3-Clause".to_owned())
        .with_compatibility("any Rhai scratchpad".to_owned())
        .with_metadata("owner".to_owned(), "team".to_owned())
        .expect("the metadata is accepted")
        .with_annex("queries".to_owned(), "PromQL examples".to_owned())
        .expect("the annex is accepted")
    }

    fn full_toolset() -> Toolset {
        Toolset::new(
            "k8s".to_owned(),
            "The Kubernetes toolset.".to_owned(),
            semver("1.2.3"),
            "fn go() { \"ok\" }".to_owned(),
        )
        .expect("the fixture toolset is built")
        .with_skill(skill())
        .expect("the skill is accepted")
        .with_type_requirement(
            TypeRequirement::new("cluster".to_owned(), semver("1.2.0")).expect("the requirement is built"),
        )
        .expect("the requirement is accepted")
        .with_param(
            ParamDecl::new("namespace".to_owned(), json!({"type": "string"})).with_default(json!("default")),
        )
        .expect("the parameter is accepted")
        .with_param(ParamDecl::new("region".to_owned(), json!({"type": "string"})))
        .expect("the parameter is accepted")
        .with_role(RoleDecl::new("ops".to_owned(), true))
        .expect("the role is accepted")
        .with_dependency("files".to_owned())
        .expect("the dependency is accepted")
    }

    fn minimal_toolset() -> Toolset {
        Toolset::new(
            "k8s".to_owned(),
            "The Kubernetes toolset.".to_owned(),
            semver("0.1.0"),
            String::new(),
        )
        .expect("the fixture toolset is built")
    }

    fn full_context() -> ToolsetContext {
        ToolsetContext::new("default".to_owned(), ContextLevel::Session)
            .expect("the fixture context is built")
            .with_parameter("namespace".to_owned(), json!("default"))
            .expect("the parameter is accepted")
            .with_role("ops".to_owned(), "ref:ops-role".to_owned())
            .expect("the role is accepted")
            .with_override("k8s".to_owned(), "namespace".to_owned(), json!("other"))
            .expect("the override is accepted")
    }

    fn minimal_context() -> ToolsetContext {
        ToolsetContext::new("default".to_owned(), ContextLevel::User).expect("the fixture context is built")
    }

    fn full_context_type() -> ContextType {
        ContextType::new("cluster".to_owned(), semver("2.0.0"), json!({"type": "object"}))
            .expect("the fixture context type is built")
            .with_role(RoleSpec::new("ops".to_owned(), true))
            .expect("the role is accepted")
            .with_include(
                TypeRequirement::new("base".to_owned(), semver("1.0.0")).expect("the requirement is built"),
            )
            .with_default_agent("main".to_owned())
            .expect("the default agent is accepted")
            .with_session_kind(SessionKind::Config)
            .expect("the session kind is accepted")
    }

    fn minimal_context_type() -> ContextType {
        ContextType::new("plain".to_owned(), semver("0.1.0"), json!({}))
            .expect("the fixture context type is built")
    }

    /// The text serde writes for an enumeration variant.
    fn serde_text(value: impl serde::Serialize) -> String {
        match written(value) {
            Value::String(text) => text,
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
                panic!("an enumeration variant must serialize as text")
            }
        }
    }

    // --- the five Scenarios of `src/ts_contract.sdd` ---

    // Scenario: les clés d'une valeur complète sont exactement celles de la déclaration
    #[test]
    fn complete_values_have_exactly_the_declared_keys() {
        check_roots(Presence::Complete);
        // Must: no file is produced — every declaration is read in memory as a plain `type` alias.
        for (name, declaration, _) in declarations() {
            assert!(
                declaration.starts_with("type ") && !declaration.contains("export"),
                "{name}: the declaration must be read in memory, never exported to a file"
            );
        }
    }

    // Scenario: une valeur minimale omet exactement les clés optionnelles
    #[test]
    fn minimal_values_omit_exactly_the_optional_keys() {
        check_roots(Presence::Minimal);
    }

    // Scenario: les entiers 64 bits sont des nombres
    #[test]
    fn sixty_four_bit_integers_are_declared_as_numbers() {
        let config = config();
        let profile_json = written(full_model_profile());
        let sandbox_json = written(full_definition());
        assert!(
            profile_json.get("context_size").is_some_and(Value::is_number),
            "the JSON writes the context size 262144 as a number"
        );
        assert!(
            sandbox_json
                .get("sandbox")
                .and_then(|limits| limits.get("max_string_bytes"))
                .is_some_and(Value::is_number),
            "the JSON writes the sandbox ceiling 1000 as a number"
        );
        assert!(
            <ModelProfile as TS>::decl(&config).contains("context_size?: number"),
            "a 64-bit context size is declared as a JSON number"
        );
        let limits_decl = <SandboxLimits as TS>::decl(&config);
        assert!(
            limits_decl.contains("render_cap_chars: number")
                && limits_decl.contains("max_string_bytes?: number"),
            "the 64-bit sandbox ceilings are declared as JSON numbers"
        );
        for (name, declaration, inlined) in declarations() {
            assert!(
                !declaration.contains("bigint") && !inlined.contains("bigint"),
                "{name}: no 64-bit integer may be declared as bigint: {declaration}"
            );
        }
    }

    // Scenario: une énumération se déclare par ses valeurs serde
    #[test]
    fn enums_are_declared_by_their_serde_values() {
        let variants: Vec<(&str, Vec<String>)> = vec![
            (
                "ContextLevel",
                vec![
                    serde_text(ContextLevel::User),
                    serde_text(ContextLevel::Shared),
                    serde_text(ContextLevel::Session),
                ],
            ),
            (
                "ProviderLevel",
                vec![serde_text(ProviderLevel::User), serde_text(ProviderLevel::Shared)],
            ),
            (
                "AgentKind",
                vec![
                    serde_text(AgentKind::Work),
                    serde_text(AgentKind::Config),
                    serde_text(AgentKind::Sub),
                ],
            ),
            (
                "ReplayMode",
                vec![
                    serde_text(ReplayMode::None),
                    serde_text(ReplayMode::IntraTurn),
                    serde_text(ReplayMode::AllTurns),
                ],
            ),
        ];
        for (name, values) in &variants {
            let declaration = inline_of(name).expect("the enum is a contract type");
            let declared: BTreeSet<&str> = declaration.split('|').map(str::trim).collect();
            let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
            let expected: BTreeSet<&str> = quoted.iter().map(String::as_str).collect();
            assert_eq!(
                declared, expected,
                "{name}: the declaration must list exactly the values serde writes"
            );
            assert!(
                !declaration.contains("\"project\""),
                "{name}: the reading alias project must stay out of the declaration"
            );
            assert!(
                !declaration.contains("\"main\""),
                "{name}: the reading alias main must stay out of the declaration"
            );
        }
    }

    // Scenario: aucun champ de service dans une déclaration
    #[test]
    fn service_fields_are_absent_from_declarations() {
        let config = config();
        let service_types: [(&str, String); 3] = [
            ("Toolset", <Toolset as TS>::decl(&config)),
            ("ToolsetContext", <ToolsetContext as TS>::decl(&config)),
            ("ContextType", <ContextType as TS>::decl(&config)),
        ];
        for (name, declaration) in &service_types {
            assert!(
                !declaration.contains("detector"),
                "{name}: the secret detector must stay out of the declaration"
            );
            assert!(
                !declaration.contains("compiled"),
                "{name}: the compiled schema must stay out of the declaration"
            );
        }
    }
}
