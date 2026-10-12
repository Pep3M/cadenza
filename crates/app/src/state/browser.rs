//! The device list behind the browser panel: built-in devices and scanned plugins.

use cadenza_dsp::DeviceRole;
use cadenza_plugin_host::PluginCatalog;

/// Where a device comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
	Builtin,
	Plugin,
}

/// One insertable device. `id` is the id the command API (and MCP `insert_device`) uses:
/// a built-in id (`"reverb"`) or `clap:<id>` / `vst3:<class id>`.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserItem {
	pub id: String,
	pub name: String,
	pub role: DeviceRole,
	pub source: Source,
	/// Vendor and format for plugins (`"u-he · CLAP"`), empty for built-ins.
	pub detail: String,
	pub description: String,
}

impl BrowserItem {
	pub fn matches(&self, query: &str) -> bool {
		let q = query.trim().to_lowercase();
		q.is_empty()
			|| q.split_whitespace().all(|word| {
				self.name.to_lowercase().contains(word)
					|| self.detail.to_lowercase().contains(word)
					|| self.id.to_lowercase().contains(word)
			})
	}
}

/// Every device, built-ins first, each group sorted by name.
pub fn items(catalog: Option<&PluginCatalog>) -> Vec<BrowserItem> {
	let mut builtin: Vec<BrowserItem> = cadenza_dsp::registry()
		.iter()
		.map(|d| BrowserItem {
			id: d.id.to_owned(),
			name: d.name.to_owned(),
			role: d.role,
			source: Source::Builtin,
			detail: String::new(),
			description: d.description.to_owned(),
		})
		.collect();
	builtin.sort_by(|a, b| a.name.cmp(&b.name));
	let mut plugins: Vec<BrowserItem> = catalog
		.into_iter()
		.flat_map(PluginCatalog::descriptors)
		.map(|d| BrowserItem {
			id: cadenza_mcp::convert::plugin_device_id(d),
			name: d.name.clone(),
			role: d.role.into(),
			source: Source::Plugin,
			detail: format!("{} · {}", d.vendor, d.format.as_str().to_uppercase()),
			description: d.description.clone(),
		})
		.collect();
	plugins.sort_by_key(|a| a.name.to_lowercase());
	builtin.extend(plugins);
	builtin
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn builtins_are_listed_and_searchable() {
		let all = items(None);
		assert!(
			all.iter()
				.any(|i| i.id == "poly_synth" && i.role == DeviceRole::Instrument)
		);
		assert!(
			all.iter()
				.any(|i| i.id == "reverb" && i.role == DeviceRole::Effect)
		);
		let found: Vec<_> = all.iter().filter(|i| i.matches("REV")).collect();
		assert_eq!(found.len(), 1);
		assert!(all.iter().all(|i| i.matches("  ")));
		assert!(!all.iter().any(|i| i.matches("zzz")));
	}
}
