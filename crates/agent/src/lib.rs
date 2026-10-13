//! Cadenza's embedded AI producer: given a creative brief, an LLM plans, writes, arranges,
//! mixes and renders a song by calling the same tools the MCP server exposes, in a loop.
//!
//! The loop ([`agent`]) is provider-agnostic: it speaks the neutral conversation model in
//! [`types`] to any [`provider::LlmProvider`]. The first backend is OpenRouter
//! ([`openrouter`], OpenAI-compatible Chat Completions). Tools are dispatched in-process
//! through [`cadenza_mcp::CadenzaServer::call_tool_json`] behind a file-system policy
//! ([`tools`]), spending is capped by [`budget`], and every session leaves a JSONL audit
//! trail ([`transcript`]).
//!
//! Two modes: [`Agent`] composes a song from a brief into a folder (`cadenza compose`);
//! [`SessionAgent`] is the in-app producer, a conversation that edits the project the
//! user has open through a shared session, one undo step per request ([`session`]).

pub mod agent;
pub mod budget;
pub mod cancel;
pub mod config;
pub mod error;
pub mod http;
pub mod observer;
pub mod openrouter;
pub mod prompt;
pub mod provider;
pub mod scripted;
pub mod session;
pub mod tools;
pub mod transcript;
pub mod types;

pub use agent::{Agent, AgentOutcome, StopStatus};
pub use cancel::CancelToken;
pub use config::{AgentConfig, ProviderConfig};
pub use error::AgentError;
pub use observer::{AgentEvent, AgentObserver};
pub use openrouter::{OpenRouterConfig, OpenRouterProvider};
pub use prompt::{AppContext, EngineerBrief};
pub use provider::LlmProvider;
pub use session::{
	ENGINEER_UNDO_LABEL, RequestOutcome, RequestStatus, SessionAgent, SessionConfig,
};
