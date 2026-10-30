//! Toolsets: Rhai modules, skills, the requirements they place on a toolset context, the context types they refer to,
//! and the toolset context itself (`agent-sessions/toolsets`). This module holds types and pure logic; storage I/O
//! belongs to the host.

mod assemble;
mod compat;
mod config;
mod context_type;
mod contract;
mod error;
mod skill;
mod store;
mod template;
mod toolset;
mod toolset_context;
mod version;

pub use assemble::{Assembled, assemble};
pub use compat::{
    Delegation, Gap, GapKind, RemovalReason, RemovedSubagent, check_context, check_creation_rule,
    delegable_subagents,
};
pub use config::{
    ConfigEnv, ConfigStores, InspectionSlot, ScriptError, SessionFilter, SessionIndex, SessionSummary,
    config_registration,
};
pub use context_type::{ContextType, FlatContextType, RoleSpec};
pub use contract::{Contract, VersionWarning, check_declared_version, extract_contract};
pub use error::{ToolsetsError, ToolsetsResult};
pub use skill::Skill;
pub use store::{ContextStore, ContextTypeStore, InMemoryToolsets, ToolsetStore};
pub use template::{SessionMeta, check_template, render_prompt, render_skill};
pub use toolset::{ParamDecl, RoleDecl, Toolset};
pub use toolset_context::{ContextLevel, ResolvedContext, ToolsetContext};
pub use version::{Bump, TypeRequirement};
