//! Handlebars rendering of the system prompt and of the description and body of the skills, with the data of the resolved
//! toolset context and of the session metadata frozen at rendering (`agent-sessions/toolsets#FR-6`, `AC-2`, `AC-12`).
//! The engine is strict (an absent variable is an error), keeps only its default helpers, and does not escape HTML: the
//! text goes to a language model.

use std::collections::BTreeMap;

use handlebars::{Handlebars, RenderError, RenderErrorReason, no_escape};
use serde::Serialize;
use serde_json::Value;

use super::{ResolvedContext, Skill, ToolsetsError, ToolsetsResult};

/// Name under which the system prompt is reported in a [`ToolsetsError::Template`].
const SYSTEM_PROMPT_NAME: &str = "system prompt";

/// Session metadata given to a template.
///
/// It holds the identifier of the agent, the start date as text written by the host, the granted toolsets with their
/// descriptions, the names of the granted skills (never their descriptions or bodies), and the number of parallelizable
/// sub-agents for a main agent (`agent-sessions/delegation#FR-11`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SessionMeta {
    agent_id: String,
    started_at: String,
    toolsets: Vec<NamedText>,
    skills: Vec<String>,
    parallel_subagents: Option<u32>,
}

/// A name with a text, as the templates see a granted toolset: `{{name}}` and `{{description}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NamedText {
    name: String,
    description: String,
}

impl SessionMeta {
    /// Builds the metadata of a session. The toolsets are given as `(name, description)`, the skills by name only, and
    /// `parallel_subagents` is set for a main agent only.
    #[must_use]
    pub fn new(
        agent_id: String,
        started_at: String,
        toolsets: Vec<(String, String)>,
        skills: Vec<String>,
        parallel_subagents: Option<u32>,
    ) -> Self {
        Self {
            agent_id,
            started_at,
            toolsets: toolsets
                .into_iter()
                .map(|(name, description)| NamedText { name, description })
                .collect(),
            skills,
            parallel_subagents,
        }
    }
}

/// Renders the system prompt template with the resolved context and the session metadata.
///
/// # Errors
///
/// Returns [`ToolsetsError::Template`] when the template does not compile or references a variable that is absent.
pub fn render_prompt(
    template: &str,
    context: &ResolvedContext,
    session: &SessionMeta,
) -> ToolsetsResult<String> {
    render(
        SYSTEM_PROMPT_NAME,
        template,
        &RenderData::new(context.shared(), context, session),
    )
}

/// Renders the description and the body of a skill of the toolset `toolset`, with the resolved context and the session
/// metadata. Returns `(description, body)`.
///
/// The `context.params` of the skill are the shared parameters, overlaid with the resolved parameters of this toolset
/// only: a skill never sees the parameters of another toolset.
///
/// # Errors
///
/// Returns [`ToolsetsError::Template`] when the description or the body does not compile or references an absent variable.
pub fn render_skill(
    skill: &Skill,
    toolset: &str,
    context: &ResolvedContext,
    session: &SessionMeta,
) -> ToolsetsResult<(String, String)> {
    let params = skill_params(context, toolset);
    let data = RenderData::new(&params, context, session);
    let description = render(
        &format!("skill {} description", skill.name()),
        skill.description(),
        &data,
    )?;
    let body = render(&format!("skill {} body", skill.name()), skill.body(), &data)?;
    Ok((description, body))
}

/// The parameters a skill of `toolset` sees: the shared parameters of the context, overlaid with the resolved parameters of
/// that toolset. The parameters of the other toolsets stay out.
fn skill_params(context: &ResolvedContext, toolset: &str) -> BTreeMap<String, Value> {
    let mut params = context.shared().clone();
    if let Some(own) = context.toolsets().get(toolset) {
        for (name, value) in own {
            let _previous = params.insert(name.clone(), value.clone());
        }
    }
    params
}

/// Checks that a template compiles, without rendering it. `name` is the name the template is reported under.
///
/// # Errors
///
/// Returns [`ToolsetsError::Template`] when the template does not compile.
pub fn check_template(name: &str, source: &str) -> ToolsetsResult<()> {
    engine()
        .register_template_string(name, source)
        .map_err(|error| template_error(name, &error.to_string()))
}

/// Data of a render: `context.params` (the parameters the template sees), `context.roles` (each bound role, as `true`) and
/// `session`. Role references and secrets never enter it.
#[derive(Serialize)]
struct RenderData<'data> {
    context: ContextData<'data>,
    session: &'data SessionMeta,
}

/// The `context` section of the render data.
#[derive(Serialize)]
struct ContextData<'data> {
    params: &'data BTreeMap<String, Value>,
    roles: BTreeMap<&'data str, bool>,
}

impl<'data> RenderData<'data> {
    /// Gathers the render data: the given parameters, the roles of the resolved context and the session metadata.
    fn new(
        params: &'data BTreeMap<String, Value>,
        context: &'data ResolvedContext,
        session: &'data SessionMeta,
    ) -> Self {
        Self {
            context: ContextData {
                params,
                roles: context.roles().iter().map(|role| (role.as_str(), true)).collect(),
            },
            session,
        }
    }
}

/// Builds a Handlebars engine in strict mode, with the default helpers only and without HTML escaping.
fn engine() -> Handlebars<'static> {
    let mut registry = Handlebars::new();
    registry.set_strict_mode(true);
    registry.register_escape_fn(no_escape);
    registry
}

/// Compiles and renders `source` under the name `name`.
fn render(name: &str, source: &str, data: &RenderData<'_>) -> ToolsetsResult<String> {
    let mut registry = engine();
    registry
        .register_template_string(name, source)
        .map_err(|error| template_error(name, &error.to_string()))?;
    registry
        .render(name, data)
        .map_err(|error| template_error(name, &render_reason(&error)))
}

/// Names the reason of a render error. An absent variable is named by its path; the values never appear.
fn render_reason(error: &RenderError) -> String {
    if let RenderErrorReason::MissingVariable(Some(path)) = error.reason() {
        format!("variable `{path}` is absent")
    } else {
        error.reason().to_string()
    }
}

/// Builds the template error of the template `name`.
fn template_error(name: &str, reason: &str) -> ToolsetsError {
    ToolsetsError::Template {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{SYSTEM_PROMPT_NAME, SessionMeta, check_template, render_prompt, render_skill};
    use crate::toolsets::{ContextLevel, ResolvedContext, Skill, ToolsetContext, ToolsetsError};

    /// Resolved context with the given parameters and the given role bindings (role, reference).
    fn context(params: &[(&str, Value)], roles: &[(&str, &str)]) -> ResolvedContext {
        let mut built =
            ToolsetContext::new("site".to_owned(), ContextLevel::Project).expect("a context name");
        for (name, value) in params {
            built = built
                .with_parameter((*name).to_owned(), value.clone())
                .expect("a plain parameter");
        }
        for (role, reference) in roles {
            built = built
                .with_role((*role).to_owned(), (*reference).to_owned())
                .expect("a plain role");
        }
        built
            .resolve(std::iter::empty())
            .expect("an empty default list resolves")
    }

    /// Session metadata of a main agent with two toolsets and two skills.
    fn session() -> SessionMeta {
        SessionMeta::new(
            "main".to_owned(),
            "2026-10-29T09:00:00Z".to_owned(),
            vec![
                ("k8s-ops".to_owned(), "Kubernetes operations".to_owned()),
                ("notes".to_owned(), "Personal notes".to_owned()),
            ],
            vec!["deploy".to_owned(), "triage".to_owned()],
            Some(3),
        )
    }

    // Scenario: same template, two contexts
    #[test]
    fn same_template_two_contexts() {
        let template = "Cluster: {{context.params.cluster}}.";
        let session = session();
        let first = render_prompt(template, &context(&[("cluster", json!("prod"))], &[]), &session)
            .expect("the first context renders");
        let second = render_prompt(
            template,
            &context(&[("cluster", json!("staging"))], &[]),
            &session,
        )
        .expect("the second context renders");
        assert_eq!(first, "Cluster: prod.", "the first text holds its value");
        assert_eq!(second, "Cluster: staging.", "the second text holds its value");
    }

    // Scenario: absent role removes its section
    #[test]
    fn absent_role_removes_its_section() {
        let template = "start{{#if context.roles.k8s}} K8S SECTION{{/if}} end";
        let session = session();
        let without = render_prompt(template, &context(&[], &[]), &session).expect("the template renders");
        assert_eq!(without, "start end", "the section of an absent role is absent");
        let with = render_prompt(template, &context(&[], &[("k8s", "prod-k8s")]), &session)
            .expect("the template renders");
        assert!(
            with.contains("K8S SECTION"),
            "the section of a bound role is present: {with}"
        );
    }

    // Scenario: skill names without bodies
    #[test]
    fn skill_names_without_bodies() {
        let template = "Skills: {{#each session.skills}}[{{this}}]{{/each}}";
        let rendered = render_prompt(template, &context(&[], &[]), &session()).expect("the template renders");
        assert_eq!(
            rendered, "Skills: [deploy][triage]",
            "each granted skill appears by its name"
        );
        assert!(
            !rendered.contains("Use it when"),
            "no skill description or body enters the prompt: {rendered}"
        );
    }

    // Scenario: unknown variable is an error
    #[test]
    fn unknown_variable_is_an_error() {
        let refused = render_prompt("{{context.params.missing}}", &context(&[], &[]), &session());
        match refused {
            Err(ToolsetsError::Template { name, reason }) => {
                assert_eq!(name, SYSTEM_PROMPT_NAME, "the template is named");
                assert!(
                    reason.contains("context.params.missing"),
                    "the variable is named: {reason}"
                );
            }
            other => panic!("an absent variable must fail with Template, got {other:?}"),
        }
    }

    // Scenario: rendering is deterministic
    #[test]
    fn rendering_is_deterministic() {
        let template = "{{session.agent_id}} {{session.started_at}}\
            {{#each session.toolsets}} {{name}}={{description}}{{/each}} {{session.parallel_subagents}}";
        let context = context(&[("cluster", json!("prod"))], &[("k8s", "prod-k8s")]);
        let session = session();
        let first = render_prompt(template, &context, &session).expect("the template renders");
        let second = render_prompt(template, &context, &session).expect("the template renders again");
        assert_eq!(first, second, "the same template and data give the same text");
    }

    #[test]
    fn session_fields_reach_the_template() {
        let rendered = render_prompt(
            "{{session.agent_id}}|{{session.started_at}}|{{#each session.toolsets}}{{name}}:{{description}};{{/each}}|{{session.parallel_subagents}}",
            &context(&[], &[]),
            &session(),
        )
        .expect("the template renders");
        assert_eq!(
            rendered, "main|2026-10-29T09:00:00Z|k8s-ops:Kubernetes operations;notes:Personal notes;|3",
            "the agent, the start date, the toolsets with their descriptions and the parallel count are rendered"
        );
    }

    #[test]
    fn no_html_escape() {
        let rendered = render_prompt(
            "{{context.params.note}}",
            &context(&[("note", json!("a & <b> \"c\""))], &[]),
            &session(),
        )
        .expect("the template renders");
        assert_eq!(
            rendered, "a & <b> \"c\"",
            "the text goes to a language model, unescaped"
        );
    }

    #[test]
    fn only_default_helpers_are_registered() {
        let refused = render_prompt("{{env \"HOME\"}}", &context(&[], &[]), &session());
        assert!(
            matches!(refused, Err(ToolsetsError::Template { .. })),
            "a helper that reads the environment is not registered, got {refused:?}"
        );
    }

    #[test]
    fn skill_description_and_body_are_rendered() {
        let skill = Skill::new(
            "deploy".to_owned(),
            "Deploy on {{context.params.cluster}}".to_owned(),
            "Target: {{context.params.cluster}}".to_owned(),
        )
        .expect("a valid skill");
        let (description, body) = render_skill(
            &skill,
            "ops",
            &context(&[("cluster", json!("prod"))], &[]),
            &session(),
        )
        .expect("the skill renders");
        assert_eq!(description, "Deploy on prod", "the description is rendered");
        assert_eq!(body, "Target: prod", "the body is rendered");
    }

    #[test]
    fn skill_error_names_the_skill() {
        let skill = Skill::new(
            "deploy".to_owned(),
            "Deploy".to_owned(),
            "{{context.params.missing}}".to_owned(),
        )
        .expect("a valid skill");
        let refused = render_skill(&skill, "ops", &context(&[], &[]), &session());
        match refused {
            Err(ToolsetsError::Template { name, reason }) => {
                assert!(name.contains("deploy"), "the skill is named: {name}");
                assert!(
                    reason.contains("context.params.missing"),
                    "the variable is named: {reason}"
                );
            }
            other => panic!("an absent variable in a skill must fail with Template, got {other:?}"),
        }
    }

    #[test]
    fn check_template_refuses_a_broken_template() {
        let refused = check_template("skill deploy body", "{{#if open}}never closed");
        match refused {
            Err(ToolsetsError::Template { name, .. }) => {
                assert_eq!(name, "skill deploy body", "the template is named");
            }
            other => panic!("a template that does not compile must fail with Template, got {other:?}"),
        }
        assert!(
            check_template(
                "skill deploy body",
                "{{context.params.anything}} {{#if x}}ok{{/if}}"
            )
            .is_ok(),
            "a template that compiles is accepted without rendering"
        );
    }

    #[test]
    fn bound_role_is_a_boolean_and_its_reference_stays_out() {
        let rendered = render_prompt(
            "{{#each context.roles}}x{{/each}}{{#if context.roles.k8s}}K{{/if}}",
            &context(&[], &[("k8s", "prod-k8s")]),
            &session(),
        )
        .expect("the template renders");
        assert_eq!(
            rendered, "xK",
            "a bound role is a boolean, the reference never enters the data"
        );
        assert!(
            !rendered.contains("prod-k8s"),
            "the reference never enters the rendered text"
        );
    }

    /// Resolved context of two toolsets, `ops` and `other`, both with the default `region = eu`. The context overrides the
    /// region to `us` for `ops` only. `other` also declares `only_other`, which `ops` does not have.
    fn per_toolset_context() -> ResolvedContext {
        ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_override("ops".to_owned(), "region".to_owned(), json!("us"))
            .expect("a plain override")
            .resolve([
                ("ops".to_owned(), "region".to_owned(), json!("eu")),
                ("other".to_owned(), "region".to_owned(), json!("eu")),
                ("other".to_owned(), "only_other".to_owned(), json!("x")),
            ])
            .expect("the defaults resolve")
    }

    /// A skill whose body reads `context.params.region`.
    fn region_skill() -> Skill {
        Skill::new(
            "deploy".to_owned(),
            "Deploy".to_owned(),
            "Region: {{context.params.region}}".to_owned(),
        )
        .expect("a valid skill")
    }

    // Scenario: skill sees its toolset parameters
    #[test]
    fn skill_sees_its_toolset_parameters() {
        let resolved = per_toolset_context();
        let skill = region_skill();
        let (_, for_ops) =
            render_skill(&skill, "ops", &resolved, &session()).expect("the skill of ops renders");
        assert_eq!(
            for_ops, "Region: us",
            "the skill of ops sees the override of its toolset"
        );
        let (_, for_other) =
            render_skill(&skill, "other", &resolved, &session()).expect("the skill of other renders");
        assert_eq!(
            for_other, "Region: eu",
            "the same template for another toolset sees that toolset's own default"
        );
    }

    #[test]
    fn skill_does_not_see_another_toolset_parameters() {
        let resolved = per_toolset_context();
        let skill = Skill::new(
            "deploy".to_owned(),
            "Deploy".to_owned(),
            "{{context.params.only_other}}".to_owned(),
        )
        .expect("a valid skill");
        let refused = render_skill(&skill, "ops", &resolved, &session());
        assert!(
            matches!(refused, Err(ToolsetsError::Template { .. })),
            "a parameter of another toolset is absent for this skill, got {refused:?}"
        );
    }

    #[test]
    fn skill_keeps_the_shared_parameters() {
        let shared = ToolsetContext::new("site".to_owned(), ContextLevel::Project)
            .expect("a context name")
            .with_parameter("cluster".to_owned(), json!("prod"))
            .expect("a plain parameter")
            .resolve(std::iter::empty())
            .expect("an empty default list resolves");
        let skill = Skill::new(
            "deploy".to_owned(),
            "Deploy".to_owned(),
            "{{context.params.cluster}}".to_owned(),
        )
        .expect("a valid skill");
        let (_description, body) =
            render_skill(&skill, "ops", &shared, &session()).expect("the skill renders");
        assert_eq!(
            body, "prod",
            "a skill of a toolset without parameters still sees the shared ones"
        );
    }
}
