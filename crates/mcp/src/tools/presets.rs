//! Device presets and plugin state.
//!
//! - Factory presets: built-in devices ship named parameter maps
//!   ([`cadenza_dsp::factory_presets`]); plugins expose theirs (CLAP preset discovery),
//!   loaded into a fresh instance whose resulting state is stored in the project.
//! - User presets: JSON files `<preset dir>/<device id>/<name>.json` holding the device's
//!   explicit params and (plugins) its captured state; the preset dir is
//!   `$CADENZA_PRESET_DIR`, else `$XDG_DATA_HOME/cadenza/presets` (see [`default_preset_dir`]).
//! - State capture: a plugin's opaque state is captured from a freshly loaded instance
//!   configured exactly like the project's device (stored state + explicit params) and
//!   stored with `Command::SetDeviceState`, so it is undoable and reloads exactly. It runs
//!   after loading a preset, on demand (`capture_plugin_state`) and on `save_project`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cadenza_core::{DeviceId, TrackId};
use cadenza_model::{Command, Device, DeviceKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::check_param_range;
use crate::error::McpError;
use crate::session::Session;

/// Overrides the user preset directory.
pub const PRESET_DIR_ENV: &str = "CADENZA_PRESET_DIR";
const PRESET_FORMAT_VERSION: u32 = 1;

/// Where user presets live: `$CADENZA_PRESET_DIR`, else the OS data dir +
/// `cadenza/presets` (`$XDG_DATA_HOME` or `~/.local/share` on Linux).
pub fn default_preset_dir() -> PathBuf {
	let var = |k: &str| {
		std::env::var_os(k)
			.filter(|v| !v.is_empty())
			.map(PathBuf::from)
	};
	if let Some(dir) = var(PRESET_DIR_ENV) {
		return dir;
	}
	let base = if cfg!(windows) {
		var("APPDATA")
	} else if cfg!(target_os = "macos") {
		var("HOME").map(|h| h.join("Library/Application Support"))
	} else {
		var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")))
	};
	base.unwrap_or_else(std::env::temp_dir)
		.join("cadenza")
		.join("presets")
}

/// A user preset file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPreset {
	pub format_version: u32,
	pub name: String,
	/// Device id as in `list_devices` (`"compressor"`, `"clap:<plugin id>"`, ...).
	pub device: String,
	#[serde(default)]
	pub params: BTreeMap<String, f32>,
	/// Plugin state (base64), if the device has one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub state: Option<String>,
	/// Sample zones (sampler / drum_sampler), with absolute sample paths.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub zones: Vec<cadenza_model::SampleZone>,
}

/// `list_devices`-style id of a project device.
fn device_key(device: &Device) -> String {
	match &device.kind {
		DeviceKind::Builtin { id } => id.clone(),
		DeviceKind::Clap { plugin_id, .. } => format!("clap:{plugin_id}"),
		DeviceKind::Vst3 { class_id, .. } => format!("vst3:{class_id}"),
	}
}

/// Folder name for a device's presets: its builtin id, CLAP plugin id or VST3 class id.
fn device_folder(device: &Device) -> String {
	let id = match &device.kind {
		DeviceKind::Builtin { id } => id,
		DeviceKind::Clap { plugin_id, .. } => plugin_id,
		DeviceKind::Vst3 { class_id, .. } => class_id,
	};
	safe_name(id)
}

fn safe_name(name: &str) -> String {
	name.trim()
		.chars()
		.map(|c| {
			if c.is_alphanumeric() || " -_.()+".contains(c) {
				c
			} else {
				'_'
			}
		})
		.collect::<String>()
		.trim_start_matches('.')
		.to_string()
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> McpError {
	McpError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	}
}

fn find_device(
	session: &Session,
	track: Option<TrackId>,
	id: DeviceId,
) -> Result<Device, McpError> {
	let project = session.document().project();
	if let Some(t) = track
		&& project.track(t).is_none()
	{
		return Err(cadenza_model::ModelError::TrackNotFound(t).into());
	}
	project.device(track, id).cloned().ok_or_else(|| {
		cadenza_model::ModelError::DeviceNotFound {
			location: match track {
				Some(t) => format!("track {t}"),
				None => "the master channel".into(),
			},
			device: id,
		}
		.into()
	})
}

fn user_presets(dir: &Path, device: &Device) -> Vec<(UserPreset, PathBuf)> {
	let folder = dir.join(device_folder(device));
	let Ok(entries) = std::fs::read_dir(&folder) else {
		return Vec::new();
	};
	let mut out: Vec<(UserPreset, PathBuf)> = entries
		.flatten()
		.map(|e| e.path())
		.filter(|p| p.extension().is_some_and(|e| e == "json"))
		.filter_map(|p| {
			let preset: UserPreset = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
			Some((preset, p))
		})
		.collect();
	out.sort_by_key(|a| a.0.name.to_lowercase());
	out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PresetSource {
	/// Shipped with the device (built-in presets or the plugin's own).
	Factory,
	/// Saved with save_preset.
	User,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListPresetsArgs {
	/// Track holding the device; omit for a master insert.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// Instrument or insert id from get_project.
	pub device_id: DeviceId,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PresetOut {
	pub name: String,
	pub source: PresetSource,
	/// Bank/folder (plugin factory presets) or what it sounds like (built-ins).
	#[serde(skip_serializing_if = "String::is_empty")]
	pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListPresetsOutput {
	/// Device id (list_devices style).
	pub device: String,
	pub presets: Vec<PresetOut>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub note: Option<String>,
}

/// Factory presets of `device`: built-in tables, or the plugin's own (loads the plugin).
fn factory_presets(session: &mut Session, device: &Device) -> Result<Vec<PresetOut>, McpError> {
	Ok(match &device.kind {
		DeviceKind::Builtin { id } => cadenza_dsp::factory_presets(id)
			.iter()
			.map(|p| PresetOut {
				name: p.name.to_owned(),
				source: PresetSource::Factory,
				description: p.description.to_owned(),
			})
			.collect(),
		_ => {
			let catalog = session.plugins();
			let hosting = session.plugin_hosting().clone();
			cadenza_engine::plugin_presets(device, Some(&catalog), &hosting)?
				.into_iter()
				.map(|p| PresetOut {
					name: p.name,
					source: PresetSource::Factory,
					description: p.category,
				})
				.collect()
		}
	})
}

pub fn list_presets(
	session: &mut Session,
	args: ListPresetsArgs,
) -> Result<ListPresetsOutput, McpError> {
	let device = find_device(session, args.track_id, args.device_id)?;
	let mut presets = factory_presets(session, &device)?;
	presets.extend(
		user_presets(session.preset_dir(), &device)
			.into_iter()
			.map(|(p, _)| PresetOut {
				name: p.name,
				source: PresetSource::User,
				description: String::new(),
			}),
	);
	let note = presets.is_empty().then(|| {
		"this device has no presets yet; dial it in with set_device_params and keep the \
		 result with save_preset"
			.to_string()
	});
	Ok(ListPresetsOutput {
		device: device_key(&device),
		presets,
		note,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadPresetArgs {
	/// Track holding the device; omit for a master insert.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	pub device_id: DeviceId,
	/// Preset name from list_presets (case-insensitive).
	pub name: String,
	/// factory | user; only needed when both have a preset with this name.
	#[serde(default)]
	pub source: Option<PresetSource>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LoadPresetOutput {
	pub name: String,
	pub source: PresetSource,
	/// Explicit params now set on the device (others are at the preset's/plugin's values).
	pub params: BTreeMap<String, f32>,
	/// Plugin state stored with the device.
	pub has_state: bool,
	/// Sample zones now loaded (sampler / drum_sampler presets).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub zones: Option<usize>,
}

pub fn load_preset(
	session: &mut Session,
	args: LoadPresetArgs,
) -> Result<LoadPresetOutput, McpError> {
	let device = find_device(session, args.track_id, args.device_id)?;
	let wanted = args.name.trim().to_lowercase();
	let user = user_presets(session.preset_dir(), &device)
		.into_iter()
		.find(|(p, _)| p.name.to_lowercase() == wanted);
	let factory = if args.source == Some(PresetSource::User) {
		Vec::new()
	} else {
		factory_presets(session, &device)?
	};
	let factory = factory
		.into_iter()
		.find(|p| p.name.to_lowercase() == wanted);
	let source = match (args.source, &factory, &user) {
		(Some(s), _, _) => s,
		(None, Some(_), Some(_)) => {
			return Err(McpError::Invalid(format!(
				"both a factory and a user preset are named '{}'; pass source: \"factory\" or \"user\"",
				args.name
			)));
		}
		(None, Some(_), None) => PresetSource::Factory,
		_ => PresetSource::User,
	};
	let not_found = || {
		McpError::Invalid(format!(
			"no {} preset named '{}' for device '{}'; see list_presets",
			match source {
				PresetSource::Factory => "factory",
				PresetSource::User => "user",
			},
			args.name,
			device.name
		))
	};
	let (name, state, params, zones) = match source {
		PresetSource::Factory => {
			let preset = factory.ok_or_else(not_found)?;
			match &device.kind {
				DeviceKind::Builtin { id } => {
					let p = cadenza_dsp::factory_presets(id)
						.iter()
						.find(|p| p.name == preset.name)
						.ok_or_else(not_found)?;
					let params = p
						.params
						.iter()
						.map(|(k, v)| ((*k).to_owned(), *v))
						.collect();
					let zones = cadenza_dsp::uses_samples(id)
						.then(|| cadenza_engine::factory::preset_zones(p))
						.transpose()?;
					(preset.name, None, params, zones)
				}
				_ => {
					let catalog = session.plugins();
					let hosting = session.plugin_hosting().clone();
					let key = cadenza_engine::plugin_presets(&device, Some(&catalog), &hosting)?
						.into_iter()
						.find(|p| p.name == preset.name)
						.ok_or_else(not_found)?
						.key;
					let state =
						cadenza_engine::preset_state(&device, &key, Some(&catalog), &hosting)?;
					(preset.name, Some(state), BTreeMap::new(), None)
				}
			}
		}
		PresetSource::User => {
			let (preset, path) = user.ok_or_else(not_found)?;
			if preset.device != device_key(&device) {
				return Err(McpError::Invalid(format!(
					"user preset '{}' ({}) was saved for device '{}', but this device is '{}'",
					preset.name,
					path.display(),
					preset.device,
					device_key(&device)
				)));
			}
			if let DeviceKind::Builtin { id } = &device.kind {
				let info = cadenza_dsp::device_info(id);
				for (param, value) in &preset.params {
					check_param_range(info, param, *value)?;
				}
			}
			let zones = device
				.builtin_id()
				.is_some_and(cadenza_dsp::uses_samples)
				.then_some(preset.zones);
			(preset.name, preset.state, preset.params, zones)
		}
	};
	let has_state = state.is_some();
	let set_state = Command::SetDeviceState {
		track: args.track_id,
		device: args.device_id,
		state,
		params: Some(params.clone()),
	};
	let zone_count = zones.as_ref().map(Vec::len);
	let command = match zones {
		Some(zones) => Command::Batch {
			label: format!("Load preset '{name}'"),
			commands: vec![
				set_state,
				Command::SetSampleZones {
					track: args.track_id,
					device: args.device_id,
					zones,
				},
			],
		},
		None => set_state,
	};
	session.document_mut().execute(command)?;
	Ok(LoadPresetOutput {
		name,
		source,
		params,
		has_state,
		zones: zone_count,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SavePresetArgs {
	/// Track holding the device; omit for a master insert.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	pub device_id: DeviceId,
	/// Preset name (also its file name).
	pub name: String,
	/// Replace an existing user preset with the same name (default false).
	#[serde(default)]
	pub overwrite: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SavePresetOutput {
	pub path: String,
}

pub fn save_preset(
	session: &mut Session,
	args: SavePresetArgs,
) -> Result<SavePresetOutput, McpError> {
	let device = find_device(session, args.track_id, args.device_id)?;
	let file_name = safe_name(&args.name);
	if file_name.is_empty() || file_name.chars().count() > 100 {
		return Err(McpError::Invalid(format!(
			"preset name {:?} is not usable; use 1-100 letters, digits, spaces or -_.()+",
			args.name
		)));
	}
	let state = if device.is_plugin() {
		let catalog = session.plugins();
		let hosting = session.plugin_hosting().clone();
		cadenza_engine::capture_state(&device, Some(&catalog), &hosting)?.or(device.state.clone())
	} else {
		None
	};
	let preset = UserPreset {
		format_version: PRESET_FORMAT_VERSION,
		name: args.name.trim().to_owned(),
		device: device_key(&device),
		params: device.params.clone(),
		state,
		zones: device.zones.clone(),
	};
	let folder = session.preset_dir().join(device_folder(&device));
	let path = folder.join(format!("{file_name}.json"));
	if path.exists() && !args.overwrite {
		return Err(McpError::Invalid(format!(
			"a user preset named '{}' already exists ({}); pass overwrite: true or pick another name",
			args.name,
			path.display()
		)));
	}
	std::fs::create_dir_all(&folder).map_err(|e| io_err(&folder, e))?;
	let json = serde_json::to_vec_pretty(&preset).map_err(|e| io_err(&path, e))?;
	std::fs::write(&path, json).map_err(|e| io_err(&path, e))?;
	Ok(SavePresetOutput {
		path: path.display().to_string(),
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CapturePluginStateArgs {
	/// Track holding the device; omit for a master insert (with device_id).
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// Plugin device to capture; omit to capture every plugin device in the project.
	#[serde(default)]
	pub device_id: Option<DeviceId>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CapturedOut {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub track_id: Option<TrackId>,
	pub device_id: DeviceId,
	pub name: String,
	/// The stored state changed (and was updated in one undoable step).
	pub changed: bool,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct CapturePluginStateOutput {
	pub captured: Vec<CapturedOut>,
	/// Plugin devices whose state could not be captured (kept as they were).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

/// Plugin devices of the project with their location.
fn plugin_devices(session: &Session) -> Vec<(Option<TrackId>, Device)> {
	let project = session.document().project();
	project
		.tracks
		.iter()
		.flat_map(|t| t.devices().map(move |d| (Some(t.id), d.clone())))
		.chain(project.master.inserts.iter().map(|d| (None, d.clone())))
		.filter(|(_, d)| d.is_plugin())
		.collect()
}

/// Captures and stores the state of `targets` (one undo step for all changes). With
/// `strict`, the first failure is returned; otherwise failures become warnings.
fn capture(
	session: &mut Session,
	targets: Vec<(Option<TrackId>, Device)>,
	strict: bool,
) -> Result<CapturePluginStateOutput, McpError> {
	let mut out = CapturePluginStateOutput::default();
	if targets.is_empty() {
		return Ok(out);
	}
	let catalog = session.plugins();
	let hosting = session.plugin_hosting().clone();
	let mut commands = Vec::new();
	for (track, device) in targets {
		let state = match cadenza_engine::capture_state(&device, Some(&catalog), &hosting) {
			Ok(Some(state)) => state,
			Ok(None) => {
				out.warnings.push(format!(
					"device '{}' ({}) does not save state; its params are all that is stored",
					device.name, device.id
				));
				continue;
			}
			Err(e) if strict => return Err(e.into()),
			Err(e) => {
				out.warnings.push(format!("{e}; its stored state was kept"));
				continue;
			}
		};
		let changed = device.state.as_deref() != Some(state.as_str());
		if changed {
			commands.push(Command::SetDeviceState {
				track,
				device: device.id,
				state: Some(state),
				params: None,
			});
		}
		out.captured.push(CapturedOut {
			track_id: track,
			device_id: device.id,
			name: device.name,
			changed,
		});
	}
	if !commands.is_empty() {
		session.document_mut().execute(Command::Batch {
			label: "Capture plugin state".into(),
			commands,
		})?;
	}
	Ok(out)
}

pub fn capture_plugin_state(
	session: &mut Session,
	args: CapturePluginStateArgs,
) -> Result<CapturePluginStateOutput, McpError> {
	match args.device_id {
		Some(id) => {
			let device = find_device(session, args.track_id, id)?;
			if !device.is_plugin() {
				return Err(McpError::Invalid(format!(
					"device '{}' is built-in: its params are its whole state, nothing to capture",
					device.name
				)));
			}
			capture(session, vec![(args.track_id, device)], true)
		}
		None => {
			let targets = plugin_devices(session);
			capture(session, targets, false)
		}
	}
}

/// Captures every plugin device before a save; failures are warnings.
pub fn capture_all(session: &mut Session) -> Result<CapturePluginStateOutput, McpError> {
	let targets = plugin_devices(session);
	capture(session, targets, false)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn file_names_are_sanitized() {
		assert_eq!(safe_name(" My Pad / v2 "), "My Pad _ v2");
		assert_eq!(safe_name("../etc"), "_etc");
		assert_eq!(
			safe_name("org.surge-synth-team.surge-xt"),
			"org.surge-synth-team.surge-xt"
		);
	}

	#[test]
	fn default_dir_honors_the_override() {
		assert!(
			default_preset_dir().ends_with("presets") || std::env::var_os(PRESET_DIR_ENV).is_some()
		);
	}
}
