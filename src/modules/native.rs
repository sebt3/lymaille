//! Table of the native modules of a session: what the nature of the session gives, with no list in the agent definition
//! (`agent-sessions#BR-2`, `agent-sessions#DEC-16`, `agent-sessions/modules#FR-4`, `agent-sessions/toolsets#FR-8`, `FR-19`).
//! The table is the only source of the native modules: no agent definition, no context and no toolset adds or removes one.
//! The host reads it to assemble the session engine; this module performs no I/O.

use crate::agents::AgentKind;

/// Access of a session to the user level of Memory: every session reads it, the configuration agent alone writes it
/// (`agent-sessions/modules#FR-4`).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MemoryAccess {
    /// The level is read only: the module of the level offers `list`, `read` and the search, and no writing function.
    Read,
    /// The level is fully open: the module of the level offers every function, as before.
    ReadWrite,
}

/// A native module of a session, named by the Rhai name the host registers it under (`@NativeModule::name`).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum NativeModule {
    /// The todo list of the session.
    Todo,
    /// The session level of Memory.
    MemorySession,
    /// The shared level of Memory, given only when the host provides that level to the session.
    MemoryShared,
    /// The user level of Memory, in the access the nature of the session gives.
    MemoryUser(MemoryAccess),
    /// The `delegate` module: launches and tracks sub-agents. Given to a work agent only.
    Delegate,
    /// The `result` module: reports the result of a sub-agent. Given to every sub-agent, with or without a parent session.
    Result,
    /// The `session` module: never given to a sub-agent with no parent session, which has no title to set.
    Session,
    /// The toolset creation module of the configuration agent.
    ToolsetBuilder,
    /// The context modules of the configuration agent.
    Contexts,
    /// The past sessions module of the configuration agent.
    Sessions,
    /// The production inspection module of the configuration agent.
    Inspect,
}

impl NativeModule {
    /// The Rhai name the host registers this module under.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::MemorySession => "memory_session",
            Self::MemoryShared => "memory_shared",
            Self::MemoryUser(_) => "memory_user",
            Self::Delegate => "delegate",
            Self::Result => "result",
            Self::Session => "session",
            Self::ToolsetBuilder => "toolset_builder",
            Self::Contexts => "contexts",
            Self::Sessions => "sessions",
            Self::Inspect => "inspect",
        }
    }
}

/// Lists the native modules of a session, in order: the nature of the session alone decides.
///
/// `has_parent` says whether the session has a parent session (from `SessionState::parent`); `has_shared_memory` says
/// whether the host provides a shared Memory level to this session.
#[must_use]
pub fn native_modules(kind: AgentKind, has_parent: bool, has_shared_memory: bool) -> Vec<NativeModule> {
    let mut modules = vec![NativeModule::Todo, NativeModule::MemorySession];
    match kind {
        AgentKind::Work | AgentKind::Sub => {
            if has_shared_memory {
                modules.push(NativeModule::MemoryShared);
            }
            modules.push(NativeModule::MemoryUser(MemoryAccess::Read));
        }
        AgentKind::Config => modules.push(NativeModule::MemoryUser(MemoryAccess::ReadWrite)),
    }
    match kind {
        AgentKind::Work => modules.extend([NativeModule::Delegate, NativeModule::Session]),
        AgentKind::Sub => {
            modules.push(NativeModule::Result);
            if has_parent {
                modules.push(NativeModule::Session);
            }
        }
        AgentKind::Config => modules.extend([
            NativeModule::Session,
            NativeModule::ToolsetBuilder,
            NativeModule::Contexts,
            NativeModule::Sessions,
            NativeModule::Inspect,
        ]),
    }
    modules
}

#[cfg(test)]
mod tests {
    use super::{MemoryAccess, NativeModule, native_modules};
    use crate::agents::AgentKind;

    /// Scenario: un agent de travail
    #[test]
    fn work_agent_lists_exactly_the_work_modules() {
        let modules = native_modules(AgentKind::Work, false, true);
        assert_eq!(
            modules,
            vec![
                NativeModule::Todo,
                NativeModule::MemorySession,
                NativeModule::MemoryShared,
                NativeModule::MemoryUser(MemoryAccess::Read),
                NativeModule::Delegate,
                NativeModule::Session,
            ],
            "a work agent gets the work modules and no configuration module"
        );
        assert!(
            !modules.contains(&NativeModule::Result),
            "the work agent launches sub-agents through Delegate and never reports through Result: {modules:?}"
        );
    }

    /// Scenario: un sous-agent rattaché à un humain
    #[test]
    fn sub_agent_with_a_parent_session_gets_the_session_module_and_read_access() {
        assert_eq!(
            native_modules(AgentKind::Sub, true, false),
            vec![
                NativeModule::Todo,
                NativeModule::MemorySession,
                NativeModule::MemoryUser(MemoryAccess::Read),
                NativeModule::Result,
                NativeModule::Session,
            ],
            "a sub-agent with a parent session gets Session, Result and MemoryUser(Read), never Delegate nor a configuration module"
        );
    }

    /// Scenario: un sous-agent de processus automatisé
    #[test]
    fn sub_agent_without_a_parent_session_gets_no_session_module() {
        let modules = native_modules(AgentKind::Sub, false, false);
        assert!(
            !modules.contains(&NativeModule::Session),
            "a sub-agent of an automated process has no title to set: {modules:?}"
        );
        assert_eq!(
            modules,
            vec![
                NativeModule::Todo,
                NativeModule::MemorySession,
                NativeModule::MemoryUser(MemoryAccess::Read),
                NativeModule::Result,
            ],
            "Result is given with or without a parent session, and nothing else"
        );
    }

    /// Scenario: un agent de configuration
    #[test]
    fn config_agent_lists_exactly_the_configuration_modules() {
        assert_eq!(
            native_modules(AgentKind::Config, false, false),
            vec![
                NativeModule::Todo,
                NativeModule::MemorySession,
                NativeModule::MemoryUser(MemoryAccess::ReadWrite),
                NativeModule::Session,
                NativeModule::ToolsetBuilder,
                NativeModule::Contexts,
                NativeModule::Sessions,
                NativeModule::Inspect,
            ],
            "a configuration agent gets no MemoryShared"
        );
        let config = native_modules(AgentKind::Config, false, false);
        assert!(
            !config.contains(&NativeModule::Delegate) && !config.contains(&NativeModule::Result),
            "a configuration agent neither delegates nor reports: {config:?}"
        );
        let with_shared_level = native_modules(AgentKind::Config, false, true);
        assert!(
            !with_shared_level.contains(&NativeModule::MemoryShared),
            "the shared level is never given to a configuration agent: {with_shared_level:?}"
        );
    }

    /// Scenario: sans niveau partagé, pas de Memory partagée
    #[test]
    fn no_shared_level_gives_no_shared_memory_module() {
        let modules = native_modules(AgentKind::Work, false, false);
        assert_eq!(
            modules,
            vec![
                NativeModule::Todo,
                NativeModule::MemorySession,
                NativeModule::MemoryUser(MemoryAccess::Read),
                NativeModule::Delegate,
                NativeModule::Session,
            ],
            "MemoryShared is absent when the host provides no shared level"
        );
    }

    /// `Returns`: each native module names the Rhai module the host registers.
    #[test]
    fn every_module_names_its_rhai_module() {
        let named = native_modules(AgentKind::Config, false, false)
            .into_iter()
            .chain(native_modules(AgentKind::Work, false, true))
            .chain(native_modules(AgentKind::Sub, true, true))
            .map(|module| (module.name(), module))
            .collect::<Vec<_>>();
        for (name, module) in named {
            let expected = match module {
                NativeModule::Todo => "todo",
                NativeModule::MemorySession => "memory_session",
                NativeModule::MemoryShared => "memory_shared",
                NativeModule::MemoryUser(_) => "memory_user",
                NativeModule::Delegate => "delegate",
                NativeModule::Result => "result",
                NativeModule::Session => "session",
                NativeModule::ToolsetBuilder => "toolset_builder",
                NativeModule::Contexts => "contexts",
                NativeModule::Sessions => "sessions",
                NativeModule::Inspect => "inspect",
            };
            assert_eq!(name, expected, "the Rhai name of {module:?}");
        }
    }
}
