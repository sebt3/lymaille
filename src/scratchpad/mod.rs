//! Rhai scratchpad: the sandboxed evaluation of the LLM's scripts, their limits, the description of what the definition grants,
//! and the bounded rendering and formatting of the results. This lot holds the pure parts; the engine, persistence,
//! introspection and the `rhai_pad` tool come with the next ones.

mod error;
mod format;
mod grant;
mod limits;
mod render;

pub use error::{ErrorClass, LimitKind, NotFoundKind, ScratchpadError, ScratchpadResult, ScriptPosition};
pub use format::register_format_functions;
pub use grant::{CoreGroup, GrantedModule, GrantedSkill, GrantedToolset, Grants};
pub use limits::ScratchpadLimits;
pub use render::{Rendered, full_text, render_value, truncate_middle};
