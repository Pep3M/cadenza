//! Session configuration, model presets and credential lookup (environment, then `.env`).

use std::path::{Path, PathBuf};

use crate::openrouter::OpenRouterConfig;

/// The model used when none is given. One constant so the default is easy to change.
pub const DEFAULT_MODEL: &str = "anthropic/claude-haiku-5.5";

/// Suggested models (OpenRouter ids) with a one-line note, shown in the CLI help.
pub const MODEL_PRESETS: &[(&str, &str)] = &[
	(
		"anthropic/claude-haiku-5.5",
		"default: fast and cheap ($0.10 in / $0.50 out per MTok)",
	),
	(
		"anthropic/claude-sonnet-5.5",
		"quality tier ($2 / $10 per MTok)",
	),
	(
		"google/gemini-3.8-flash",
		"budget alternative ($0.75 / $3.75 per MTok)",
	),
	(
		"deepseek/deepseek-v4.1-flash",
		"budget alternative ($0.30 / $1.20 per MTok)",
	),
];

pub const DEFAULT_REASONING_EFFORT: &str = "medium";
pub const DEFAULT_MAX_COST_USD: f64 = 1.0;
pub const DEFAULT_MAX_TURNS: u32 = 150;
pub const DEFAULT_TARGET_LUFS: f32 = -14.0;
/// Environment variable holding the OpenRouter API key.
pub const OPENROUTER_KEY_ENV: &str = "OPENROUTER_API_KEY";
/// Environment variable holding the key of the generic OpenAI-compatible provider.
pub const OPENAI_KEY_ENV: &str = "OPENAI_API_KEY";

/// Which LLM backend drives the session, with its settings.
#[derive(Debug, Clone)]
pub enum ProviderConfig {
	OpenRouter(OpenRouterConfig),
	/// Any OpenAI-compatible Chat Completions endpoint (OpenAI, Ollama, LM Studio...);
	/// same wire code as OpenRouter, without its extensions (see [`crate::openrouter`]).
	OpenAiCompatible(OpenRouterConfig),
}

impl ProviderConfig {
	/// OpenRouter with `model`, its key read from `key_env` (environment first, then the
	/// nearest `.env` above the current directory).
	pub fn openrouter_from_env(model: impl Into<String>, key_env: &str) -> Self {
		let cwd = std::env::current_dir().unwrap_or_default();
		Self::OpenRouter(OpenRouterConfig::new(model, resolve_secret(key_env, &cwd)))
	}

	pub fn model(&self) -> &str {
		&self.chat_config().model
	}

	/// The Chat Completions settings behind either provider.
	pub fn chat_config(&self) -> &OpenRouterConfig {
		match self {
			Self::OpenRouter(c) | Self::OpenAiCompatible(c) => c,
		}
	}

	pub fn chat_config_mut(&mut self) -> &mut OpenRouterConfig {
		match self {
			Self::OpenRouter(c) | Self::OpenAiCompatible(c) => c,
		}
	}

	/// Creates the provider.
	pub fn build(
		&self,
	) -> Result<Box<dyn crate::provider::LlmProvider>, crate::provider::ProviderError> {
		let mut config = self.chat_config().clone();
		config.flavor = match self {
			Self::OpenRouter(_) => crate::openrouter::ApiFlavor::OpenRouter,
			Self::OpenAiCompatible(_) => crate::openrouter::ApiFlavor::OpenAiCompatible,
		};
		Ok(Box::new(crate::openrouter::OpenRouterProvider::new(
			config,
		)?))
	}
}

/// Everything that shapes one composition session.
#[derive(Debug, Clone)]
pub struct AgentConfig {
	pub provider: ProviderConfig,
	/// Hard cap on model requests.
	pub max_turns: u32,
	/// Spending cap in USD; the session wraps up near it and stops once it is reached.
	pub max_cost_usd: f64,
	/// Integrated loudness the mix should reach.
	pub target_lufs: f32,
	/// Optional style hint ("lo-fi", "synthwave"...), passed to the model.
	pub genre: Option<String>,
	/// Every file the session reads or writes lives here.
	pub out_dir: PathBuf,
	/// Base name of the project/render files.
	pub slug: String,
	/// Whether third-party CLAP/VST3 plugins may be scanned and used.
	pub allow_plugins: bool,
	/// Tool results longer than this many characters are truncated (with a note).
	pub max_tool_result_chars: usize,
	/// When a request's input reaches this many tokens, the conversation is condensed into
	/// a hand-off note at the next safe point. `None` disables it.
	pub compact_at_input_tokens: Option<u64>,
}

impl AgentConfig {
	/// A configuration with default limits for `provider`.
	pub fn new(
		provider: ProviderConfig,
		out_dir: impl Into<PathBuf>,
		slug: impl Into<String>,
	) -> Self {
		Self {
			provider,
			max_turns: DEFAULT_MAX_TURNS,
			max_cost_usd: DEFAULT_MAX_COST_USD,
			target_lufs: DEFAULT_TARGET_LUFS,
			genre: None,
			out_dir: out_dir.into(),
			slug: slug.into(),
			allow_plugins: false,
			max_tool_result_chars: 16_000,
			compact_at_input_tokens: Some(200_000),
		}
	}

	pub fn project_file_name(&self) -> String {
		format!("{}.cdz", self.slug)
	}

	pub fn wav_file_name(&self) -> String {
		format!("{}.wav", self.slug)
	}

	pub fn project_path(&self) -> PathBuf {
		self.out_dir.join(self.project_file_name())
	}

	pub fn wav_path(&self) -> PathBuf {
		self.out_dir.join(self.wav_file_name())
	}

	pub fn transcript_path(&self) -> PathBuf {
		self.out_dir.join("session.jsonl")
	}

	pub fn model(&self) -> &str {
		self.provider.model()
	}
}

/// A file-name-safe slug from the first words of a brief ("lo-fi ballad, sad" ->
/// "lo-fi-ballad-sad").
pub fn slugify(brief: &str) -> String {
	let mut slug = String::new();
	for word in brief
		.split(|c: char| !(c.is_alphanumeric() || c == '-'))
		.filter(|w| !w.is_empty())
	{
		let word: String = word
			.chars()
			.filter(|c| c.is_ascii_alphanumeric() || *c == '-')
			.collect::<String>()
			.to_ascii_lowercase();
		let word = word.trim_matches('-');
		if word.is_empty() {
			continue;
		}
		if !slug.is_empty() && slug.len() + word.len() + 1 > 40 {
			break;
		}
		if !slug.is_empty() {
			slug.push('-');
		}
		slug.push_str(word);
		if slug.len() >= 40 {
			slug.truncate(40);
			break;
		}
	}
	if slug.is_empty() { "song".into() } else { slug }
}

/// Looks a secret up in the process environment first, then in the nearest `.env` file
/// found walking up from `start_dir` (at most five levels). Empty values count as unset.
/// The value is never logged.
pub fn resolve_secret(name: &str, start_dir: &Path) -> Option<String> {
	if let Ok(value) = std::env::var(name)
		&& !value.trim().is_empty()
	{
		return Some(value.trim().to_owned());
	}
	let env_file = find_dotenv(start_dir)?;
	let content = std::fs::read_to_string(env_file).ok()?;
	parse_dotenv(&content)
		.into_iter()
		.find(|(key, value)| key == name && !value.is_empty())
		.map(|(_, value)| value)
}

/// The nearest `.env` at or above `start_dir` (up to five parent levels).
pub fn find_dotenv(start_dir: &Path) -> Option<PathBuf> {
	start_dir
		.ancestors()
		.take(6)
		.map(|dir| dir.join(".env"))
		.find(|candidate| candidate.is_file())
}

/// Minimal `.env` parser: `KEY=VALUE` lines, optional `export ` prefix, `#` comments and
/// blank lines ignored, matching single or double quotes stripped.
pub fn parse_dotenv(content: &str) -> Vec<(String, String)> {
	content
		.lines()
		.filter_map(|line| {
			let line = line.trim();
			if line.is_empty() || line.starts_with('#') {
				return None;
			}
			let line = line.strip_prefix("export ").unwrap_or(line);
			let (key, value) = line.split_once('=')?;
			let key = key.trim();
			if key.is_empty() {
				return None;
			}
			let mut value = value.trim();
			let quoted = value.len() >= 2
				&& ((value.starts_with('"') && value.ends_with('"'))
					|| (value.starts_with('\'') && value.ends_with('\'')));
			if quoted {
				value = &value[1..value.len() - 1];
			} else if let Some((before, _comment)) = value.split_once(" #") {
				value = before.trim_end();
			}
			Some((key.to_owned(), value.to_owned()))
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slugs_are_short_and_file_safe() {
		assert_eq!(
			slugify("lo-fi ballad, 3 minutes, sad, piano and strings"),
			"lo-fi-ballad-3-minutes-sad-piano-and"
		);
		assert_eq!(slugify("  ¡¡!!  "), "song");
		assert!(slugify(&"word ".repeat(40)).len() <= 40);
	}

	#[test]
	fn dotenv_parsing_handles_comments_quotes_and_export() {
		let parsed = parse_dotenv(
			"# comment\n\nA=1\nexport B = \"two words\"\nC='x=y'\nD=plain # trailing\n=bad\nnoequals\n",
		);
		assert_eq!(
			parsed,
			vec![
				("A".into(), "1".into()),
				("B".into(), "two words".into()),
				("C".into(), "x=y".into()),
				("D".into(), "plain".into()),
			]
		);
	}

	#[test]
	fn secrets_come_from_the_nearest_dotenv_when_not_in_the_environment() {
		let root = tempfile::tempdir().unwrap();
		let nested = root.path().join("a/b/c");
		std::fs::create_dir_all(&nested).unwrap();
		std::fs::write(
			root.path().join(".env"),
			"CADENZA_TEST_SECRET_XYZ=\"from-file\"\n",
		)
		.unwrap();
		assert_eq!(
			resolve_secret("CADENZA_TEST_SECRET_XYZ", &nested).as_deref(),
			Some("from-file")
		);
		assert_eq!(resolve_secret("CADENZA_TEST_SECRET_MISSING", &nested), None);
		// too far up: more than five levels
		let deep = root.path().join("1/2/3/4/5/6/7");
		std::fs::create_dir_all(&deep).unwrap();
		assert_eq!(resolve_secret("CADENZA_TEST_SECRET_XYZ", &deep), None);
	}
}
