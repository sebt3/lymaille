//! Working modules of a session: the todo list and its Rhai module (`agent-sessions/modules`). This domain holds types and
//! pure logic; the storage of Memory and the MCP transport belong to the host or to later lots.

mod error;
mod mcp;
mod mcp_module;
mod mcp_name;
mod memory;
mod memory_module;
mod todo;

pub use error::{ModulesError, ModulesResult};
pub use mcp::{McpConnector, McpError, McpLister, McpTool, ScriptedConnector};
pub use mcp_module::{McpEnv, McpRetryPolicy, mcp_module, mcp_shell_descriptor};
pub use mcp_name::{function_name, name_tools};
pub use memory::{InMemoryMemoryStore, Memory, MemoryLevel, MemoryScope, MemoryStore, MemorySummary};
pub use memory_module::{MemoryScopes, memory_module, memory_module_name};
pub use todo::{TodoItem, TodoList, TodoStatus, todo_module};
