//! The Agent Video Studio MCP server: the agent-facing tool surface over the same
//! core, session and render crates the editor app uses.

pub mod contract;
pub mod handoff;
pub mod protocol;
pub mod render;
pub mod server;

pub use protocol::{ToolError, ToolHost, serve};
pub use server::Server;
