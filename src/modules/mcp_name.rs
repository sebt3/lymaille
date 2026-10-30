//! Name of the Rhai function derived from the name of an MCP tool, and the collision check of one server
//! (`agent-sessions/modules#FR-10`, `agent-sessions/modules#AC-6`). The identifier rules are those of
//! [`GrantedModule::new`](crate::scratchpad::GrantedModule::new): one definition, not a copy.

use rhai::{Module, Shared};

use super::{ModulesError, ModulesResult};
use crate::scratchpad::GrantedModule;

/// Name of the Rhai function of an MCP tool: each character that is not an ASCII letter, a digit or `_` becomes `_`.
/// `loki.query` gives `loki_query`; a name that is already valid is unchanged.
#[must_use]
pub fn function_name(tool: &str) -> String {
    tool.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Names the tools of one MCP server, in the order received, as pairs of (function name, original MCP tool name).
///
/// # Errors
///
/// Returns [`ModulesError::InvalidMcp`] when a tool name is empty, when the function name it gives is not a usable Rhai
/// identifier (for instance it starts with a digit or is a reserved word), or when two tools give the same function name.
/// The collision reason names both original tool names.
pub fn name_tools(role: &str, tools: &[&str]) -> ModulesResult<Vec<(String, String)>> {
    let mut named: Vec<(String, String)> = Vec::with_capacity(tools.len());
    for tool in tools {
        if tool.is_empty() {
            return Err(refused(role, "a tool name is empty"));
        }
        let function = function_name(tool);
        if !is_usable_function_name(&function) {
            return Err(refused(
                role,
                "a tool name does not give a usable Rhai function name",
            ));
        }
        if let Some((_, previous)) = named.iter().find(|(name, _)| *name == function) {
            return Err(refused(
                role,
                &format!("tools `{previous}` and `{tool}` both map to the function `{function}`"),
            ));
        }
        named.push((function, (*tool).to_owned()));
    }
    Ok(named)
}

/// Whether `name` is accepted as the name of a Rhai module, which is the rule of Rhai function names used by
/// [`GrantedModule::new`]: the same definition, not a copy.
fn is_usable_function_name(name: &str) -> bool {
    GrantedModule::new(name, Shared::new(Module::new())).is_some()
}

/// The refusal of an MCP module for `role`.
fn refused(role: &str, reason: &str) -> ModulesError {
    ModulesError::InvalidMcp {
        role: role.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ModulesError, function_name, name_tools};

    #[test]
    fn dots_and_dashes_become_underscores() {
        assert_eq!(function_name("loki.query"), "loki_query");
        assert_eq!(function_name("get-pods"), "get_pods");
    }

    #[test]
    fn valid_name_is_unchanged() {
        assert_eq!(function_name("list_pods"), "list_pods");
    }

    #[test]
    fn non_ascii_characters_become_underscores_one_by_one() {
        assert_eq!(function_name("\u{e9}"), "_");
        assert_eq!(function_name("a\u{e9}b"), "a_b");
        assert_eq!(function_name("ab\u{20ac}"), "ab_");
    }

    #[test]
    fn collision_refused_and_names_role_and_both_tools() {
        let error = name_tools("grafana", &["a.b", "a_b"]).expect_err("the two tools collide");
        let text = error.to_string();
        assert!(text.contains("grafana"), "the role must be named: {text}");
        assert!(text.contains("`a.b`"), "the first tool must be named: {text}");
        assert!(text.contains("`a_b`"), "the second tool must be named: {text}");
        assert!(
            matches!(error, ModulesError::InvalidMcp { .. }),
            "an MCP error: {text}"
        );
    }

    #[test]
    fn distant_collision_names_both_original_names() {
        let error = name_tools("grafana", &["a.b", "x", "a-b"]).expect_err("the tools collide at a distance");
        let text = error.to_string();
        assert!(text.contains("`a.b`"), "the first tool must be named: {text}");
        assert!(
            text.contains("`a-b`"),
            "the second original name must be named: {text}"
        );
    }

    #[test]
    fn identical_tool_names_collide() {
        assert!(
            matches!(
                name_tools("grafana", &["query", "query"]),
                Err(ModulesError::InvalidMcp { .. })
            ),
            "the same tool name twice is a collision"
        );
    }

    #[test]
    fn distinct_names_kept_in_order() {
        let named = name_tools("grafana", &["loki.query", "get-pods"]).expect("no collision");
        assert_eq!(
            named,
            vec![
                ("loki_query".to_owned(), "loki.query".to_owned()),
                ("get_pods".to_owned(), "get-pods".to_owned()),
            ]
        );
    }

    #[test]
    fn valid_names_are_kept_unchanged_by_name_tools() {
        let named = name_tools("grafana", &["list_pods"]).expect("valid");
        assert_eq!(named, vec![("list_pods".to_owned(), "list_pods".to_owned())]);
    }

    #[test]
    fn unusable_names_refused() {
        for tool in ["1abc", "fn", "let", "import", "const", "this", ""] {
            assert!(
                matches!(
                    name_tools("grafana", &[tool]),
                    Err(ModulesError::InvalidMcp { .. })
                ),
                "the tool `{tool}` must be refused, not deformed"
            );
        }
    }

    #[test]
    fn refusal_names_the_role() {
        let text = name_tools("grafana", &[""]).expect_err("empty name").to_string();
        assert!(text.contains("grafana"), "the role must be named: {text}");
    }
}
