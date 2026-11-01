//! The tool definitions of a session by nature (`agent-sessions#FR-1`, `AC-6`, `AC-6bis`). See `src/tools/tools.sdd`.

use serde_json::Value;

use crate::runtime::rhai_pad_definition;
use crate::scratchpad::rhai_prod_inspect_definition;
use crate::session::SessionKind;
use crate::toolsets::skill_definition;

/// The ordered tool definitions a session of the given nature sends to the model.
///
/// `Work` and `Sub` get `rhai_pad` and `skill`; `Config` gets `rhai_pad`, `skill` and `rhai_prod_inspect`, in that order.
/// Nothing else is added. The value is constant by nature: two calls give equal values.
#[must_use]
pub fn session_tool_definitions(kind: SessionKind) -> Vec<Value> {
    let mut definitions = vec![rhai_pad_definition(), skill_definition()];
    if matches!(kind, SessionKind::Config) {
        definitions.push(rhai_prod_inspect_definition());
    }
    definitions
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::session_tool_definitions;
    use crate::runtime::rhai_pad_definition;
    use crate::scratchpad::rhai_prod_inspect_definition;
    use crate::session::SessionKind;
    use crate::toolsets::skill_definition;

    /// The names of a list of definitions, in order.
    fn names(definitions: &[Value]) -> Vec<&str> {
        definitions
            .iter()
            .filter_map(|definition| definition.get("name").and_then(Value::as_str))
            .collect()
    }

    // Scenario: un agent de travail ou un sous-agent a deux tools
    #[test]
    fn a_work_or_sub_agent_has_two_tools() {
        for kind in [SessionKind::Work, SessionKind::Sub] {
            let definitions = session_tool_definitions(kind);
            assert_eq!(
                names(&definitions),
                ["rhai_pad", "skill"],
                "{kind:?} has exactly two tools"
            );
            assert_eq!(
                definitions,
                vec![rhai_pad_definition(), skill_definition()],
                "no copy of the text"
            );
        }
    }

    // Scenario: un agent de configuration a trois tools
    #[test]
    fn a_config_agent_has_three_tools() {
        let definitions = session_tool_definitions(SessionKind::Config);
        assert_eq!(
            names(&definitions),
            ["rhai_pad", "skill", "rhai_prod_inspect"],
            "three tools, in order"
        );
        assert_eq!(
            definitions,
            vec![
                rhai_pad_definition(),
                skill_definition(),
                rhai_prod_inspect_definition()
            ],
            "the definitions are the canonical ones"
        );
    }

    // Must (tools.sdd) : le résultat est constant par nature
    #[test]
    fn the_result_is_constant_by_kind() {
        for kind in [SessionKind::Work, SessionKind::Config, SessionKind::Sub] {
            assert_eq!(
                session_tool_definitions(kind),
                session_tool_definitions(kind),
                "{kind:?} is constant"
            );
        }
    }
}
