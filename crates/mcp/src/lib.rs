//! Cadenza's AI hands: an MCP server (stdio) exposing the project command API as
//! well-designed tools for an LLM arranger/mix engineer.
//!
//! Tool logic lives in plain, testable functions in [`tools`] (each taking a
//! [`session::Session`] and a typed args struct); [`server`] is a thin `rmcp` adapter
//! over them.

pub mod convert;
pub mod error;
pub mod server;
pub mod session;
pub mod tools;

pub use error::{McpError, ServeError};
pub use server::{CadenzaServer, serve_stdio};
pub use session::Session;
