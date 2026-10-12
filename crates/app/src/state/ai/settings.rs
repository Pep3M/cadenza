//! Non-secret AI settings, persisted with the user's preferences (eframe storage). API
//! keys never live here: see [`super::secrets`].

use cadenza_agent::budget::Price;
use cadenza_agent::config::{
	DEFAULT_MODEL, MODEL_PRESETS, OPENAI_KEY_ENV, OPENROUTER_KEY_ENV, ProviderConfig,
};
use cadenza_agent::openrouter::OPENAI_BASE_URL;
use cadenza_agent::session::DEFAULT_SESSION_MAX_COST_USD;
use cadenza_agent::{OpenRouterConfig, SessionConfig};
use serde::{Deserialize, Serialize};

/// Which backend answers the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProviderKind {
	#[default]
	OpenRouter,
	/// Any OpenAI-compatible endpoint: OpenAI, Ollama, LM Studio, vLLM...
	OpenAiCompatible,
}

impl ProviderKind {
	pub const ALL: [ProviderKind; 2] = [ProviderKind::OpenRouter, ProviderKind::OpenAiCompatible];

	pub fn name(self) -> &'static str {
		match self {
			ProviderKind::OpenRouter => "OpenRouter",
			ProviderKind::OpenAiCompatible => "OpenAI-compatible",
		}
	}

	/// Environment variable (or `.env` entry) checked first for the key.
	pub fn key_env(self) -> &'static str {
		match self {
			ProviderKind::OpenRouter => OPENROUTER_KEY_ENV,
			ProviderKind::OpenAiCompatible => OPENAI_KEY_ENV,
		}
	}

	/// Account name of the key in the secret store.
	pub fn account(self) -> &'static str {
		match self {
			ProviderKind::OpenRouter => "openrouter-api-key",
			ProviderKind::OpenAiCompatible => "openai-compatible-api-key",
		}
	}

	/// Whether requests fail without a key (local OpenAI-compatible servers need none).
	pub fn needs_key(self) -> bool {
		self == ProviderKind::OpenRouter
	}
}

/// Reasoning effort requested from the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effort {
	Off,
	Low,
	Medium,
	High,
}

impl Effort {
	pub const ALL: [Effort; 4] = [Effort::Off, Effort::Low, Effort::Medium, Effort::High];

	pub fn name(self) -> &'static str {
		match self {
			Effort::Off => "off",
			Effort::Low => "low",
			Effort::Medium => "medium",
			Effort::High => "high",
		}
	}

	fn wire(self) -> Option<String> {
		(self != Effort::Off).then(|| self.name().to_owned())
	}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
	pub provider: ProviderKind,
	/// OpenRouter model id (presets: `cadenza_agent::config::MODEL_PRESETS`).
	pub model: String,
	/// Endpoint of the OpenAI-compatible provider (up to `/v1`).
	pub base_url: String,
	/// Model id at the OpenAI-compatible endpoint.
	pub compatible_model: String,
	pub reasoning: Effort,
	/// Spending cap per request, in USD.
	pub max_cost_usd: f64,
	/// USD per million input/output tokens for cost estimates when the endpoint reports
	/// no cost (`None`: Cadenza's price table, unknown models priced high).
	pub custom_price: Option<(f64, f64)>,
	/// Let the AI use installed third-party plugins.
	pub allow_plugins: bool,
}

impl Default for AiSettings {
	fn default() -> Self {
		Self {
			provider: ProviderKind::OpenRouter,
			model: DEFAULT_MODEL.into(),
			base_url: OPENAI_BASE_URL.into(),
			compatible_model: "gpt-5-mini".into(),
			reasoning: Effort::Medium,
			max_cost_usd: DEFAULT_SESSION_MAX_COST_USD,
			custom_price: None,
			allow_plugins: false,
		}
	}
}

impl AiSettings {
	/// Model presets offered next to the free-text model field.
	pub fn presets(&self) -> &'static [(&'static str, &'static str)] {
		match self.provider {
			ProviderKind::OpenRouter => MODEL_PRESETS,
			ProviderKind::OpenAiCompatible => &[],
		}
	}

	/// The model id in use.
	pub fn current_model(&self) -> &str {
		match self.provider {
			ProviderKind::OpenRouter => &self.model,
			ProviderKind::OpenAiCompatible => &self.compatible_model,
		}
	}

	/// Values clamped to something usable (old or hand-edited prefs).
	pub fn sanitized(mut self) -> Self {
		if !self.max_cost_usd.is_finite() || self.max_cost_usd <= 0.0 {
			self.max_cost_usd = DEFAULT_SESSION_MAX_COST_USD;
		}
		self.max_cost_usd = self.max_cost_usd.min(50.0);
		if self.model.trim().is_empty() {
			self.model = DEFAULT_MODEL.into();
		}
		if self.base_url.trim().is_empty() {
			self.base_url = OPENAI_BASE_URL.into();
		}
		self
	}

	/// The provider settings with `api_key`.
	pub fn provider_config(&self, api_key: Option<String>) -> ProviderConfig {
		match self.provider {
			ProviderKind::OpenRouter => {
				let mut c = OpenRouterConfig::new(self.model.trim(), api_key);
				c.reasoning_effort = self.reasoning.wire();
				ProviderConfig::OpenRouter(c)
			}
			ProviderKind::OpenAiCompatible => {
				let mut c = OpenRouterConfig::openai_compatible(
					self.base_url.trim(),
					self.compatible_model.trim(),
					api_key,
				);
				c.reasoning_effort = self.reasoning.wire();
				ProviderConfig::OpenAiCompatible(c)
			}
		}
	}

	/// The agent's session settings for these choices.
	pub fn session_config(
		&self,
		api_key: Option<String>,
		transcript: std::path::PathBuf,
	) -> SessionConfig {
		let mut config = SessionConfig::new(self.provider_config(api_key), transcript);
		config.max_cost_usd = self.max_cost_usd;
		config.allow_plugins = self.allow_plugins;
		config.price = self
			.custom_price
			.map(|(input, output)| Price { input, output });
		config
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn settings_round_trip_and_map_to_providers() {
		let mut s = AiSettings {
			provider: ProviderKind::OpenAiCompatible,
			base_url: "http://localhost:11434/v1".into(),
			compatible_model: "llama3".into(),
			reasoning: Effort::Off,
			custom_price: Some((0.0, 0.0)),
			..AiSettings::default()
		};
		let back: AiSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
		assert_eq!(back, s);
		let config = s.session_config(None, "/tmp/t.jsonl".into());
		let ProviderConfig::OpenAiCompatible(c) = &config.provider else {
			panic!("wrong provider")
		};
		assert_eq!(c.base_url, "http://localhost:11434/v1");
		assert_eq!(c.reasoning_effort, None);
		assert_eq!(config.price.unwrap().input, 0.0);
		s.provider = ProviderKind::OpenRouter;
		assert_eq!(s.current_model(), DEFAULT_MODEL);
		assert!(!s.presets().is_empty());
		// Old prefs without the field, nonsense values.
		let partial: AiSettings = serde_json::from_str("{\"max_cost_usd\": -3}").unwrap();
		assert_eq!(
			partial.sanitized().max_cost_usd,
			DEFAULT_SESSION_MAX_COST_USD
		);
	}
}
