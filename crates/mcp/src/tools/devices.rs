//! Device discovery: built-in instruments/effects plus scanned third-party plugins,
//! described for an AI choosing and configuring devices.
//!
//! Device ids are stable strings: built-ins use their registry id (`"poly_synth"`),
//! plugins use `"clap:<plugin id>"` or `"vst3:<class id>"`.

use std::path::PathBuf;

use cadenza_dsp::{DeviceInfo, DeviceRole};
use cadenza_plugin_host::{PluginCatalog, PluginDescriptor, PluginRole};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{DeviceRef, plugin_device_id, resolve_device};
use crate::error::McpError;
use crate::session::Session;

const DEFAULT_LIST_LIMIT: usize = 50;
const DEFAULT_PARAM_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleFilter {
	Instrument,
	Effect,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceFilter {
	/// Built-in devices only (always available).
	Builtin,
	/// Scanned third-party plugins only.
	Plugin,
	#[default]
	All,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListDevicesArgs {
	/// Only list devices of this role.
	#[serde(default)]
	pub role: Option<RoleFilter>,
	/// Case-insensitive search over name, vendor, category tags (features), description and
	/// id; several words must all match (e.g. "surge", "compressor", "lsp reverb").
	#[serde(default)]
	pub query: Option<String>,
	/// builtin | plugin | all (default all).
	#[serde(default)]
	pub source: Option<SourceFilter>,
	/// Maximum entries to return (default 50).
	#[serde(default)]
	pub limit: Option<usize>,
	/// Entries to skip, for paging (default 0).
	#[serde(default)]
	pub offset: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ParamOut {
	pub id: String,
	pub name: String,
	pub min: f32,
	pub max: f32,
	pub default: f32,
	pub unit: String,
	pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeviceOut {
	/// Stable device id to pass to insert_device / create_track.instrument.
	pub id: String,
	pub name: String,
	/// "instrument" or "effect".
	pub role: &'static str,
	/// "builtin" or "plugin".
	pub source: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub vendor: Option<String>,
	/// Plugin format: "CLAP" or "VST3".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub format: Option<&'static str>,
	/// Vendor category tags (plugins only).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub features: Vec<String>,
	/// What the device does (built-ins; plugins: use get_device_info).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
	/// Number of (automatable) parameters.
	pub param_count: usize,
	/// Processing latency in samples (compensated automatically).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub latency_samples: Option<u32>,
	/// Full parameter list (built-ins only; plugin params via get_device_info).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub params: Option<Vec<ParamOut>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListDevicesOutput {
	/// Matching devices before paging.
	pub total: usize,
	pub offset: usize,
	pub devices: Vec<DeviceOut>,
	/// Offset of the next page, if more matches remain.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub next_offset: Option<usize>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub note: Option<String>,
}

fn role_str(role: DeviceRole) -> &'static str {
	match role {
		DeviceRole::Instrument => "instrument",
		DeviceRole::Effect => "effect",
	}
}

fn terms(query: Option<&str>) -> Vec<String> {
	query
		.unwrap_or("")
		.split_whitespace()
		.map(str::to_lowercase)
		.collect()
}

fn matches_all(terms: &[String], haystack: &str) -> bool {
	let haystack = haystack.to_lowercase();
	terms.iter().all(|t| haystack.contains(t.as_str()))
}

fn builtin_out(d: &DeviceInfo) -> DeviceOut {
	DeviceOut {
		id: d.id.to_owned(),
		name: d.name.to_owned(),
		role: role_str(d.role),
		source: "builtin",
		vendor: None,
		format: None,
		features: Vec::new(),
		description: Some(d.description.to_owned()),
		param_count: d.params.len(),
		latency_samples: None,
		params: Some(
			d.params
				.iter()
				.map(|p| ParamOut {
					id: p.id.to_owned(),
					name: p.name.to_owned(),
					min: p.min,
					max: p.max,
					default: p.default,
					unit: p.unit.to_owned(),
					description: p.description.to_owned(),
				})
				.collect(),
		),
	}
}

fn plugin_out(d: &PluginDescriptor) -> DeviceOut {
	DeviceOut {
		id: plugin_device_id(d),
		name: d.name.clone(),
		role: role_str(d.role.into()),
		source: "plugin",
		vendor: Some(d.vendor.clone()).filter(|v| !v.is_empty()),
		format: Some(d.format.as_str()),
		features: d.features.clone(),
		description: None,
		param_count: d.params.iter().filter(|p| p.automatable).count(),
		latency_samples: Some(d.latency_samples),
		params: None,
	}
}

/// Searches built-ins (always available) and scanned plugins. `catalog` is only needed
/// when plugins are included.
pub fn list_devices(catalog: Option<&PluginCatalog>, args: ListDevicesArgs) -> ListDevicesOutput {
	let source = args.source.unwrap_or_default();
	let terms = terms(args.query.as_deref());
	let role = args.role.map(|r| match r {
		RoleFilter::Instrument => DeviceRole::Instrument,
		RoleFilter::Effect => DeviceRole::Effect,
	});
	let mut builtins: Vec<&DeviceInfo> = Vec::new();
	if source != SourceFilter::Plugin {
		builtins = cadenza_dsp::registry()
			.iter()
			.copied()
			.filter(|d| role.is_none_or(|r| d.role == r))
			.filter(|d| matches_all(&terms, &format!("{} {} {}", d.id, d.name, d.description)))
			.collect();
	}
	let mut plugins: Vec<&PluginDescriptor> = Vec::new();
	let catalog_size = catalog.map_or(0, |c| c.descriptors().count());
	if source != SourceFilter::Builtin
		&& let Some(catalog) = catalog
	{
		plugins = catalog
			.descriptors()
			.filter(|d| role.is_none_or(|r| DeviceRole::from(d.role) == r))
			.filter(|d| {
				matches_all(
					&terms,
					&format!(
						"{} {} {} {} {}",
						plugin_device_id(d),
						d.name,
						d.vendor,
						d.features.join(" "),
						d.description
					),
				)
			})
			.collect();
		plugins.sort_by_cached_key(|d| (d.name.to_lowercase(), d.format.as_str(), d.id.clone()));
	}

	let total = builtins.len() + plugins.len();
	let offset = args.offset.unwrap_or(0);
	let limit = args.limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIMIT);
	let devices: Vec<DeviceOut> = builtins
		.into_iter()
		.map(builtin_out)
		.chain(plugins.into_iter().map(plugin_out))
		.skip(offset)
		.take(limit)
		.collect();
	let next_offset = (offset + devices.len() < total).then_some(offset + devices.len());
	let note = if source != SourceFilter::Builtin && catalog_size == 0 {
		Some(
			"No third-party plugins in the catalog yet; call scan_plugins once to find installed \
			 plugins. Built-in devices are always available."
				.to_owned(),
		)
	} else if total == 0 {
		Some("Nothing matched; try a shorter query or another role/source.".to_owned())
	} else {
		None
	};
	ListDevicesOutput {
		total,
		offset,
		devices,
		next_offset,
		note,
	}
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDeviceInfoArgs {
	/// Device id from list_devices: built-in (`"reverb"`) or plugin (`"clap:<plugin id>"`).
	pub device: String,
	/// Case-insensitive filter on parameter name/module (words must all match), e.g.
	/// "cutoff", "filter 1", "osc 2 pitch".
	#[serde(default)]
	pub param_query: Option<String>,
	/// Maximum parameters to return (default 100).
	#[serde(default)]
	pub limit: Option<usize>,
	/// Parameters to skip, for paging (default 0).
	#[serde(default)]
	pub offset: Option<usize>,
	/// Also list parameters the plugin marks as not automatable (e.g. the 2048 "MIDI CC"
	/// pseudo-params of some VST3 plugins). Default false.
	#[serde(default)]
	pub include_non_automatable: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ParamDetail {
	/// Use this id (or the exact name) in params / set_device_params / automation addresses.
	pub id: String,
	pub name: String,
	/// Group path inside a plugin (e.g. "Scene A/Filter 1"); empty for built-ins.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub module: String,
	pub min: f64,
	pub max: f64,
	pub default: f64,
	/// Only integer values are meaningful (switches, modes).
	pub stepped: bool,
	/// False if the plugin forbids automating it (it can still be set statically).
	pub automatable: bool,
	/// The plugin's own bypass switch; prefer set_bypass on the device instead.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub bypass: bool,
	#[serde(skip_serializing_if = "String::is_empty")]
	pub unit: String,
	#[serde(skip_serializing_if = "String::is_empty")]
	pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeviceInfoOutput {
	pub id: String,
	pub name: String,
	pub role: &'static str,
	pub source: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub vendor: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub format: Option<&'static str>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub version: Option<String>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub features: Vec<String>,
	pub description: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub latency_samples: Option<u32>,
	/// Parameters of the device (automatable ones only, unless include_non_automatable).
	pub param_count: usize,
	/// Parameters matching param_query (before paging).
	pub matched: usize,
	pub offset: usize,
	pub params: Vec<ParamDetail>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub next_offset: Option<usize>,
}

/// Full description of one device with a filtered, paginated parameter list.
pub fn get_device_info(
	catalog: Option<&PluginCatalog>,
	args: GetDeviceInfoArgs,
) -> Result<DeviceInfoOutput, McpError> {
	let device = resolve_any_role(&args.device, catalog)?;
	let terms = terms(args.param_query.as_deref());
	let all: Vec<ParamDetail> = match device {
		DeviceRef::Builtin(info) => info
			.params
			.iter()
			.map(|p| ParamDetail {
				id: p.id.to_owned(),
				name: p.name.to_owned(),
				module: String::new(),
				min: p.min.into(),
				max: p.max.into(),
				default: p.default.into(),
				stepped: false,
				automatable: true,
				bypass: false,
				unit: p.unit.to_owned(),
				description: p.description.to_owned(),
			})
			.collect(),
		DeviceRef::Plugin(desc) => desc
			.params
			.iter()
			.filter(|p| p.automatable || args.include_non_automatable)
			.map(|p| ParamDetail {
				id: p.id.clone(),
				name: p.name.clone(),
				module: crate::convert::module_path(p).to_owned(),
				min: p.min,
				max: p.max,
				default: p.default,
				stepped: p.stepped,
				automatable: p.automatable,
				bypass: p.bypass,
				unit: String::new(),
				description: String::new(),
			})
			.collect(),
	};
	let param_count = all.len();
	let matching: Vec<ParamDetail> = all
		.into_iter()
		.filter(|p| {
			terms.is_empty()
				|| terms.iter().all(|t| *t == p.id)
				|| matches_all(&terms, &format!("{}/{} {}", p.module, p.name, p.id))
		})
		.collect();
	let matched = matching.len();
	let offset = args.offset.unwrap_or(0);
	let limit = args
		.limit
		.unwrap_or(DEFAULT_PARAM_LIMIT)
		.clamp(1, MAX_LIMIT);
	let params: Vec<ParamDetail> = matching.into_iter().skip(offset).take(limit).collect();
	let next_offset = (offset + params.len() < matched).then_some(offset + params.len());
	Ok(match device {
		DeviceRef::Builtin(info) => DeviceInfoOutput {
			id: info.id.to_owned(),
			name: info.name.to_owned(),
			role: role_str(info.role),
			source: "builtin",
			vendor: None,
			format: None,
			version: None,
			features: Vec::new(),
			description: info.description.to_owned(),
			latency_samples: None,
			param_count,
			matched,
			offset,
			params,
			next_offset,
		},
		DeviceRef::Plugin(desc) => DeviceInfoOutput {
			id: plugin_device_id(desc),
			name: desc.name.clone(),
			role: role_str(desc.role.into()),
			source: "plugin",
			vendor: Some(desc.vendor.clone()).filter(|v| !v.is_empty()),
			format: Some(desc.format.as_str()),
			version: Some(desc.version.clone()).filter(|v| !v.is_empty()),
			features: desc.features.clone(),
			description: desc.description.clone(),
			latency_samples: Some(desc.latency_samples),
			param_count,
			matched,
			offset,
			params,
			next_offset,
		},
	})
}

fn resolve_any_role<'a>(
	id: &str,
	catalog: Option<&'a PluginCatalog>,
) -> Result<DeviceRef<'a>, McpError> {
	match resolve_device(id, DeviceRole::Effect, catalog) {
		Err(McpError::WrongDeviceRole { .. }) => {
			resolve_device(id, DeviceRole::Instrument, catalog)
		}
		other => other,
	}
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScanPluginsArgs {
	/// Directories or plugin files to scan; omit for the standard plugin locations of this
	/// OS (e.g. ~/.clap, /usr/lib/clap, ~/.vst3, /usr/lib/vst3 on Linux, plus $CLAP_PATH).
	#[serde(default)]
	pub paths: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BlockedOut {
	pub path: String,
	pub reason: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScanPluginsOutput {
	/// Plugin files found on disk.
	pub files_found: usize,
	/// Files loaded by the scanner (unchanged or blocked files are skipped).
	pub files_scanned: usize,
	pub added: usize,
	pub updated: usize,
	pub removed: usize,
	/// Plugins in the catalog after the scan.
	pub total_plugins: usize,
	pub instruments: usize,
	pub effects: usize,
	/// Files that failed (crash, hang, invalid binary) during this scan; they are skipped
	/// until they change on disk.
	pub blocked: Vec<BlockedOut>,
	/// All blocked files, including ones from earlier scans.
	pub blocked_total: usize,
	pub cache_path: String,
	pub hint: String,
}

/// Scans plugins out-of-process (blocking; one subprocess per new/changed file), saves
/// the catalog cache and returns the new catalog with a summary.
pub fn scan_plugins_blocking(
	cache_path: PathBuf,
	args: ScanPluginsArgs,
) -> Result<(PluginCatalog, ScanPluginsOutput), McpError> {
	let roots: Vec<PathBuf> = match args.paths {
		Some(paths) if !paths.is_empty() => paths.into_iter().map(PathBuf::from).collect(),
		_ => cadenza_plugin_host::default_search_paths(),
	};
	let scanner = cadenza_plugin_host::scanner_path()?;
	let mut catalog = PluginCatalog::load_or_default(&cache_path);
	let report = catalog.scan(&roots, &scanner)?;
	catalog.save()?;
	let count = |role: PluginRole| catalog.descriptors().filter(|d| d.role == role).count();
	let out = ScanPluginsOutput {
		files_found: report.files_found,
		files_scanned: report.files_scanned,
		added: report.added.len(),
		updated: report.updated.len(),
		removed: report.removed.len(),
		total_plugins: catalog.descriptors().count(),
		instruments: count(PluginRole::Instrument),
		effects: count(PluginRole::Effect),
		blocked: report
			.blocked
			.iter()
			.map(|b| BlockedOut {
				path: b.path.display().to_string(),
				reason: b.reason.clone(),
			})
			.collect(),
		blocked_total: catalog.blocked().count(),
		cache_path: cache_path.display().to_string(),
		hint: "Search plugins with list_devices { query, source: \"plugin\" }; inspect params \
		       with get_device_info; insert them by their id (\"clap:<plugin id>\")."
			.to_owned(),
	};
	Ok((catalog, out))
}

/// Runs [`scan_plugins_blocking`] on a blocking thread and installs the result in the
/// session. The session lock is only taken briefly before and after the scan.
pub async fn scan_plugins(
	session: &std::sync::Mutex<Session>,
	args: ScanPluginsArgs,
) -> Result<ScanPluginsOutput, McpError> {
	let cache_path = lock(session).plugin_cache_path().to_path_buf();
	let (catalog, out) =
		tokio::task::spawn_blocking(move || scan_plugins_blocking(cache_path, args))
			.await
			.map_err(|e| McpError::Invalid(format!("plugin scan task panicked: {e}")))??;
	lock(session).set_plugins(std::sync::Arc::new(catalog));
	Ok(out)
}

fn lock(session: &std::sync::Mutex<Session>) -> std::sync::MutexGuard<'_, Session> {
	session.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn builtin_listing_is_searchable_and_paged() {
		let all = list_devices(
			None,
			ListDevicesArgs {
				source: Some(SourceFilter::Builtin),
				..Default::default()
			},
		);
		assert!(all.total >= 5);
		assert!(all.devices.iter().all(|d| d.params.is_some()));

		let reverb = list_devices(
			None,
			ListDevicesArgs {
				query: Some("REVERB".into()),
				role: Some(RoleFilter::Effect),
				..Default::default()
			},
		);
		assert!(
			reverb.devices.iter().any(|d| d.id == "reverb"),
			"{reverb:?}"
		);
		assert!(reverb.note.is_some(), "empty catalog hints at scan_plugins");

		let page = list_devices(
			None,
			ListDevicesArgs {
				source: Some(SourceFilter::Builtin),
				limit: Some(2),
				offset: Some(1),
				..Default::default()
			},
		);
		assert_eq!(page.devices.len(), 2);
		assert_eq!(page.devices[0].id, all.devices[1].id);
		assert_eq!(page.next_offset, Some(3));
	}

	#[test]
	fn builtin_info_filters_params() {
		let info = get_device_info(
			None,
			GetDeviceInfoArgs {
				device: "reverb".into(),
				param_query: Some("mix".into()),
				limit: None,
				offset: None,
				include_non_automatable: false,
			},
		)
		.unwrap();
		assert_eq!(info.source, "builtin");
		assert!(info.matched >= 1 && info.matched < info.param_count);
		assert!(
			info.params
				.iter()
				.all(|p| p.name.to_lowercase().contains("mix") || p.id.contains("mix"))
		);
		let err = get_device_info(
			None,
			GetDeviceInfoArgs {
				device: "clap:nope".into(),
				param_query: None,
				limit: None,
				offset: None,
				include_non_automatable: false,
			},
		)
		.unwrap_err();
		assert!(err.to_string().contains("scan_plugins"), "{err}");
	}
}
