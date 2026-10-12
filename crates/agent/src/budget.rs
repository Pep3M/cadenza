//! Spending: prefers the cost the provider reports; falls back to a per-model price table.

use serde::Serialize;

use crate::provider::Usage;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
	pub input: f64,
	pub output: f64,
}

/// Known list prices, matched by model id suffix (works for both "anthropic/claude-x" and
/// "claude-x"). Unknown models are priced at the most expensive entry, so an estimate
/// never undershoots the budget.
const PRICES: &[(&str, Price)] = &[
	(
		"claude-haiku-5.5",
		Price {
			input: 0.10,
			output: 0.50,
		},
	),
	(
		"claude-sonnet-5.5",
		Price {
			input: 2.0,
			output: 10.0,
		},
	),
	(
		"claude-opus-5",
		Price {
			input: 5.0,
			output: 25.0,
		},
	),
	(
		"claude-sonnet-5",
		Price {
			input: 2.0,
			output: 10.0,
		},
	),
	(
		"claude-haiku-4.5",
		Price {
			input: 1.0,
			output: 5.0,
		},
	),
	(
		"claude-haiku-4-5",
		Price {
			input: 1.0,
			output: 5.0,
		},
	),
	(
		"gemini-3.8-flash",
		Price {
			input: 0.75,
			output: 3.75,
		},
	),
	(
		"deepseek-v4.1-flash",
		Price {
			input: 0.30,
			output: 1.20,
		},
	),
];

const FALLBACK_PRICE: Price = Price {
	input: 5.0,
	output: 25.0,
};

pub fn price_for(model: &str) -> Price {
	let id = model.rsplit('/').next().unwrap_or(model);
	PRICES
		.iter()
		.find(|(name, _)| *name == id)
		.map(|(_, price)| *price)
		.unwrap_or(FALLBACK_PRICE)
}

/// Estimated USD cost of one response. Cache reads bill at 10% of the input price and
/// cache writes at 125% (Anthropic's 5-minute cache pricing; other providers are within
/// that envelope or report the cost themselves).
pub fn estimate_cost(model: &str, usage: &Usage) -> f64 {
	let price = price_for(model);
	let cached = usage.cache_read_tokens + usage.cache_write_tokens;
	let uncached = usage.input_tokens.saturating_sub(cached) as f64;
	let input = uncached * price.input
		+ usage.cache_read_tokens as f64 * price.input * 0.1
		+ usage.cache_write_tokens as f64 * price.input * 1.25;
	(input + usage.output_tokens as f64 * price.output) / 1e6
}

/// Running totals for a session.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Budget {
	pub max_cost_usd: f64,
	pub spent_usd: f64,
	/// Part of `spent_usd` that was estimated rather than reported.
	pub estimated_usd: f64,
	pub requests: u32,
	pub input_tokens: u64,
	pub output_tokens: u64,
	pub cache_read_tokens: u64,
	pub cache_write_tokens: u64,
}

impl Budget {
	pub fn new(max_cost_usd: f64) -> Self {
		Self {
			max_cost_usd,
			..Self::default()
		}
	}

	/// Records one response and returns `(cost, estimated)`.
	pub fn record(&mut self, model: &str, usage: &Usage) -> (f64, bool) {
		let (cost, estimated) = match usage.cost_usd {
			Some(cost) if cost.is_finite() && cost >= 0.0 => (cost, false),
			_ => (estimate_cost(model, usage), true),
		};
		self.spent_usd += cost;
		if estimated {
			self.estimated_usd += cost;
		}
		self.requests += 1;
		self.input_tokens += usage.input_tokens;
		self.output_tokens += usage.output_tokens;
		self.cache_read_tokens += usage.cache_read_tokens;
		self.cache_write_tokens += usage.cache_write_tokens;
		(cost, estimated)
	}

	pub fn fraction_used(&self) -> f64 {
		if self.max_cost_usd <= 0.0 {
			return 1.0;
		}
		self.spent_usd / self.max_cost_usd
	}

	pub fn exhausted(&self) -> bool {
		self.spent_usd >= self.max_cost_usd
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn prices_match_by_id_with_or_without_vendor_prefix() {
		assert_eq!(price_for("anthropic/claude-haiku-5.5").input, 0.10);
		assert_eq!(price_for("claude-opus-5").output, 25.0);
		assert_eq!(price_for("someone/unknown-model"), FALLBACK_PRICE);
	}

	#[test]
	fn estimates_account_for_cache_reads_and_writes() {
		let usage = Usage {
			input_tokens: 1_000_000,
			output_tokens: 100_000,
			cache_read_tokens: 800_000,
			cache_write_tokens: 100_000,
			..Usage::default()
		};
		// opus-5: 100k uncached * 5 + 800k * 0.5 + 100k * 6.25 + 100k * 25 (per MTok)
		let cost = estimate_cost("claude-opus-5", &usage);
		assert!((cost - (0.5 + 0.4 + 0.625 + 2.5)).abs() < 1e-9, "{cost}");
	}

	#[test]
	fn reported_cost_wins_over_estimates() {
		let mut budget = Budget::new(1.0);
		let (cost, estimated) = budget.record(
			"anthropic/claude-haiku-5.5",
			&Usage {
				input_tokens: 10,
				cost_usd: Some(0.6),
				..Usage::default()
			},
		);
		assert_eq!((cost, estimated), (0.6, false));
		assert!(!budget.exhausted());
		budget.record(
			"anthropic/claude-haiku-5.5",
			&Usage {
				cost_usd: Some(0.5),
				..Usage::default()
			},
		);
		assert!(budget.exhausted());
		assert!((budget.fraction_used() - 1.1).abs() < 1e-9);
	}
}
