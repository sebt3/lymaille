//! Glue of the native tools: the exact set of tool definitions a session sends to the model. See `src/tools/tools.sdd`.

mod definitions;
mod inspect;

pub use definitions::session_tool_definitions;
pub use inspect::{InspectEnv, InspectTool, InspectionHost};
