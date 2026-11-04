//! Typed error of the `modules` domain. Messages name levels, identifiers, roles and tool names, never the body of a
//! Memory, an argument of an MCP call or a secret (`agent-sessions#CON-3`).

/// Error of the `modules` domain.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModulesError {
    /// A todo is refused: blank text, unknown state, unknown identifier, or a value that is not a recognized todo.
    #[error("invalid todo: {0}")]
    InvalidTodo(String),
    /// A Memory is refused: blank identifier or title, identifier already taken or unknown. The reason is a fixed phrase
    /// chosen by the caller; it never carries the body.
    #[error("invalid memory `{id}`{}: {reason}", level_note(.level))]
    InvalidMemory {
        /// Level of the Memory: user, shared or session. Empty when the refusal happens before a level is known.
        level: String,
        /// Identifier of the Memory within its level.
        id: String,
        /// Why the Memory is refused.
        reason: String,
    },
    /// The host storage of the Memory failed. The message is produced by the host.
    #[error("memory store failed: {0}")]
    Store(String),
    /// An internal lock was poisoned.
    #[error("an internal lock was poisoned")]
    Poisoned,
    /// An MCP module is refused: invalid role name, two tools of one server that map to the same function name (the reason
    /// names both MCP tool names), or an input schema that is not a usable JSON schema.
    #[error("invalid MCP module for role `{role}`: {reason}")]
    InvalidMcp {
        /// Role of the MCP server.
        role: String,
        /// Why the module is refused.
        reason: String,
    },
}

/// Result of the `modules` domain.
pub type ModulesResult<T> = Result<T, ModulesError>;

/// The ` at level X` part of a Memory refusal, empty when the level is not known.
fn level_note(level: &str) -> String {
    if level.is_empty() {
        String::new()
    } else {
        format!(" at level {level}")
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::ModulesError;

    fn assert_error_send_sync<E: Error + Send + Sync + 'static>() {}

    #[test]
    fn memory_error_names_level_and_identifier_only() {
        let error = ModulesError::InvalidMemory {
            level: "shared".to_owned(),
            id: "notes-1".to_owned(),
            reason: "unknown identifier".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "invalid memory `notes-1` at level shared: unknown identifier"
        );
    }

    #[test]
    fn memory_error_without_level_omits_it() {
        let error = ModulesError::InvalidMemory {
            level: String::new(),
            id: "a b".to_owned(),
            reason: "the identifier must not contain a blank".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "invalid memory `a b`: the identifier must not contain a blank"
        );
    }

    #[test]
    fn name_collision_error_names_role_and_both_tools() {
        let error = ModulesError::InvalidMcp {
            role: "grafana".to_owned(),
            reason: "tools `a.b` and `a_b` both map to the function `a_b`".to_owned(),
        };
        let text = error.to_string();
        assert!(text.contains("grafana"), "the role must be named: {text}");
        assert!(text.contains("`a.b`"), "the first tool must be named: {text}");
        assert!(text.contains("`a_b`"), "the second tool must be named: {text}");
    }

    #[test]
    fn poisoned_and_store_texts_are_stable() {
        assert_eq!(
            ModulesError::Poisoned.to_string(),
            "an internal lock was poisoned"
        );
        assert_eq!(
            ModulesError::Store("disk full".to_owned()).to_string(),
            "memory store failed: disk full"
        );
    }

    #[test]
    fn error_is_send_sync_and_std_error() {
        assert_error_send_sync::<ModulesError>();
    }
}
