//! Canonical definition of the `rhai_pad` tool, given to the model by every host, so that the text the model reads is the
//! same whatever the host (`agent-sessions#BO-2`, `agent-sessions/scratchpad#FR-9`). See `src/runtime/definition.sdd`.

use serde_json::{Value, json};

use super::RHAI_PAD_TOOL;

/// Description of the tool, read by the model. It is in English because it speaks to the model.
const DESCRIPTION: &str = r#"Run one Rhai script in this session's persistent scratchpad. Returns what the script printed, then the value of its last statement. `let` variables and `fn` definitions persist between calls; closures and function pointers work within a session but are lost when it is restored (define a `fn` for anything you must keep), `const` does not exist, and a `fn` cannot see scope variables (pass arguments; read the toolset context with `session_context()`). Granted modules are called as `module::function(args)`. Discovery built-ins: `vars()`, `modules()`, `functions("name")`, `toolsets()`, `skills()`, `find("words")`, `rules()`. Output over the cap is cut in the middle: keep results in variables and extract with the `fmt_*` functions. Errors come back as text: fix the script and call again. Read the `rhai-pad` skill with the `skill` tool before your first script."#;

/// Description of the `script` parameter, read by the model.
const SCRIPT_DESCRIPTION: &str = "The Rhai script to run.";

/// Definition of the `rhai_pad` tool for the inference request: a flat JSON object with `name`, `description` and
/// `parameters`, the schema of an object whose only property `script` is a required string.
///
/// The value is constant: two calls give the same value.
#[must_use]
pub fn rhai_pad_definition() -> Value {
    json!({
        "name": RHAI_PAD_TOOL,
        "description": DESCRIPTION,
        "parameters": {
            "type": "object",
            "properties": {
                "script": {"type": "string", "description": SCRIPT_DESCRIPTION}
            },
            "required": ["script"]
        }
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use rhai::{Module, Shared};
    use serde_json::json;

    use super::rhai_pad_definition;
    use crate::clock::{ManualClock, Timestamp};
    use crate::context::estimate_tokens;
    use crate::scratchpad::{
        GrantedModule, Grants, RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits,
        UnpersistableReason,
    };
    use crate::session::{CancelHandle, Redactor};

    /// The description of the definition, as the model reads it.
    fn description() -> String {
        rhai_pad_definition()["description"]
            .as_str()
            .expect("the description is a string")
            .to_owned()
    }

    /// An engine over `grants` and `limits`, with no other registration.
    fn engine(grants: Grants, limits: ScratchpadLimits) -> Arc<ScratchpadEngine> {
        Arc::new(
            ScratchpadEngine::builder(limits, grants)
                .build()
                .expect("the engine builds"),
        )
    }

    /// A scratchpad over `grants` and `limits`, with no other registration.
    fn pad(grants: Grants, limits: ScratchpadLimits) -> Scratchpad {
        Scratchpad::new(engine(grants, limits))
    }

    /// Runs `script` in `pad` as the agent of a session would.
    async fn run(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    /// Asserts that the description states `claim` in these words.
    fn states(text: &str, claim: &str) {
        assert!(text.contains(claim), "the description must state: {claim}");
    }

    // Scenario: la définition est complète et stable
    #[test]
    fn the_definition_is_complete_and_stable() {
        let first = rhai_pad_definition();
        let second = rhai_pad_definition();
        assert_eq!(first, second, "two reads give the same value");
        assert_eq!(first["name"], "rhai_pad");
        assert_eq!(first["parameters"]["required"], json!(["script"]));
        assert_eq!(first["parameters"]["properties"]["script"]["type"], "string");
        assert_eq!(
            first["parameters"]["properties"]["script"]["description"],
            "The Rhai script to run."
        );
    }

    // Scenario: chaque affirmation de la description est vraie (valeur du dernier statement)
    #[tokio::test]
    async fn the_printed_text_comes_before_the_last_value() {
        states(
            &description(),
            "Returns what the script printed, then the value of its last statement.",
        );
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        let printed = run(&pad, r#"print("first-line"); 42"#).await;
        assert_eq!(
            printed.status(),
            RunStatus::Ok,
            "the script runs: {}",
            printed.text()
        );
        assert!(
            matches!(
                (printed.text().find("first-line"), printed.text().find("42")),
                (Some(line), Some(value)) if line < value
            ),
            "the printed text comes before the value: {}",
            printed.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (let et fn persistants)
    #[tokio::test]
    async fn let_variables_and_fn_definitions_persist_between_calls() {
        states(
            &description(),
            "`let` variables and `fn` definitions persist between calls",
        );
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        let defined = run(&pad, "let kept = 7; fn twice(n) { n * 2 }").await;
        assert_eq!(
            defined.status(),
            RunStatus::Ok,
            "the definitions run: {}",
            defined.text()
        );
        let reused = run(&pad, "kept + twice(1)").await;
        assert_eq!(
            reused.status(),
            RunStatus::Ok,
            "the next call runs: {}",
            reused.text()
        );
        assert!(
            reused.text().contains('9'),
            "`let` and `fn` persist: {}",
            reused.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (closures utilisables dans la session)
    #[tokio::test]
    async fn closures_and_function_pointers_work_within_a_session() {
        states(
            &description(),
            "closures and function pointers work within a session but are lost when it is restored (define a `fn` for anything you must keep)",
        );
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        let twice = run(&pad, "fn twice(n) { n * 2 }").await;
        assert_eq!(
            twice.status(),
            RunStatus::Ok,
            "the function is defined: {}",
            twice.text()
        );

        let closure = run(&pad, "let adder = |n| n + 1;").await;
        assert_eq!(
            closure.status(),
            RunStatus::Ok,
            "the closure is defined: {}",
            closure.text()
        );
        assert!(
            !closure.snapshot().variables().contains_key("adder"),
            "a closure is not in the saved state"
        );
        assert!(
            closure
                .warnings()
                .iter()
                .any(|warning| warning.variable() == "adder"
                    && matches!(warning.reason(), UnpersistableReason::Closure)),
            "a warning names the closure"
        );
        let called = run(&pad, "adder.call(1)").await;
        assert_eq!(
            called.text().trim(),
            "2",
            "the closure works within the session: {}",
            called.text()
        );

        let pointer = run(&pad, r#"let pointer = Fn("twice");"#).await;
        assert_eq!(
            pointer.status(),
            RunStatus::Ok,
            "the pointer is defined: {}",
            pointer.text()
        );
        assert!(
            !pointer.snapshot().variables().contains_key("pointer"),
            "a function pointer is not in the saved state"
        );
        assert!(
            pointer
                .warnings()
                .iter()
                .any(|warning| warning.variable() == "pointer"
                    && matches!(warning.reason(), UnpersistableReason::FunctionPointer)),
            "a warning names the function pointer"
        );
        let through_pointer = run(&pad, "pointer.call(1)").await;
        assert_eq!(
            through_pointer.text().trim(),
            "2",
            "the function pointer works within the session: {}",
            through_pointer.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (closures perdues à la restauration)
    #[tokio::test]
    async fn closures_and_function_pointers_are_lost_when_restored() {
        let engine = engine(Grants::new(), ScratchpadLimits::new());
        let pad = Scratchpad::new(Arc::clone(&engine));
        let twice = run(&pad, "fn twice(n) { n * 2 }").await;
        assert_eq!(
            twice.status(),
            RunStatus::Ok,
            "the function is defined: {}",
            twice.text()
        );
        let closure = run(&pad, "let adder = |n| n + 1;").await;
        assert_eq!(
            closure.status(),
            RunStatus::Ok,
            "the closure is defined: {}",
            closure.text()
        );
        let pointer = run(&pad, r#"let pointer = Fn("twice");"#).await;
        assert_eq!(
            pointer.status(),
            RunStatus::Ok,
            "the pointer is defined: {}",
            pointer.text()
        );

        let restored = Scratchpad::restore(
            Arc::clone(&engine),
            pointer.snapshot().variables(),
            pointer.snapshot().functions(),
        )
        .into_pad();
        let lost = run(&restored, "adder.call(1)").await;
        assert_ne!(
            lost.status(),
            RunStatus::Ok,
            "the closure is lost when restored: {}",
            lost.text()
        );
        let lost_pointer = run(&restored, "pointer.call(1)").await;
        assert_ne!(
            lost_pointer.status(),
            RunStatus::Ok,
            "the function pointer is lost when restored: {}",
            lost_pointer.text()
        );
        let kept = run(&restored, "twice(2)").await;
        assert_eq!(
            kept.text().trim(),
            "4",
            "the `fn` is kept when restored: {}",
            kept.text()
        );
    }

    // Scenario: la définition est complète et stable (borne de 350 tokens estimés, mesurée sur la description)
    //
    // The spec's bound (350 estimated tokens at 2.5 characters per token) is checked on the description, which the spec
    // itself measures (about 311 tokens). The whole serialized definition is about 417 tokens with its schema: the bound
    // does not hold for it with the verbatim text. This is an open question for the lead developer, see the report.
    #[test]
    fn the_description_stays_under_350_estimated_tokens() {
        let chars = u64::try_from(description().chars().count()).expect("a length fits in u64");
        let ratio = NonZeroU64::new(2500).expect("the ratio is not zero");
        let tokens = estimate_tokens(chars, ratio, 0);
        assert!(
            tokens < 350,
            "the description is {chars} characters, {tokens} estimated tokens at 2.5 characters per token"
        );
    }

    // Scenario: chaque affirmation de la description est vraie (const absent, fn sans scope)
    #[tokio::test]
    async fn const_does_not_exist_and_a_fn_cannot_see_the_scope() {
        let text = description();
        states(&text, "`const` does not exist");
        states(&text, "a `fn` cannot see scope variables");
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        let constant = run(&pad, "const LIMIT = 3; LIMIT").await;
        assert_ne!(
            constant.status(),
            RunStatus::Ok,
            "`const` is refused: {}",
            constant.text()
        );
        let blind = run(&pad, "let hidden = 5; fn peek() { hidden } peek()").await;
        assert_ne!(
            blind.status(),
            RunStatus::Ok,
            "a `fn` does not see the scope: {}",
            blind.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (fonctions d'introspection nommées)
    //
    // The description writes `functions("name")`: the engine takes the origin as a string, so the test runs the form
    // `functions("global")`, which the description's `functions("name")` stands for.
    #[tokio::test]
    async fn the_discovery_built_ins_are_callable() {
        let text = description();
        for name in [
            "`vars()`",
            "`modules()`",
            "`functions(\"name\")`",
            "`toolsets()`",
            "`skills()`",
            r#"`find("words")`"#,
            "`rules()`",
        ] {
            states(&text, name);
        }
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        for script in [
            "vars()",
            "modules()",
            "functions(\"global\")",
            "toolsets()",
            "skills()",
            "find(\"print\")",
        ] {
            let outcome = run(&pad, &format!("type_of({script}) == \"array\"")).await;
            assert_eq!(
                outcome.status(),
                RunStatus::Ok,
                "{script} runs: {}",
                outcome.text()
            );
            assert_eq!(
                outcome.text().trim(),
                "true",
                "{script} is an array: {}",
                outcome.text()
            );
        }
        let rules = run(&pad, "rules()").await;
        assert!(
            rules.text().contains("Sandbox rules"),
            "`rules()` describes the sandbox: {}",
            rules.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (fonctions fmt_*)
    #[tokio::test]
    async fn the_fmt_functions_are_built_in() {
        states(&description(), "the `fmt_*` functions");
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        let formatted = run(
            &pad,
            r#""fmt_table" in functions("global").map(|found| found.path)"#,
        )
        .await;
        assert_eq!(
            formatted.text().trim(),
            "true",
            "`fmt_table` is a built-in function: {}",
            formatted.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (erreurs rendues en texte)
    #[tokio::test]
    async fn errors_come_back_as_text_and_the_next_call_runs() {
        states(
            &description(),
            "Errors come back as text: fix the script and call again.",
        );
        let pad = pad(Grants::new(), ScratchpadLimits::new());
        let broken = run(&pad, "let broken = ;").await;
        assert_ne!(broken.status(), RunStatus::Ok, "a syntax error fails");
        assert!(!broken.text().is_empty(), "the error comes back as text");
        let fixed = run(&pad, "1 + 1").await;
        assert_eq!(
            fixed.status(),
            RunStatus::Ok,
            "the next call runs: {}",
            fixed.text()
        );
        assert!(
            fixed.text().contains('2'),
            "the fixed script answers: {}",
            fixed.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (module accordé)
    #[tokio::test]
    async fn granted_modules_are_called_as_module_function() {
        states(
            &description(),
            "Granted modules are called as `module::function(args)`.",
        );
        let mut module = Module::new();
        let _hello = module.set_native_fn("hello", |number: i64| Ok(number + 1));
        let grants = Grants::new()
            .with_module(GrantedModule::new("demo", Shared::new(module)).expect("a valid module name"))
            .expect("the module is accepted");
        let pad = pad(grants, ScratchpadLimits::new());
        let outcome = run(&pad, "demo::hello(1)").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the granted function runs: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains('2'),
            "the function answers: {}",
            outcome.text()
        );
    }

    // Scenario: chaque affirmation de la description est vraie (plafond de sortie coupé au milieu)
    #[tokio::test]
    async fn output_over_the_cap_is_cut_in_the_middle() {
        states(&description(), "Output over the cap is cut in the middle");
        let pad = pad(Grants::new(), ScratchpadLimits::new().with_render_cap_chars(200));
        let outcome = run(
            &pad,
            r#"let text = ""; for i in 0..500 { text += "abcdef"; } text"#,
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the script runs: {}",
            outcome.text()
        );
        assert!(outcome.truncated(), "the output is over the cap");
        assert!(
            outcome.text().chars().count() <= 200,
            "the text stays under the cap"
        );
        assert!(
            outcome.text().starts_with("abcdef"),
            "the start of the output is kept"
        );
        assert!(
            outcome.text().ends_with("abcdef"),
            "the end of the output is kept"
        );
        assert!(
            outcome.text().contains("output truncated"),
            "a notice marks the cut"
        );
    }
}
