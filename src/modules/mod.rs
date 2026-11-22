//! Working modules of a session: the todo list and its Rhai module (`agent-sessions/modules`). This domain holds types and
//! pure logic; the storage of Memory and the MCP transport belong to the host or to later lots.

mod error;
mod mcp;
mod mcp_auth;
#[cfg(feature = "mcp-http")]
mod mcp_client;
mod mcp_module;
mod mcp_name;
mod memory;
mod memory_module;
#[expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt rewrites pub(in crate) into pub(crate), which this lint refuses: both are the crate visibility that unreachable_pub requires"
)]
pub(crate) mod native;
#[cfg(feature = "embedder")]
mod semantic;
mod session_module;
mod todo;

pub use error::{ModulesError, ModulesResult};
pub use mcp::{McpConnector, McpError, McpLister, McpNotification, McpTool, ScriptedConnector};
pub use mcp_auth::{McpAuth, McpHeaders};
#[cfg(feature = "mcp-http")]
pub use mcp_client::{McpClientConfig, RmcpConnector};
pub use mcp_module::{McpEnv, McpRetryPolicy, mcp_module, mcp_shell_descriptor};
pub use mcp_name::{function_name, name_tools};
pub use memory::{InMemoryMemoryStore, Memory, MemoryLevel, MemoryScope, MemoryStore, MemorySummary};
pub use memory_module::{MemoryScopes, memory_module, memory_module_name, memory_module_read_only};
pub use native::{MemoryAccess, NativeModule, native_modules};
#[cfg(feature = "embedder")]
pub use semantic::{SemanticHit, SemanticIndex, memory_embedding_text, skill_embedding_text};
pub use session_module::session_module;
pub use todo::{TodoItem, TodoList, TodoStatus, todo_module};
