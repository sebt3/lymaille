//! Native tools of the toolsets domain: the `skill` tool, which reads the skills without Rhai, and the `session_context()`
//! function, which the Rhai code of a session reads as a copy (`agent-sessions/toolsets#FR-17`, `FR-18`, `AC-10`, `AC-11`).
//! See `src/toolsets/native.sdd`.

use std::collections::BTreeMap;
use std::sync::Arc;

use rhai::serde::to_dynamic;
use rhai::{Dynamic, Engine as RhaiEngine, EvalAltResult, FuncRegistration, Module, Shared};
use serde::Serialize;
use serde_json::{Value, json};

use super::{ResolvedContext, SessionMeta, Skill, ToolsetsError, ToolsetsResult, render_skill};
use crate::scratchpad::{ScratchpadLimits, ScratchpadResult, limits_notice, truncate_middle};
use crate::session::{NativeTool, Redactor};

/// Name of the built-in skill that teaches the `rhai_pad` tool.
const BUILTIN_NAME: &str = "rhai-pad";

/// Description of the built-in skill, as the model sees it in the list of skills.
const BUILTIN_DESCRIPTION: &str = "Run a Rhai script in this session's scratchpad, and discover its functions, modules, toolsets, skills and limits.";

/// Text of the built-in skill before the examples, in English since it speaks to the model. Each claim is checked by a test
/// that runs it in the scratchpad (`built_in_claims_hold_in_the_scratchpad`, `built_in_rhai_digest_and_final_value_claims_hold`).
const BUILTIN_BODY: &str = "Use `rhai_pad` to run one Rhai script in this session's scratchpad.

How a call works:
- The result is the value of the last statement; a final `;` does not hide it (`1 + 1;` gives 2). A `let`, a loop, an `if` without `else` that is not taken, a `fn` definition and `print(...)` give `()`: end the script with the variable you want to read.
- `let` variables and `fn` definitions persist to the next call of this session.
- Text printed with `print` comes back before the result.
- Call the functions of a granted module as `module::function(arguments)`.
- An error comes back as text: fix the script and call again.

Discover what is available before you write a script:
- `vars()`: the variables of the scope at the start of this run, as maps `#{name, type, preview}`.
- `modules()`: the granted modules and the groups of built-in functions (for example `pure`, `format` and `host`), as maps `#{name, kind, functions}`; `kind` is `module`, `core`, `format` or `host` and `functions` is the number of functions.
- `functions()`: every function you can call, as maps `#{path, signature, doc, origin}`. `functions(\"global\")` lists the built-in groups, `functions(\"script\")` the functions you defined in this session, and `functions(\"name\")` the functions of one granted module; any other name is an error.
- `toolsets()`: the granted toolsets, as maps `#{name, description}`.
- `skills()`: the granted skills, with their descriptions. Read the text of a skill with the `skill` tool, not with a script.
- `find(\"words\")`: the functions, granted modules, toolsets and skills that hold every word of the query, in any case, in their name, path, signature, documentation or description, as maps `#{kind, name, path, text}`. The exact names come first; at most 50 results are returned.
- `rules()`: the sandbox rules and the limits.

`session_context()` returns a copy of the toolset context of this session: `params` holds the shared parameters, `toolsets` holds the resolved parameters of each toolset, and `roles` holds the names of the bound roles. Changing the copy does not change the context. A `fn` can call it too.

Rhai in brief (`functions()` and `find(\"words\")` tell the rest):
- `let x = 1;` declares, `x = x + 1;` assigns; an unknown variable is an error.
- Strings: `\"text\"`, or backticks with interpolation: `pod-${name}`.
- Arrays: `[1, 2]`, `a[0]`, `a.len()`, `a.push(3)`, `a.map(|v| v * 2)`, `a.filter(|v| v > 1)`.
- Maps: `#{name: \"web\", n: 1}`, `m.name` or `m[\"n\"]`, `m.keys()`.
- Closures: `|v| expr` sees the variables of the scope and may call your `fn`; it is lost when the session is restored.
- Loops: `for v in list { }`, `for i in 0..3 { }`, `while cond { }`.
- Functions: `fn add(a, b) { a + b }` returns its last expression and cannot see scope variables.
- Errors: `try { } catch (e) { }` is a statement, not a value: assign inside it (`msg = e;`) and read the variable after.
- `()` is the empty value.

";

/// First example call of the built-in skill: a variable and a `fn`.
const FIRST_EXAMPLE: &str = "let names = [\"web\", \"api\"];
fn label(name) { `pod-${name}` }
names.map(|n| label(n))";

/// Result of the first example call, as the skill states it.
const FIRST_RESULT: &str = "[\"pod-web\",\"pod-api\"]";

/// Second example call of the built-in skill: it uses the variable and the `fn` of the first.
const SECOND_EXAMPLE: &str = "names.push(\"db\");
let labels = names.map(|n| label(n));
labels";

/// Result of the second example call, as the skill states it.
const SECOND_RESULT: &str = "[\"pod-web\",\"pod-api\",\"pod-db\"]";

/// Name of the `skill` tool.
const SKILL_TOOL: &str = "skill";

/// Description of the `skill` tool, read by the model. It is in English because it speaks to the model.
const SKILL_TOOL_DESCRIPTION: &str = "Read the skills granted to this session. With no argument, lists them (name and description). With `name`, returns the text of that skill; with `name` and `document`, one of its annex documents. Read `rhai-pad` first.";

/// A granted skill as the `skill` tool holds it: the toolset it belongs to, when it has one, and the skill.
#[derive(Debug)]
struct Entry {
    toolset: Option<String>,
    skill: Skill,
}

/// The `skill` tool, which reads the granted skills without Rhai.
///
/// It lists the skills, gives the rendered text of one, or one of its annex documents. Every text it gives is masked by the
/// redactor of the session, then capped by `truncate_middle` (`agent-sessions/toolsets#FR-18`, `AC-11`, decision of
/// 2026-10-31 D-c: a declared secret never comes out).
#[derive(Debug)]
#[non_exhaustive]
pub struct SkillTool {
    entries: BTreeMap<String, Entry>,
    context: ResolvedContext,
    session: SessionMeta,
    cap_chars: usize,
    redactor: Arc<Redactor>,
}

impl SkillTool {
    /// Builds the tool over the granted skills, each with its toolset, and over the built-in skill. `redactor` is the redactor
    /// of the session: the secrets it declares are masked in every text the tool gives.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] when two skills share a name, the built-in one included.
    #[expect(
        clippy::too_many_arguments,
        reason = "the tool takes its six inputs at once; grouping them in a struct would add a public type for one constructor"
    )]
    pub fn new(
        granted: Vec<(String, Skill)>,
        builtin: Skill,
        context: ResolvedContext,
        session: SessionMeta,
        cap_chars: usize,
        redactor: Arc<Redactor>,
    ) -> ToolsetsResult<Self> {
        let mut entries = BTreeMap::new();
        for (toolset, skill) in granted {
            insert_entry(&mut entries, Some(toolset), skill)?;
        }
        insert_entry(&mut entries, None, builtin)?;
        Ok(Self {
            entries,
            context,
            session,
            cap_chars,
            redactor,
        })
    }

    /// Answers the `skill` tool. With no name, the list of the skills, name and description. With a name, the rendered body
    /// of that skill. With a name and a document name, that annex document.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] for an unknown skill or document, the message listing the existing names,
    /// and [`ToolsetsError::Template`] when a description or a body does not render.
    #[expect(
        clippy::same_name_method,
        reason = "the typed call of the spec and the `NativeTool::call` of the turn loop share a name; the second delegates to the first"
    )]
    pub fn call(&self, name: Option<&str>, document: Option<&str>) -> ToolsetsResult<String> {
        match (name, document) {
            (None, None) => self.list(),
            (Some(name), None) => self.body(name),
            (Some(name), Some(document)) => self.annex(name, document),
            (None, Some(document)) => Err(ToolsetsError::InvalidSkill {
                name: document.to_owned(),
                reason: "a document is read from a named skill",
                known: Vec::new(),
            }),
        }
    }

    /// The name and the rendered description of each skill, one per line.
    fn list(&self) -> ToolsetsResult<String> {
        let mut lines = Vec::with_capacity(self.entries.len());
        for entry in self.entries.values() {
            let (description, _body) = self.render(entry)?;
            lines.push(format!("- {}: {description}", entry.skill.name()));
        }
        Ok(self.capped(&lines.join("\n")))
    }

    /// The rendered body of a skill.
    fn body(&self, name: &str) -> ToolsetsResult<String> {
        let (_description, body) = self.render(self.entry(name)?)?;
        Ok(self.capped(&body))
    }

    /// The text of an annex document of a skill, as written.
    fn annex(&self, name: &str, document: &str) -> ToolsetsResult<String> {
        let entry = self.entry(name)?;
        let Some(text) = entry.skill.annex(document) else {
            let known: Vec<String> = entry.skill.annexes().keys().cloned().collect();
            return Err(ToolsetsError::InvalidSkill {
                name: document.to_owned(),
                reason: "unknown annex document",
                known,
            });
        };
        Ok(self.capped(text))
    }

    /// The granted skill of this name. The error lists the names of the granted skills.
    fn entry(&self, name: &str) -> ToolsetsResult<&Entry> {
        self.entries.get(name).ok_or_else(|| ToolsetsError::InvalidSkill {
            name: name.to_owned(),
            reason: "unknown skill name",
            known: self.entries.keys().cloned().collect(),
        })
    }

    /// The rendered description and body of a skill, with the parameters of its toolset.
    fn render(&self, entry: &Entry) -> ToolsetsResult<(String, String)> {
        render_skill(
            &entry.skill,
            entry.toolset.as_deref().unwrap_or(""),
            &self.context,
            &self.session,
        )
    }

    /// The text masked by the redactor of the session, then capped at the render cap, its middle replaced by the notice of
    /// `truncate_middle`. The mask comes first: a cut can then never leave a part of a declared secret.
    fn capped(&self, text: &str) -> String {
        truncate_middle(&self.redactor.redact(text), self.cap_chars)
            .text()
            .to_owned()
    }
}

/// Adds a skill to the table of the tool. A name taken twice is refused, never replaced in silence.
fn insert_entry(
    entries: &mut BTreeMap<String, Entry>,
    toolset: Option<String>,
    skill: Skill,
) -> ToolsetsResult<()> {
    let name = skill.name().to_owned();
    if entries.contains_key(&name) {
        return Err(ToolsetsError::InvalidSkill {
            name,
            reason: "a skill of this name is granted twice",
            known: Vec::new(),
        });
    }
    let _previous = entries.insert(name, Entry { toolset, skill });
    Ok(())
}

/// The copy of the context that `session_context()` gives. The key of the shared parameters is `params`: `shared` is a
/// reserved word of Rhai, which a script cannot read with a dot.
#[derive(Serialize)]
struct ContextCopy<'context> {
    params: &'context BTreeMap<String, Value>,
    toolsets: &'context BTreeMap<String, BTreeMap<String, Value>>,
    roles: &'context [String],
}

/// Builds the Rhai registration step of `session_context()`.
///
/// It is passed to `ScratchpadEngine::builder`'s `with_registration`. The function gives a copy, built at each call, of the
/// given resolved context: the shared parameters, the parameters by toolset and the names of the bound roles. No reference
/// of a role is in it, and every secret declared to `redactor` is masked in it, keys and numbers included (decision of
/// 2026-10-31, D-c). It is registered with `FuncRegistration`, which declares it non-volatile, that is deterministic for
/// Rhai: `register_fn` would declare it volatile.
pub fn session_context_registration(
    context: ResolvedContext,
    redactor: Arc<Redactor>,
) -> impl FnOnce(&mut RhaiEngine) -> ScratchpadResult<()> + 'static {
    move |engine| {
        let mut module = Module::new();
        let _function = FuncRegistration::new("session_context")
            .in_global_namespace()
            .set_into_module(&mut module, move || -> Result<Dynamic, Box<EvalAltResult>> {
                let copy = serde_json::to_value(ContextCopy {
                    params: context.shared(),
                    toolsets: context.toolsets(),
                    roles: context.roles(),
                })
                .ok()
                .ok_or_else(|| {
                    Box::<EvalAltResult>::from("session_context: the context does not serialize")
                })?;
                to_dynamic(masked(&redactor, &copy))
            });
        let _registered = engine.register_global_module(Shared::new(module));
        Ok(())
    }
}

/// A copy of `value` in which every declared secret is masked: in a text, in a key, and in a number whose text holds it.
/// Booleans and nulls cannot hold a secret, which is at least six characters long.
fn masked(redactor: &Redactor, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redactor.redact(text)),
        Value::Number(number) => {
            let text = number.to_string();
            let redacted = redactor.redact(&text);
            if redacted == text {
                value.clone()
            } else {
                Value::String(redacted)
            }
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| masked(redactor, item)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (redactor.redact(key), masked(redactor, item)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) => value.clone(),
    }
}

/// Definition of the `skill` tool for the inference request.
///
/// It is a flat JSON object with `name`, `description` and `parameters`, the schema of an object whose properties `name`
/// and `document` are strings, none of them required.
///
/// The value is constant: two calls give the same value.
#[must_use]
pub fn skill_definition() -> Value {
    json!({
        "name": SKILL_TOOL,
        "description": SKILL_TOOL_DESCRIPTION,
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Name of the skill to read."},
                "document": {"type": "string", "description": "Name of an annex document of that skill."}
            }
        }
    })
}

impl NativeTool for SkillTool {
    fn name(&self) -> &str {
        SKILL_TOOL
    }

    fn definition(&self) -> Value {
        skill_definition()
    }

    fn call(&self, arguments: &Value) -> String {
        let text = |key: &str| arguments.get(key).and_then(Value::as_str);
        self.call(text("name"), text("document"))
            .unwrap_or_else(|error| error.to_string())
    }
}

/// Builds the built-in skill that teaches the `rhai_pad` tool.
///
/// It covers the introspection and the limits of the scratchpad; its text cites the sandbox notice of `limits`.
///
/// # Errors
///
/// Returns [`ToolsetsError::InvalidSkill`] or [`ToolsetsError::SkillTooLarge`] when the skill does not build, or when its
/// text does not fit the render cap of `limits`.
pub fn builtin_rhai_pad_skill(limits: &ScratchpadLimits) -> ToolsetsResult<Skill> {
    let body = format!(
        "{BUILTIN_BODY}Example, first call:\n```\n{FIRST_EXAMPLE}\n```\nResult: {FIRST_RESULT}\n\n\
         Example, second call (it uses the variable and the `fn` of the first):\n```\n{SECOND_EXAMPLE}\n```\n\
         Result: {SECOND_RESULT}\n\n{}",
        limits_notice(limits)
    );
    let skill = Skill::new(BUILTIN_NAME.to_owned(), BUILTIN_DESCRIPTION.to_owned(), body)?;
    skill.check_size(u64::try_from(limits.render_cap_chars()).unwrap_or(u64::MAX))?;
    Ok(skill)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;
    use vynil_core::Semver;

    use super::{
        FIRST_EXAMPLE, FIRST_RESULT, SECOND_EXAMPLE, SECOND_RESULT, SkillTool, builtin_rhai_pad_skill,
        session_context_registration, skill_definition,
    };
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{
        GrantedSkill, RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits,
    };
    use crate::session::{CancelHandle, NativeTool, Redactor};
    use crate::toolsets::{
        ContextLevel, ResolvedContext, SessionMeta, Skill, Toolset, ToolsetContext, ToolsetsError, assemble,
    };

    /// The resolved context of the tests: `cluster` shared, `k8s` bound to a reference, and `region` overridden for `ops`
    /// (default `eu`, override `us`).
    fn context() -> ResolvedContext {
        ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter("cluster".to_owned(), json!("prod"))
            .expect("a plain parameter")
            .with_role("k8s".to_owned(), "prod-k8s".to_owned())
            .expect("a role binding")
            .with_override("ops".to_owned(), "region".to_owned(), json!("us"))
            .expect("a plain override")
            .resolve([("ops".to_owned(), "region".to_owned(), json!("eu"))])
            .expect("the defaults resolve")
    }

    fn session() -> SessionMeta {
        SessionMeta::new(
            "main".to_owned(),
            "2026-10-30T09:00:00Z".to_owned(),
            vec![("ops".to_owned(), "Operations".to_owned())],
            vec!["deploy".to_owned()],
            None,
        )
    }

    /// The skill `deploy` of `ops`, with an annex document `queries`.
    fn deploy() -> Skill {
        Skill::new(
            "deploy".to_owned(),
            "Deploy to {{context.params.cluster}}".to_owned(),
            "Target: {{context.params.cluster}} in {{context.params.region}}".to_owned(),
        )
        .expect("a valid skill")
        .with_annex("queries".to_owned(), "PromQL text".to_owned())
        .expect("a valid annex")
    }

    fn builtin() -> Skill {
        builtin_rhai_pad_skill(&ScratchpadLimits::new()).expect("the built-in skill builds")
    }

    fn tool_with_cap(cap_chars: usize) -> SkillTool {
        SkillTool::new(
            vec![("ops".to_owned(), deploy())],
            builtin(),
            context(),
            session(),
            cap_chars,
            Arc::new(Redactor::new()),
        )
        .expect("the tool builds")
    }

    fn tool() -> SkillTool {
        tool_with_cap(8_000)
    }

    fn toolset(name: &str, source: &str) -> Toolset {
        Toolset::new(
            name.to_owned(),
            format!("the toolset {name}"),
            Semver::parse("1.0.0").expect("a sample version must parse"),
            source.to_owned(),
        )
        .expect("a valid toolset builds")
    }

    /// Scratchpad over the grants of `toolsets`, with `session_context()` registered from `context`, and no secret declared.
    fn pad(toolsets: &[Toolset], context: &ResolvedContext) -> Scratchpad {
        pad_with(toolsets, context, Arc::new(Redactor::new()))
    }

    /// Scratchpad over the grants of `toolsets`, with `session_context()` registered from `context` and `redactor`.
    fn pad_with(toolsets: &[Toolset], context: &ResolvedContext, redactor: Arc<Redactor>) -> Scratchpad {
        let grants =
            assemble(toolsets, &[], &BTreeMap::new(), context, &ScratchpadLimits::new()).into_grants();
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), grants)
            .with_registration(session_context_registration(context.clone(), redactor))
            .build()
            .expect("the engine builds");
        Scratchpad::new(Arc::new(engine))
    }

    /// A secret that no heuristic of `ToolsetContext` flags, declared to the redactor of the session.
    const CANARY: &str = "p4ss-reelle-XYZ";

    /// A redactor that masks the canary, as the host declares a secret of the session.
    fn redactor_with_canary() -> Arc<Redactor> {
        let mut redactor = Redactor::new();
        redactor.add_secret(CANARY).expect("the canary is long enough");
        Arc::new(redactor)
    }

    /// A resolved context whose shared parameter `api_token` holds the canary.
    fn canary_context() -> ResolvedContext {
        ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter("api_token".to_owned(), json!(CANARY))
            .expect("a plain parameter")
            .resolve(Vec::<(String, String, serde_json::Value)>::new())
            .expect("the context resolves")
    }

    async fn run(pad: &Scratchpad, script: &str) -> RunOutcome {
        let redactor = Redactor::new();
        let clock = ManualClock::new(Timestamp::from_millis(0));
        let cancel = CancelHandle::new();
        let env = RunEnv::new("agent", &redactor, &clock);
        pad.run(script, &cancel, &env).await
    }

    // Scenario: le tool skill fonctionne sans rhai_pad
    #[test]
    fn skill_tool_works_without_rhai_pad() {
        let tool = tool();
        let list = tool.call(None, None).expect("the list is given");
        assert!(
            list.contains("- deploy: Deploy to prod"),
            "the list gives the rendered description: {list}"
        );
        assert!(list.contains("rhai-pad"), "the built-in skill is listed: {list}");
        let body = tool.call(Some("deploy"), None).expect("the body is given");
        assert_eq!(
            body, "Target: prod in us",
            "the body is rendered with the parameters of its toolset"
        );
        let annex = tool
            .call(Some("deploy"), Some("queries"))
            .expect("the annex is given");
        assert_eq!(annex, "PromQL text", "the annex document is given as written");
        let built_in = tool
            .call(Some("rhai-pad"), None)
            .expect("the built-in body is given");
        assert!(
            built_in.contains("vars()"),
            "the built-in body teaches the introspection: {built_in}"
        );
        assert!(
            built_in.contains("Sandbox rules"),
            "the built-in body cites the limits: {built_in}"
        );
    }

    // Scenario: un skill inconnu liste les noms
    #[test]
    fn unknown_skill_lists_the_names() {
        let tool = tool();
        match tool.call(Some("nope"), None) {
            Err(ToolsetsError::InvalidSkill { name, known, .. }) => {
                assert_eq!(name, "nope", "the name carries only the requested name");
                assert_eq!(
                    known,
                    ["deploy", "rhai-pad"],
                    "the existing names are listed apart, in name order"
                );
            }
            other => panic!("an unknown skill must fail with InvalidSkill, got {other:?}"),
        }
        match tool.call(Some("deploy"), Some("nope")) {
            Err(ToolsetsError::InvalidSkill { name, known, .. }) => {
                assert_eq!(name, "nope", "the name carries only the requested document");
                assert_eq!(known, ["queries"], "the existing documents are listed apart");
            }
            other => panic!("an unknown document must fail with InvalidSkill, got {other:?}"),
        }
    }

    #[test]
    fn document_without_a_skill_is_refused() {
        let refused = tool().call(None, Some("queries"));
        assert!(
            matches!(refused, Err(ToolsetsError::InvalidSkill { .. })),
            "a document needs a skill name, got {refused:?}"
        );
    }

    #[test]
    fn texts_are_capped_by_truncate_middle() {
        let capped = tool_with_cap(40);
        let body = capped
            .call(Some("rhai-pad"), None)
            .expect("the built-in body is given");
        assert!(
            body.chars().count() <= 40,
            "the text is capped at 40 characters: {body}"
        );
        let full = tool()
            .call(Some("rhai-pad"), None)
            .expect("the built-in body is given");
        assert_ne!(body, full, "the capped text is not the full text");
    }

    #[test]
    fn a_skill_name_taken_twice_is_refused() {
        let refused = SkillTool::new(
            vec![("ops".to_owned(), builtin())],
            builtin(),
            context(),
            session(),
            8_000,
            Arc::new(Redactor::new()),
        );
        assert!(
            matches!(refused, Err(ToolsetsError::InvalidSkill { .. })),
            "a granted skill named like the built-in one is refused"
        );
    }

    #[test]
    fn builtin_teaches_the_tool_the_introspection_and_the_limits() {
        let limits = ScratchpadLimits::new();
        let text = builtin_rhai_pad_skill(&limits).expect("the built-in skill builds");
        let body = text.body();
        for expected in [
            "rhai_pad",
            "vars()",
            "modules()",
            "functions()",
            "toolsets()",
            "skills()",
            "find(",
            "rules()",
            "session_context()",
            "Sandbox rules",
        ] {
            assert!(
                body.contains(expected),
                "the built-in skill must name `{expected}`: {body}"
            );
        }
        assert!(
            body.contains(&limits.render_cap_chars().to_string()),
            "the built-in skill states the render cap: {body}"
        );
        assert!(
            !body.contains("{{"),
            "the built-in body must not hold template markers"
        );
    }

    #[test]
    fn builtin_larger_than_the_render_cap_is_refused() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(100);
        let refused = builtin_rhai_pad_skill(&limits);
        assert!(
            matches!(refused, Err(ToolsetsError::SkillTooLarge { cap_chars: 100, .. })),
            "a built-in text over the render cap is refused, got {refused:?}"
        );
    }

    // Scenario: session_context est une copie
    #[tokio::test(flavor = "multi_thread")]
    async fn session_context_is_a_copy() {
        let context = context();
        let pad = pad(&[], &context);
        let outcome = run(
            &pad,
            "let first = session_context(); first.params.cluster = \"changed\"; session_context().params.cluster",
        )
        .await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the script runs: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("prod"),
            "the value is unchanged: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("changed"),
            "the copy did not reach the context: {}",
            outcome.text()
        );
    }

    // Scenario: même valeur depuis un module et depuis un script
    #[tokio::test(flavor = "multi_thread")]
    async fn module_function_and_script_read_the_same_value() {
        let context = context();
        let pad = pad(
            &[toolset(
                "ops",
                "fn cluster() { session_context().params.cluster }",
            )],
            &context,
        );
        let from_module = run(&pad, "ops::cluster()").await;
        let from_script = run(&pad, "session_context().params.cluster").await;
        assert_eq!(
            from_module.status(),
            RunStatus::Ok,
            "the module function runs: {}",
            from_module.text()
        );
        assert_eq!(
            from_script.status(),
            RunStatus::Ok,
            "the script runs: {}",
            from_script.text()
        );
        assert_eq!(from_module.text(), from_script.text(), "both read the same value");
        assert!(
            from_module.text().contains("prod"),
            "the value is the one of the context: {}",
            from_module.text()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_context_gives_role_names_never_references() {
        let context = context();
        let pad = pad(&[], &context);
        let outcome = run(&pad, "session_context()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the script runs: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("k8s"),
            "the bound role name is given: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains("prod-k8s"),
            "the reference of a role never comes out: {}",
            outcome.text()
        );
    }

    // Scenario: un secret déclaré sous forme de nombre est masqué, un nombre sans secret reste un nombre
    #[tokio::test(flavor = "multi_thread")]
    async fn session_context_masks_a_secret_held_as_a_number_and_keeps_the_others() {
        let mut redactor = Redactor::new();
        redactor.add_secret("7351904").expect("the number is long enough");
        let context = ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter("pin".to_owned(), json!(7_351_904))
            .expect("a plain parameter")
            .with_parameter("replicas".to_owned(), json!(3))
            .expect("a plain parameter")
            .resolve(Vec::<(String, String, serde_json::Value)>::new())
            .expect("the context resolves");
        let pad = pad_with(&[], &context, Arc::new(redactor));
        let outcome = run(&pad, "session_context()").await;
        assert!(
            !outcome.text().contains("7351904"),
            "the number that is a declared secret never comes out: {}",
            outcome.text()
        );
        assert!(
            holds(&pad, "session_context().params.replicas + 1 == 4").await,
            "a number that holds no secret stays a number"
        );
    }

    // Scenario: un secret déclaré sous forme de clé est masqué
    #[tokio::test(flavor = "multi_thread")]
    async fn session_context_masks_a_declared_secret_used_as_a_key() {
        let context = ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter(CANARY.to_owned(), json!("visible"))
            .expect("a plain parameter")
            .resolve(Vec::<(String, String, serde_json::Value)>::new())
            .expect("the context resolves");
        let pad = pad_with(&[], &context, redactor_with_canary());
        let outcome = run(&pad, "session_context()").await;
        assert!(
            !outcome.text().contains(CANARY),
            "the declared secret never comes out, as a key either: {}",
            outcome.text()
        );
    }

    /// Runs `script` and tells whether it answers `true`. A script that fails does not hold.
    async fn holds(pad: &Scratchpad, script: &str) -> bool {
        let outcome = run(pad, script).await;
        outcome.status() == RunStatus::Ok && outcome.text().trim() == "true"
    }

    /// One documented filler function of the toolset `ops`.
    fn filler(index: usize) -> String {
        format!("/// Filler {index}.\nfn filler_{index}() {{ {index} }}\n")
    }

    /// The toolset `ops`: the documented function `restart_pod`, and sixty filler functions, so that `find` reaches its cap.
    fn ops_toolset() -> Toolset {
        let fillers: String = (0..60).map(filler).collect();
        toolset(
            "ops",
            &format!("/// Restarts a pod by name.\nfn restart_pod(name) {{ name }}\n{fillers}"),
        )
    }

    // Scenario: un secret déclaré à la session n'apparaît jamais dans session_context()
    #[tokio::test(flavor = "multi_thread")]
    async fn session_context_masks_a_declared_secret() {
        let pad = pad_with(&[], &canary_context(), redactor_with_canary());
        let outcome = run(&pad, "session_context()").await;
        assert_eq!(
            outcome.status(),
            RunStatus::Ok,
            "the script runs: {}",
            outcome.text()
        );
        assert!(
            !outcome.text().contains(CANARY),
            "the declared secret never comes out: {}",
            outcome.text()
        );
        assert!(
            outcome.text().contains("***"),
            "the secret is masked where it was: {}",
            outcome.text()
        );
    }

    // Scenario: un secret déclaré n'apparaît jamais dans le texte d'un skill
    #[test]
    fn skill_tool_masks_a_declared_secret() {
        let skill = Skill::new(
            "leak".to_owned(),
            "Token {{context.params.api_token}}".to_owned(),
            "Body {{context.params.api_token}}".to_owned(),
        )
        .expect("a valid skill")
        .with_annex("notes".to_owned(), format!("key {CANARY}"))
        .expect("a valid annex");
        let tool = SkillTool::new(
            vec![("ops".to_owned(), skill)],
            builtin(),
            canary_context(),
            session(),
            8_000,
            redactor_with_canary(),
        )
        .expect("the tool builds");
        let list = tool.call(None, None).expect("the list is given");
        let body = tool.call(Some("leak"), None).expect("the body is given");
        let annex = tool
            .call(Some("leak"), Some("notes"))
            .expect("the annex is given");
        for (what, text) in [("list", &list), ("body", &body), ("annex", &annex)] {
            assert!(
                !text.contains(CANARY),
                "the {what} never carries the declared secret: {text}"
            );
        }
        assert!(body.contains("***"), "the secret is masked where it was: {body}");
    }

    // Scenario: session_context donne les paramètres résolus par toolset, défauts compris
    #[tokio::test(flavor = "multi_thread")]
    async fn session_context_gives_the_resolved_parameters_of_each_toolset() {
        let context = ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_override("ops".to_owned(), "region".to_owned(), json!("us"))
            .expect("a plain override")
            .resolve([("ops".to_owned(), "zone".to_owned(), json!("eu"))])
            .expect("the defaults resolve");
        let pad = pad(&[], &context);
        assert!(
            holds(&pad, r#"session_context().toolsets.ops.region == "us""#).await,
            "the override of the toolset is given"
        );
        assert!(
            holds(&pad, r#"session_context().toolsets.ops.zone == "eu""#).await,
            "the default of the toolset is given"
        );
    }

    // Scenario: chaque affirmation du skill intégré sur le scratchpad est vraie
    #[tokio::test(flavor = "multi_thread")]
    async fn built_in_claims_hold_in_the_scratchpad() {
        let pad = pad(&[ops_toolset()], &context());
        let first = run(&pad, "let kept = 7; kept * 2").await;
        assert_eq!(first.status(), RunStatus::Ok, "the script runs: {}", first.text());
        assert!(
            first.text().contains("14"),
            "the value of the last statement is the result: {}",
            first.text()
        );
        let second = run(&pad, "kept + 1").await;
        assert!(
            second.text().contains('8'),
            "`let` persists to the next run of the session: {}",
            second.text()
        );
        let printed = run(&pad, "print(\"first-line\"); 42").await;
        assert!(
            matches!(
                (printed.text().find("first-line"), printed.text().find("42")),
                (Some(text), Some(result)) if text < result
            ),
            "the text printed comes back before the result: {}",
            printed.text()
        );
        let claims = [
            r#""kept" in vars().map(|variable| variable.name)"#,
            r#""core" in modules().map(|entry| entry.kind)"#,
            r#""format" in modules().map(|entry| entry.kind)"#,
            r#""module" in modules().map(|entry| entry.kind)"#,
            r#""pure" in modules().map(|entry| entry.name)"#,
            r#""host" in modules().map(|entry| entry.name)"#,
            r#""ops" in modules().map(|entry| entry.name)"#,
            r#"functions("global").len() > 0"#,
            r#"functions("script").len() == 0"#,
            r#"functions("ops").len() == 61"#,
            r#""restart_pod" in find("restart pod").map(|found| found.name)"#,
            r#"find("restart nothing").len() == 0"#,
            r#"find("ops").len() == 50"#,
            "toolsets().len() == 1",
            r#"rules().contains("Sandbox rules")"#,
        ];
        let mut failed = Vec::new();
        for script in claims {
            let held = holds(&pad, script).await;
            failed.extend((!held).then_some(script));
        }
        assert!(failed.is_empty(), "the claims that do not hold: {failed:#?}");
        for script in [r#"functions("nope")"#, r#"find("")"#] {
            let refused = run(&pad, script).await;
            assert_ne!(
                refused.status(),
                RunStatus::Ok,
                "the claim that this call fails holds: {script}"
            );
        }
    }

    // Scenario: le texte du skill intégré décrit l'origine, le genre et la recherche
    #[test]
    fn built_in_body_describes_the_origins_the_kinds_and_the_search_rule() {
        let body = builtin().body().to_owned();
        for expected in [
            "functions(\"global\")",
            "functions(\"script\")",
            "pure",
            "format",
            "host",
            "every word",
            "50",
        ] {
            assert!(
                body.contains(expected),
                "the built-in text names `{expected}`: {body}"
            );
        }
        assert!(
            !body.contains("contains the text"),
            "the search is not a plain substring: {body}"
        );
    }

    // Scenario: skill appelé comme tool natif
    #[test]
    fn skill_is_called_as_a_native_tool() {
        let tool = tool();
        let native: &dyn NativeTool = &tool;
        assert_eq!(native.name(), "skill", "the tool is called `skill`");
        assert_eq!(
            native.definition(),
            skill_definition(),
            "its definition is the canonical one"
        );
        let body = native.call(&json!({"name": "rhai-pad"}));
        assert_eq!(
            Some(body.as_str()),
            tool.call(Some("rhai-pad"), None).ok().as_deref(),
            "a name gives the rendered body"
        );
        let list = native.call(&json!({}));
        assert!(
            list.contains("- deploy: Deploy to prod") && list.contains("- rhai-pad:"),
            "no argument gives the list: {list}"
        );
        let unknown = native.call(&json!({"name": "nope"}));
        assert!(
            unknown.contains("nope") && unknown.contains("deploy") && unknown.contains("rhai-pad"),
            "an unknown name gives an error text that lists the existing skills: {unknown}"
        );
        let annex = native.call(&json!({"name": "deploy", "document": "queries"}));
        assert_eq!(annex, "PromQL text", "a name and a document give the annex");
    }

    #[test]
    fn skill_definition_is_a_flat_definition_with_the_canonical_description() {
        let definition = skill_definition();
        assert_eq!(definition["name"], json!("skill"));
        assert_eq!(
            definition["description"],
            json!(
                "Read the skills granted to this session. With no argument, lists them (name and description). With `name`, returns the text of that skill; with `name` and `document`, one of its annex documents. Read `rhai-pad` first."
            ),
            "the description is the one of the spec"
        );
        let parameters = &definition["parameters"];
        assert_eq!(parameters["type"], json!("object"));
        assert_eq!(parameters["properties"]["name"]["type"], json!("string"));
        assert_eq!(parameters["properties"]["document"]["type"], json!("string"));
        assert!(
            parameters
                .get("required")
                .is_none_or(|required| required == &json!([])),
            "no parameter is required: {parameters}"
        );
        assert_eq!(skill_definition(), definition, "the definition is constant");
    }

    // Scenario: les exemples du skill intégré s'exécutent
    #[tokio::test(flavor = "multi_thread")]
    async fn built_in_examples_run_one_after_the_other() {
        let pad = pad(&[], &context());
        let first = run(&pad, FIRST_EXAMPLE).await;
        assert_eq!(
            first.status(),
            RunStatus::Ok,
            "the first call runs: {}",
            first.text()
        );
        assert_eq!(first.text().trim(), FIRST_RESULT, "the result the text announces");
        let second = run(&pad, SECOND_EXAMPLE).await;
        assert_eq!(
            second.status(),
            RunStatus::Ok,
            "the second call runs: {}",
            second.text()
        );
        assert_eq!(
            second.text().trim(),
            SECOND_RESULT,
            "the result the text announces"
        );
        assert!(
            SECOND_EXAMPLE.contains("names") && SECOND_EXAMPLE.contains("label("),
            "the second call uses the variable and the `fn` of the first"
        );
        let body = builtin().body().to_owned();
        for shown in [FIRST_EXAMPLE, FIRST_RESULT, SECOND_EXAMPLE, SECOND_RESULT] {
            assert!(body.contains(shown), "the skill shows `{shown}`: {body}");
        }
        assert!(
            FIRST_EXAMPLE.lines().count() <= 4 && SECOND_EXAMPLE.lines().count() <= 4,
            "the examples are short"
        );
    }

    // Scenario: chaque affirmation du skill intégré sur le scratchpad est vraie (valeur finale, abrégé de Rhai)
    #[tokio::test(flavor = "multi_thread")]
    async fn built_in_rhai_digest_and_final_value_claims_hold() {
        let pad = pad(&[ops_toolset()], &context());
        let claims = [
            // variables
            "let q = 4; q = q + 1; q == 5",
            // strings and interpolation
            "let who = \"pod\"; `a ${who}` == \"a pod\"",
            // arrays
            "let arr = [1, 2]; arr.push(3); arr[0] == 1 && arr.len() == 3",
            "[1, 2, 3].map(|v| v * 2) == [2, 4, 6]",
            "[1, 2, 3].filter(|v| v > 1) == [2, 3]",
            // maps
            "let m = #{name: \"web\", n: 1}; m.name == \"web\" && m[\"n\"] == 1 && m.keys().len() == 2",
            // closures see the variables of the scope, and may call a fn
            "let k = 10; [1, 2].map(|v| v + k) == [11, 12]",
            "fn twice(n) { n * 2 } [1, 2].map(|v| twice(v)) == [2, 4]",
            // loops
            "let t = 0; for v in [1, 2, 3] { t += v; } for i in 0..3 { t += i; } t == 9",
            "let w = 0; while w < 3 { w += 1; } w == 3",
            // fn returns its last expression
            "fn add(a, b) { a + b } add(1, 2) == 3",
            // try / catch is a statement: keep the value in a variable
            "let msg = \"\"; try { functions(\"nope\"); } catch (e) { msg = `failed: ${e}`; } msg.contains(\"failed\")",
            "let kept = \"none\"; try { throw \"boom\"; } catch (e) { kept = e; } kept == \"boom\"",
            // result shapes of the discovery functions
            "modules().all(|e| \"name\" in e && \"kind\" in e && \"functions\" in e)",
            "functions(\"ops\").all(|e| \"path\" in e && \"signature\" in e && \"doc\" in e && \"origin\" in e)",
            "find(\"restart pod\").all(|e| \"kind\" in e && \"name\" in e && \"path\" in e && \"text\" in e)",
            "find(\"restart pod\")[0].path == \"ops::restart_pod\"",
            "toolsets().all(|e| \"name\" in e && \"description\" in e)",
            "let seen = vars(); true",
            // session_context() from a script and from a fn of the script
            "session_context().params.cluster == \"prod\"",
            "fn cluster() { session_context().params.cluster } cluster() == \"prod\"",
            "session_context().roles.contains(\"k8s\") && session_context().toolsets.ops.region == \"us\"",
        ];
        let semicolon = run(&pad, "1 + 1;").await;
        assert_eq!(
            semicolon.text().trim(),
            "2",
            "a final `;` does not hide the value of the last statement"
        );
        for script in [
            "let a = 1;",
            "for i in 0..2 { }",
            "if false { 1 }",
            "fn unit() { 1 }",
            "print(\"x\");",
        ] {
            let outcome = run(&pad, script).await;
            assert!(
                outcome.status() == RunStatus::Ok && outcome.text().trim_end().ends_with("()"),
                "this statement gives `()`: {script}: {}",
                outcome.text()
            );
        }
        let mut failed = Vec::new();
        for script in claims {
            let outcome = run(&pad, script).await;
            let held = outcome.status() == RunStatus::Ok && outcome.text().trim() == "true";
            failed.extend((!held).then_some((script, outcome.text().to_owned())));
        }
        assert!(failed.is_empty(), "the claims that do not hold: {failed:#?}");
        for script in ["unknown_variable = 3", "let i = 1; fn bad() { i + 1 } bad()"] {
            let refused = run(&pad, script).await;
            assert_ne!(refused.status(), RunStatus::Ok, "this call fails: {script}");
        }
    }

    // Scenario: skill appelé comme tool natif (le skill rhai-pad est lisible par le tool, et ses affirmations sur le tool existent)
    #[test]
    fn rhai_pad_skill_is_readable_through_the_skill_tool_and_names_the_digest() {
        let body = tool().call(Some("rhai-pad"), None).expect("the skill is read");
        for expected in [
            "session_context()",
            "try {",
            "catch (e)",
            "`pod-${",
            "#{",
            "|v|",
            "while",
            "for ",
            "functions()",
            "find(",
        ] {
            assert!(body.contains(expected), "the skill names `{expected}`: {body}");
        }
    }

    // Scenario: chaque affirmation du skill intégré sur le scratchpad est vraie (colonnes de `vars()`, `skills()`, appel de module, coupure du milieu)
    #[tokio::test(flavor = "multi_thread")]
    async fn built_in_claims_on_vars_skills_module_calls_and_truncation_hold() {
        let limits = ScratchpadLimits::new().with_render_cap_chars(300);
        let grants = assemble(&[ops_toolset()], &[], &BTreeMap::new(), &context(), &limits)
            .into_grants()
            .with_skill(
                GrantedSkill::new("deploy", "Deploy to production", None).expect("a valid granted skill"),
            )
            .expect("the skill is granted");
        let engine = ScratchpadEngine::builder(limits, grants)
            .with_registration(session_context_registration(context(), Arc::new(Redactor::new())))
            .build()
            .expect("the engine builds");
        let pad = Scratchpad::new(Arc::new(engine));
        // `vars()`: columns `name`, `type` and `preview`, in the next run
        let declared = run(&pad, "let n = 3; let s = \"hello\"; n").await;
        assert_eq!(declared.status(), RunStatus::Ok, "{}", declared.text());
        for claim in [
            r#"vars().filter(|v| v.name == "n").map(|v| v.type + ":" + v.preview) == ["i64:3"]"#,
            r#"vars().filter(|v| v.name == "s").map(|v| v.type + ":" + v.preview) == ["string:hello"]"#,
            // `skills()`: name, description and toolset
            r#"skills().map(|k| k.name + ":" + k.description) == ["deploy:Deploy to production"]"#,
            "skills()[0].toolset == ()",
            // `module::function(arguments)`
            r#"ops::restart_pod("x") == "x""#,
        ] {
            assert!(holds(&pad, claim).await, "this claim does not hold: {claim}");
        }
        // the middle of a result over the render cap is replaced by a notice
        let long = run(
            &pad,
            "let big = \"\"; for i in 0..200 { big += \"abcdefghij\"; } big",
        )
        .await;
        let text = long.text();
        assert!(text.starts_with("abcdefghij"), "the start is kept: {text}");
        assert!(
            text.contains("output truncated"),
            "the notice replaces the middle: {text}"
        );
        assert!(
            text.chars().count() <= 300,
            "the output stays under the cap: {}",
            text.chars().count()
        );
        assert!(
            builtin()
                .body()
                .contains("the middle of a longer result is replaced by a notice")
        );
    }
}
