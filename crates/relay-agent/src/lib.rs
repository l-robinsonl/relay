//! Relay's assistant: an agent loop, its tools, and the model providers
//! behind it.
//!
//! Three decisions shape this crate, and they are worth stating because
//! each had a plausible alternative.
//!
//! **The loop runs in Rust, not in the webview.** The obvious route was
//! an agent framework in TypeScript beside the interface. That would put
//! the user's API key in the webview, where Relay has never put a
//! credential: secrets live in the operating system's keyring and are
//! read for the moment they are used. It would also mean every tool call
//! crossing the IPC boundary twice, and no way to run a local MCP server,
//! which needs to spawn a process. The loop lives here instead, and the
//! interface sees a transcript.
//!
//! **Conversations are rows in the same SQLite database as everything
//! else.** Not a hosted store. A product whose argument is that your data
//! is on your disk cannot answer "where do my conversations live?" with
//! the name of someone else's cloud.
//!
//! **The model is the user's choice and the key is theirs.** Relay ships
//! no key and proxies nothing: requests go from the desktop application
//! straight to whichever provider the user configured.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[path = "loop_.rs"]
pub mod agent_loop;
pub mod error;
pub mod mcp;
pub mod provider;
pub mod tool;

pub use agent_loop::{Agent, Approval, ApproveEverything, Approver, Progress, Silent, Turn};
pub use error::{AgentError, AgentResult};
pub use mcp::{McpConnection, McpServer};
pub use provider::{
    AnthropicProvider, CompletionRequest, CompletionResponse, Message, ModelProvider,
    OpenAiProvider, Role, StopReason,
};
pub use tool::{Tool, ToolCall, ToolOutcome, ToolRegistry, ToolResult, ToolRisk};
