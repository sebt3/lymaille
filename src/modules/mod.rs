//! Working modules of a session: the todo list and its Rhai module (`agent-sessions/modules`). This domain holds types and
//! pure logic; the storage of Memory and the MCP transport belong to the host or to later lots.

mod error;
mod memory;
mod memory_module;
mod todo;

pub use error::{ModulesError, ModulesResult};
pub use memory::{InMemoryMemoryStore, Memory, MemoryLevel, MemoryScope, MemoryStore, MemorySummary};
pub use memory_module::{MemoryScopes, memory_module, memory_module_name};
pub use todo::{TodoItem, TodoList, TodoStatus, todo_module};
