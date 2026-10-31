//! Runtime: the assembly that runs a session. The turn loop calls the model, runs the `rhai_pad` tool in the scratchpad of
//! the session, compacts the context window and persists the state at the end of each turn. It performs no I/O of its own:
//! storage, inference and modules come from traits and objects the host provides.

mod definition;
mod turn;

pub use definition::rhai_pad_definition;
pub use turn::{RHAI_PAD_TOOL, TurnConfig, TurnOutput, TurnRunner, UserInbox};
