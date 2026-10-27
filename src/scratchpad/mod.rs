//! Rhai scratchpad: the sandboxed evaluation of the LLM's scripts, their limits, the description of what the definition grants,
//! and the bounded rendering and formatting of the results. This lot holds the pure parts; the engine, persistence,
//! introspection and the `rhai_pad` tool come with the next ones.

mod error;
mod format;
mod grant;
mod introspect;
mod limits;
mod render;
mod suggest;

pub use error::{
    LimitKind, NotFoundKind, ScratchpadError, ScratchpadResult, ScriptErrorClass, ScriptPosition,
};
pub use format::register_format_functions;
pub use grant::{CoreGroup, GrantedModule, GrantedSkill, GrantedToolset, Grants};
pub use introspect::{
    Catalog, FunctionEntry, IntrospectionSource, Origin, ScriptFunction, VariableInfo, limits_notice,
};
pub use limits::ScratchpadLimits;
pub use render::{Rendered, full_text, render_value, truncate_middle};
pub use suggest::{CandidateSet, nearest, suggest_for};
