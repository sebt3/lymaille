//! Session toolset selection: the [`ToolsetContext`] list is authoritative for a work or configuration agent, and a
//! sub-agent may only restrict it (`agent-sessions#BR-2`, `agent-sessions#DEC-16`, `agent-sessions/toolsets#FR-25`,
//! `agent-sessions/toolsets#AC-20`). Pure function, no I/O; catalog existence is checked elsewhere (`compat.rs`).

use crate::agents::{AgentDefinition, AgentKind};

use super::ToolsetContext;

/// The toolsets of a session, as selected by [`select_toolsets`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Selection {
    names: Vec<String>,
    ignored: Vec<String>,
}

impl Selection {
    /// Returns the names of the session toolsets, in the order of the context list.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Returns the names of the definition restriction that the context list does not carry, in the order of the
    /// restriction. They are reported to the host, never added to [`Selection::names`].
    #[must_use]
    pub fn ignored(&self) -> &[String] {
        &self.ignored
    }
}

/// Selects the toolsets of a session from the context list and the agent definition.
///
/// A work or configuration agent sees the whole list of the context: a definition of that kind carries no toolset
/// (`agent-sessions/agents#FR-1`). A sub-agent without a restriction sees the whole list too; with a restriction, it
/// sees the intersection, in the order of the context. An empty restriction is a restriction: no toolset at all; the
/// absence of one is not (`Option::None`). A restriction never extends the list of the context: its extra names are
/// reported in [`Selection::ignored`], never added.
#[must_use]
pub fn select_toolsets(context: &ToolsetContext, definition: &AgentDefinition) -> Selection {
    let listed = context.toolset_names();
    let restriction = match definition.kind() {
        AgentKind::Sub => definition.toolset_restriction(),
        AgentKind::Work | AgentKind::Config => None,
    };
    restriction.map_or_else(
        || Selection {
            names: listed.to_vec(),
            ignored: Vec::new(),
        },
        |restricted| Selection {
            names: listed
                .iter()
                .filter(|name| restricted.iter().any(|r| r == *name))
                .cloned()
                .collect(),
            ignored: restricted
                .iter()
                .filter(|name| !listed.iter().any(|l| l == *name))
                .cloned()
                .collect(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::select_toolsets;
    use crate::agents::{AgentDefinition, AgentKind};
    use crate::toolsets::{ContextLevel, ToolsetContext};

    /// A context listing `a` then `b`.
    fn context_ab() -> ToolsetContext {
        ToolsetContext::new("prod".to_owned(), ContextLevel::Session)
            .expect("the context builds")
            .with_toolset("a".to_owned())
            .expect("`a` is listed")
            .with_toolset("b".to_owned())
            .expect("`b` is listed")
    }

    fn definition(kind: AgentKind, restriction: Option<Vec<String>>) -> AgentDefinition {
        let built = AgentDefinition::new("agent".to_owned(), kind, "prompt".to_owned());
        match restriction {
            Some(toolsets) => built.with_toolset_restriction(toolsets),
            None => built,
        }
    }

    // Scenario: l'agent principal voit toute la liste du contexte
    #[test]
    fn main_agent_sees_the_whole_context_list() {
        let selection = select_toolsets(&context_ab(), &definition(AgentKind::Work, None));
        assert_eq!(
            selection.names(),
            ["a".to_owned(), "b".to_owned()],
            "the work agent sees the whole context list"
        );
        assert!(selection.ignored().is_empty(), "no restriction, nothing ignored");
        let config = select_toolsets(&context_ab(), &definition(AgentKind::Config, None));
        assert_eq!(
            config.names(),
            ["a".to_owned(), "b".to_owned()],
            "the configuration agent sees the whole context list too"
        );
    }

    // Scenario: un sous-agent restreint ne voit que l'intersection
    #[test]
    fn restricted_sub_agent_sees_only_the_intersection() {
        let sub = definition(AgentKind::Sub, Some(vec!["b".to_owned()]));
        let selection = select_toolsets(&context_ab(), &sub);
        assert_eq!(
            selection.names(),
            ["b".to_owned()],
            "the restricted sub-agent sees the intersection"
        );
        assert!(selection.ignored().is_empty(), "the restriction fits the list");
        // The intersection follows the order of the context, not the one of the restriction.
        let flipped = definition(AgentKind::Sub, Some(vec!["b".to_owned(), "a".to_owned()]));
        let flipped_selection = select_toolsets(&context_ab(), &flipped);
        assert_eq!(
            flipped_selection.names(),
            ["a".to_owned(), "b".to_owned()],
            "names follow the context order"
        );
        // An unrestricted sub-agent sees the whole list.
        let free = select_toolsets(&context_ab(), &definition(AgentKind::Sub, None));
        assert_eq!(
            free.names(),
            ["a".to_owned(), "b".to_owned()],
            "an unrestricted sub-agent sees the whole list"
        );
    }

    // Scenario: la restriction n'étend jamais la liste du contexte
    #[test]
    fn restriction_never_extends_the_context_list() {
        let sub = definition(AgentKind::Sub, Some(vec!["a".to_owned(), "c".to_owned()]));
        let selection = select_toolsets(&context_ab(), &sub);
        assert_eq!(
            selection.names(),
            ["a".to_owned()],
            "the extra name `c` is never added"
        );
        assert_eq!(
            selection.ignored(),
            ["c".to_owned()],
            "the extra name is reported in restriction order"
        );
    }

    // Scenario: une restriction vide ne laisse rien
    #[test]
    fn empty_restriction_selects_nothing() {
        let sub = definition(AgentKind::Sub, Some(Vec::new()));
        let selection = select_toolsets(&context_ab(), &sub);
        assert!(
            selection.names().is_empty(),
            "an empty restriction is a restriction: no toolset"
        );
        assert!(selection.ignored().is_empty(), "nothing to ignore");
    }
}
