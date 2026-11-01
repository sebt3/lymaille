//! The definition of the `rhai_prod_inspect` tool of the configuration agent (`agent-sessions/scratchpad#FR-13`,
//! `agent-sessions#AC-6bis`). See `src/scratchpad/inspect_tool.sdd`.

use serde_json::{Value, json};

/// Name of the inspection tool, as the model calls it.
const INSPECT_TOOL: &str = "rhai_prod_inspect";

/// Description of the inspection tool, as the model reads it. Each claim is checked by a test.
const DESCRIPTION: &str = "Run one Rhai script in the read-only inspection pad of the current target, chosen with `inspect::open_context` or `inspect::open_session`. Introspection runs for real: `vars()`, `modules()`, `functions()`, `toolsets()`, `skills()`, `find(\"words\")`, `rules()`. The functions of the target's toolset and MCP modules have their real names, signatures and documentation, but every call returns an inspection-only error: nothing is executed and no server is contacted. For a past session the variables can be read and not written. Nothing is saved. Errors come back as text.";

/// Definition of the `rhai_prod_inspect` tool for the inference request.
///
/// It is a flat JSON object with `name`, `description` and `parameters`, the schema of an object with one required
/// string property, `script`.
///
/// The value is constant: two calls give the same value.
#[must_use]
pub fn rhai_prod_inspect_definition() -> Value {
    json!({
        "name": INSPECT_TOOL,
        "description": DESCRIPTION,
        "parameters": {
            "type": "object",
            "properties": {
                "script": {"type": "string", "description": "The Rhai script to run in the inspection pad."}
            },
            "required": ["script"]
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DESCRIPTION, rhai_prod_inspect_definition};

    // Scenario: la définition est complète
    #[test]
    fn definition_is_complete_and_constant() {
        let definition = rhai_prod_inspect_definition();
        assert_eq!(
            definition["name"],
            json!("rhai_prod_inspect"),
            "the tool is named as the spec says"
        );
        assert_eq!(
            definition["description"],
            json!(DESCRIPTION),
            "the description is the one of the spec"
        );
        assert_eq!(definition["parameters"]["type"], json!("object"));
        assert_eq!(
            definition["parameters"]["required"],
            json!(["script"]),
            "the script is required"
        );
        assert_eq!(
            definition["parameters"]["properties"]["script"]["type"],
            json!("string"),
            "the script is a string"
        );
        assert_eq!(definition, rhai_prod_inspect_definition(), "two reads are equal");
    }

    // Scenario: la définition est complète (texte mot pour mot)
    #[test]
    fn description_is_the_text_of_the_spec() {
        assert_eq!(
            DESCRIPTION,
            "Run one Rhai script in the read-only inspection pad of the current target, chosen with `inspect::open_context` or `inspect::open_session`. Introspection runs for real: `vars()`, `modules()`, `functions()`, `toolsets()`, `skills()`, `find(\"words\")`, `rules()`. The functions of the target's toolset and MCP modules have their real names, signatures and documentation, but every call returns an inspection-only error: nothing is executed and no server is contacted. For a past session the variables can be read and not written. Nothing is saved. Errors come back as text."
        );
    }
}
