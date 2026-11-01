//! Trait of a native tool: a tool the turn loop calls by name, without Rhai (`agent-sessions/toolsets#FR-18`). It lives in
//! `session` so that `runtime` and `toolsets` meet without depending on each other (`agent-sessions/session#FR-4`).
//! See `src/session/native_tool.sdd`.

use std::pin::Pin;

use serde_json::Value;

use super::CancelHandle;

/// The boxed future of [`NativeTool::call`]: the text for the model, or the text of an error.
type NativeCall<'call> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'call>>;

/// A tool the turn loop calls by name, with the arguments of the model and without going through the scratchpad.
///
/// The call is asynchronous, as a boxed future so that the trait stays usable as `Arc<dyn NativeTool>`; a tool that
/// awaits nothing returns a future that is already ready. The tool never panics. The text that [`NativeTool::call`]
/// returns is already masked by the redactor of the session: the turn loop masks it again.
pub trait NativeTool: Send + Sync {
    /// Name of the tool as the model calls it. It cannot be `rhai_pad`, and no other native tool of the same turn runner
    /// can have it.
    fn name(&self) -> &str;

    /// Definition of the tool for the inference request: a flat JSON object with `name`, `description` and `parameters`
    /// (a JSON schema), like the definition of `rhai_pad`.
    fn definition(&self) -> Value;

    /// Runs the tool with the arguments of the call and the cancellation handle of the turn, and returns the text given
    /// to the model. A failure is returned as the text of an error, which the turn loop gives to the model as the result
    /// of the tool, never as a panic. A tool that runs a script stops it when the handle is cancelled.
    fn call<'call>(&'call self, arguments: &'call Value, cancel: &'call CancelHandle) -> NativeCall<'call>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use std::pin::Pin;

    use super::NativeTool;
    use crate::session::CancelHandle;

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

        fn call<'call>(
            &'call self,
            arguments: &'call Value,
            cancel: &'call CancelHandle,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'call>> {
            Box::pin(std::future::ready(answer(arguments, cancel)))
        }
    }

    /// The answer of the test double: its `text` argument as JSON, or an error.
    fn answer(arguments: &Value, cancel: &CancelHandle) -> Result<String, String> {
        if cancel.is_cancelled() {
            return Err("cancelled".to_owned());
        }
        arguments
            .get("text")
            .map(Value::to_string)
            .ok_or_else(|| "missing `text`".to_owned())
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

    // Must (native_tool.sdd) : l'appel est asynchrone, par un objet dynamique, et rend le texte ou le texte d'une erreur
    #[tokio::test]
    async fn a_native_tool_is_called_asynchronously_through_a_dynamic_object() {
        let tool: Arc<dyn NativeTool> = Arc::new(Echo);
        let cancel = CancelHandle::new();
        let answered = tool.call(&json!({"text": "a"}), &cancel).await;
        assert_eq!(answered, Ok("\"a\"".to_owned()), "the text is the result");
        let refused = tool.call(&json!({}), &cancel).await;
        assert_eq!(
            refused,
            Err("missing `text`".to_owned()),
            "a failure is the text of an error"
        );
    }

    // Must (native_tool.sdd) : le tool reçoit le CancelHandle du tour
    #[tokio::test]
    async fn a_native_tool_receives_the_cancel_handle_of_the_turn() {
        let tool: Arc<dyn NativeTool> = Arc::new(Echo);
        let cancel = CancelHandle::new();
        cancel.cancel();
        let stopped = tool.call(&json!({"text": "a"}), &cancel).await;
        assert_eq!(
            stopped,
            Err("cancelled".to_owned()),
            "the tool saw the cancelled handle"
        );
    }
}
