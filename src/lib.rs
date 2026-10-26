//! lymaille is a library to run an LLM as a durable work agent.
//! It provides persistent session management, a Rhai scratchpad for
//! agent state, and lazily discovered MCP toolsets.
//! See `dsd/agent-sessions.md` for the full design.

mod clock;
mod session;

pub use clock::{Clock, ManualClock, SystemClock, Timestamp};
pub use session::{
    Backoff, Redactor, RunId, SessionError, SessionId, SessionResult, TurnGate, TurnId, TurnPermit,
};
