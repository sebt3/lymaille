//! Trait of a native tool: a tool the turn loop calls by name, without Rhai (`agent-sessions/toolsets#FR-18`). It lives in
//! `session` so that `runtime` and `toolsets` meet without depending on each other (`agent-sessions/session#FR-4`).
//! See `src/session/native_tool.sdd`.

use serde_json::Value;

/// A tool the turn loop calls by name, with the arguments of the model and without going through the scratchpad.
///
/// The trait is synchronous: a native tool waits neither for the network nor for the disk, and never panics. The text
/// that [`NativeTool::call`] returns is already masked by the redactor of the session: the turn loop masks it again.
pub trait NativeTool: Send + Sync {
    /// Name of the tool as the model calls it. It cannot be `rhai_pad`, and no other native tool of the same turn runner
    /// can have it.
    fn name(&self) -> &str;

    /// Definition of the tool for the inference request: a flat JSON object with `name`, `description` and `parameters`
    /// (a JSON schema), like the definition of `rhai_pad`.
    fn definition(&self) -> Value;

    /// Runs the tool with the arguments of the call and returns the text given to the model. A failure is returned as
    /// the text of an error, never as a panic.
    fn call(&self, arguments: &Value) -> String;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::NativeTool;

    /// The name of the test double, read by its definition and by `name`.
    const ECHO: &str = "echo";

    /// A native tool that answers its text back, with the definition of a flat tool.
    struct Echo;

    impl NativeTool for Echo {
        fn name(&self) -> &str {
            ECHO
        }

        fn definition(&self) -> Value {
            json!({
                "name": ECHO,
                "description": "Answers the text it receives.",
                "parameters": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                }
            })
        }

        fn call(&self, arguments: &Value) -> String {
            arguments.get("text").map_or_else(String::new, Value::to_string)
        }
    }

    // Scenario: un tool natif de test répond par son nom et sa définition
    #[test]
    fn a_native_tool_answers_its_name_and_its_definition() {
        let tool: Arc<dyn NativeTool> = Arc::new(Echo);
        let definition = tool.definition();
        assert_eq!(definition.get("name"), Some(&json!(tool.name())));
        assert!(definition.get("description").is_some_and(Value::is_string));
        assert!(definition.get("parameters").is_some_and(Value::is_object));
    }

    // Must (native_tool.sdd) : le trait est Send et Sync, pour un `Arc<dyn NativeTool>` partagé par la boucle de tour
    #[test]
    fn a_native_tool_can_be_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<Arc<dyn NativeTool>>();
    }
}
