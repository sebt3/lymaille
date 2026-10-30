//! Rhai scratchpad: the sandboxed evaluation of the LLM's scripts, their limits, the description of what the definition grants,
//! and the bounded rendering and formatting of the results. This lot holds the pure parts; the engine, persistence,
//! introspection and the `rhai_pad` tool come with the next ones.

mod engine;
mod error;
mod format;
mod grant;
mod inspect;
mod introspect;
mod limits;
mod persist;
mod render;
mod run;
mod suggest;

pub use engine::{EngineBuilder, ScratchpadEngine};
pub use error::{
    LimitKind, NotFoundKind, ScratchpadError, ScratchpadResult, ScriptErrorClass, ScriptPosition,
};
pub use format::register_format_functions;
pub use grant::{CoreGroup, GrantedModule, GrantedSkill, GrantedToolset, Grants};
pub use inspect::{
    FunctionDescriptor, InspectionPad, InspectionTarget, ModuleDescriptor, descriptors_from_catalog,
    shell_module,
};
pub use introspect::{
    Catalog, FunctionEntry, IntrospectionSource, Origin, ScriptFunction, VariableInfo, limits_notice,
};
pub use limits::ScratchpadLimits;
pub use persist::{
    FunctionSource, PersistWarning, Recompiled, RestoreWarning, ScopeSnapshot, UnpersistableReason,
    extract_function_sources, recompile_functions, restore_scope, snapshot_scope, to_dynamic, to_tagged,
};
pub use render::{Rendered, full_text, render_value, truncate_middle};
pub use run::{PadSnapshot, Restored, RunEnv, RunOutcome, RunStatus, Scratchpad};
pub use suggest::{CandidateSet, nearest, suggest_for};

/// The Rhai engine every session engine and every toolset module is built from (see `engine::base_engine`), for the
/// `toolsets` domain, which evaluates the modules of its toolsets on it.
#[expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt rewrites pub(in crate) into pub(crate), which this lint refuses: both are the crate visibility that unreachable_pub requires"
)]
pub(crate) fn base_engine(limits: &ScratchpadLimits) -> rhai::Engine {
    engine::base_engine(limits)
}
