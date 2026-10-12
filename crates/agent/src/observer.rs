//! Progress events for whoever is watching a session (CLI, UI, transcript).

use crate::provider::Usage;

#[derive(Debug, Clone)]
pub enum AgentEvent<'a> {
	TurnStarted {
		turn: u32,
	},
	/// Reasoning text (or its summary) the model shared this turn.
	Thinking(&'a str),
	/// The model's visible text: plans and progress notes (the whole text of a turn).
	Text(&'a str),
	/// A fragment of the turn's text as it streams in (before [`AgentEvent::Text`]).
	TextDelta(&'a str),
	/// A fragment of the turn's reasoning as it streams in (before [`AgentEvent::Thinking`]).
	ThinkingDelta(&'a str),
	ToolCall {
		name: &'a str,
		args: &'a str,
	},
	ToolResult {
		name: &'a str,
		ok: bool,
		summary: &'a str,
	},
	Usage {
		turn: u32,
		usage: &'a Usage,
		turn_cost_usd: f64,
		total_cost_usd: f64,
		/// True when the cost is estimated from a price table rather than reported.
		estimated: bool,
	},
	Retry {
		attempt: u32,
		wait_ms: u64,
		reason: &'a str,
	},
	/// Harness-level notes: budget warnings, context checkpoints, finalization.
	Notice(&'a str),
	/// The model's closing message.
	Final(&'a str),
}

pub trait AgentObserver: Send + Sync {
	fn on_event(&self, event: &AgentEvent<'_>);
}

/// Ignores every event.
pub struct NullObserver;

impl AgentObserver for NullObserver {
	fn on_event(&self, _event: &AgentEvent<'_>) {}
}

/// Shortens `text` to at most `max` characters on a char boundary, adding an ellipsis.
pub fn clip(text: &str, max: usize) -> String {
	let flat = text.replace('\n', " ");
	if flat.chars().count() <= max {
		return flat;
	}
	let mut out: String = flat.chars().take(max.saturating_sub(3)).collect();
	out.push_str("...");
	out
}
