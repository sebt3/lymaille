//! Native tools of the toolsets domain: the `skill` tool, which reads the skills without Rhai, and the `session_context()`
//! function, which the Rhai code of a session reads as a copy (`agent-sessions/toolsets#FR-17`, `FR-18`, `AC-10`, `AC-11`).
//! See `src/toolsets/native.sdd`.

use std::collections::BTreeMap;

use rhai::serde::to_dynamic;
use rhai::{Dynamic, Engine as RhaiEngine, EvalAltResult, FuncRegistration, Module, Shared};
use serde::Serialize;
use serde_json::Value;

use super::{ResolvedContext, SessionMeta, Skill, ToolsetsError, ToolsetsResult, render_skill};
use crate::scratchpad::{ScratchpadLimits, ScratchpadResult, limits_notice, truncate_middle};

/// Name of the built-in skill that teaches the `rhai_pad` tool.
const BUILTIN_NAME: &str = "rhai-pad";

/// Description of the built-in skill, as the model sees it in the list of skills.
const BUILTIN_DESCRIPTION: &str = "Run a Rhai script in this session's scratchpad, and discover its functions, modules, toolsets, skills and limits.";

/// Text of the built-in skill before the sandbox notice. A draft in English, to be reread by the lead developer.
const BUILTIN_BODY: &str = "Use `rhai_pad` to run one Rhai script in this session's scratchpad. The value of the last statement is the result. \
Variables declared with `let` persist to the next run of this session. Call the functions of a granted module as `module::function(arguments)`. \
Text printed with `print` or `debug` comes back before the result.

Discover what is available before you write a script:
- `vars()`: the variables of the scope at the start of this run, with their type and a preview of their value.
- `modules()`: the granted modules.
- `functions()`: every function you can call, with its signature and documentation; `functions(\"module\")` lists the functions of one module.
- `toolsets()`: the granted toolsets, with their descriptions.
- `skills()`: the granted skills, with their descriptions. Read the text of a skill with the `skill` tool, not with a script.
- `find(\"text\")`: the functions, modules, toolsets and skills whose name or description contains the text, in any case.
- `rules()`: the sandbox rules and the limits.

`session_context()` returns a copy of the toolset context of this session: `params` holds the shared parameters, `toolsets` holds the resolved parameters of each toolset, and `roles` holds the names of the bound roles. Changing the copy does not change the context.

";

/// A granted skill as the `skill` tool holds it: the toolset it belongs to, when it has one, and the skill.
#[derive(Debug)]
struct Entry {
    toolset: Option<String>,
    skill: Skill,
}

/// The `skill` tool, which reads the granted skills without Rhai.
///
/// It lists the skills, gives the rendered text of one, or one of its annex documents. Every text it gives is capped by
/// `truncate_middle` (`agent-sessions/toolsets#FR-18`, `AC-11`).
#[derive(Debug)]
#[non_exhaustive]
pub struct SkillTool {
    entries: BTreeMap<String, Entry>,
    context: ResolvedContext,
    session: SessionMeta,
    cap_chars: usize,
}

impl SkillTool {
    /// Builds the tool over the granted skills, each with its toolset, and over the built-in skill.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] when two skills share a name, the built-in one included.
    pub fn new(
        granted: Vec<(String, Skill)>,
        builtin: Skill,
        context: ResolvedContext,
        session: SessionMeta,
        cap_chars: usize,
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
        })
    }

    /// Answers the `skill` tool. With no name, the list of the skills, name and description. With a name, the rendered body
    /// of that skill. With a name and a document name, that annex document.
    ///
    /// # Errors
    ///
    /// Returns [`ToolsetsError::InvalidSkill`] for an unknown skill or document, the message listing the existing names,
    /// and [`ToolsetsError::Template`] when a description or a body does not render.
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

    /// The text capped at the render cap, its middle replaced by the notice of `truncate_middle`.
    fn capped(&self, text: &str) -> String {
        truncate_middle(text, self.cap_chars).text().to_owned()
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
/// It is passed to `ScratchpadEngine::builder`'s `with_registration`. The function gives a copy, built at each call, of the given resolved context: the shared parameters, the parameters by
/// toolset and the names of the bound roles. No reference of a role and no secret is in it. It is registered with
/// `FuncRegistration`, which declares it non-volatile, that is deterministic for Rhai: `register_fn` would declare it
/// volatile.
pub fn session_context_registration(
    context: ResolvedContext,
) -> impl FnOnce(&mut RhaiEngine) -> ScratchpadResult<()> + 'static {
    move |engine| {
        let mut module = Module::new();
        let _function = FuncRegistration::new("session_context")
            .in_global_namespace()
            .set_into_module(&mut module, move || -> Result<Dynamic, Box<EvalAltResult>> {
                to_dynamic(ContextCopy {
                    params: context.shared(),
                    toolsets: context.toolsets(),
                    roles: context.roles(),
                })
            });
        let _registered = engine.register_global_module(Shared::new(module));
        Ok(())
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
    let body = format!("{BUILTIN_BODY}{}", limits_notice(limits));
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

    use super::{SkillTool, builtin_rhai_pad_skill, session_context_registration};
    use crate::clock::{ManualClock, Timestamp};
    use crate::scratchpad::{RunEnv, RunOutcome, RunStatus, Scratchpad, ScratchpadEngine, ScratchpadLimits};
    use crate::session::{CancelHandle, Redactor};
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

    /// Scratchpad over the grants of `toolsets`, with `session_context()` registered from `context`.
    fn pad(toolsets: &[Toolset], context: &ResolvedContext) -> Scratchpad {
        let grants =
            assemble(toolsets, &[], &BTreeMap::new(), context, &ScratchpadLimits::new()).into_grants();
        let engine = ScratchpadEngine::builder(ScratchpadLimits::new(), grants)
            .with_registration(session_context_registration(context.clone()))
            .build()
            .expect("the engine builds");
        Scratchpad::new(Arc::new(engine))
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
}
