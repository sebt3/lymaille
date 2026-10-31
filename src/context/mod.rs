//! Context window: thresholds of compaction and startup, token usage, replay mode of the reasoning. Pure logic, no I/O:
//! the call to the summarizer belongs to the host or to the `agents` domain.

mod budget;
mod error;
mod replay;
mod strategy;
mod usage;

pub use budget::Budget;
pub use error::{ContextError, ContextResult};
pub use replay::{ReplayMode, replayed_reasoning};
pub use strategy::{
    CompactionStrategy, DEFAULT_SUMMARY_PROMPT, DefaultStrategy, Digest, Summarizer, summary_prompt,
};
pub use usage::{UsageTracker, estimate_tokens};
