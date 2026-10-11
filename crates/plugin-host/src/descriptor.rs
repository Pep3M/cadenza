//! Format-agnostic description of an installed plugin, produced by the scanner
//! and stored in the catalog. These types are what the AI sees when choosing
//! and configuring third-party devices.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use cadenza_dsp::{DeviceInfo, DeviceRole, ParamInfo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Plugin standard a binary implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
	/// CLever Audio Plugin (`.clap`), hosted in-process via clack.
	Clap,
	/// Steinberg VST3 (`.vst3` bundle or file), hosted in-process via the `vst3` COM bindings.
	Vst3,
}

impl PluginFormat {
	pub fn as_str(self) -> &'static str {
		match self {
			PluginFormat::Clap => "CLAP",
			PluginFormat::Vst3 => "VST3",
		}
	}
}

/// How a plugin can be used in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginRole {
	/// Generates sound from MIDI notes; goes in a track's instrument slot.
	Instrument,
	/// Processes audio; goes in a channel's insert chain.
	Effect,
}

impl From<PluginRole> for DeviceRole {
	fn from(role: PluginRole) -> Self {
		match role {
			PluginRole::Instrument => DeviceRole::Instrument,
			PluginRole::Effect => DeviceRole::Effect,
		}
	}
}

/// One automatable plugin parameter. Values are plain (unnormalized) numbers in
/// `min..=max`, exactly as the plugin defines them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginParam {
	/// Stable parameter id used in `Device.params` and automation addresses.
	/// For CLAP this is the plugin's `clap_id`, for VST3 the `ParamID`, both
	/// written as decimal strings.
	pub id: String,
	/// Display name chosen by the plugin vendor (e.g. "Cutoff").
	pub name: String,
	/// Group path inside the plugin (e.g. "Scene A/Filter 1"); may be empty.
	pub module: String,
	pub min: f64,
	pub max: f64,
	pub default: f64,
	/// Only integer values are meaningful (switches, enums, modes).
	pub stepped: bool,
	/// The host may change this parameter over time (automation lanes).
	pub automatable: bool,
	/// The plugin's own bypass switch (`max` = bypassed). Prefer the device's
	/// `bypass` flag; this exists so state and automation stay faithful.
	#[serde(default)]
	pub bypass: bool,
}

/// Everything Cadenza knows about one installed plugin without loading it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginDescriptor {
	pub format: PluginFormat,
	/// Unique plugin id: the CLAP plugin id (reverse-DNS, e.g. "org.surge-synth-team.surge-xt")
	/// or, for VST3, the audio-module class id as 32 hex digits. Use it as `plugin_id`
	/// (CLAP) or `class_id` (VST3) when inserting the device.
	pub id: String,
	pub name: String,
	pub vendor: String,
	pub version: String,
	/// File or bundle the plugin lives in.
	pub path: PathBuf,
	pub role: PluginRole,
	/// Vendor-declared category tags (e.g. "instrument", "synthesizer", "reverb", "stereo").
	pub features: Vec<String>,
	/// What the device is and how it is typically used, written for an AI choosing devices.
	pub description: String,
	/// Parameters the AI can set or automate, in the order `Processor::set_param` indexes them.
	/// Hidden and read-only parameters are omitted.
	pub params: Vec<PluginParam>,
	/// Channels of the main audio input (0 for most instruments).
	pub audio_inputs: u32,
	/// Channels of the main audio output.
	pub audio_outputs: u32,
	/// Accepts MIDI/note events.
	pub note_input: bool,
	/// Processing latency reported at activation (48 kHz), compensated by the engine.
	pub latency_samples: u32,
}

impl PluginDescriptor {
	/// Classifies a plugin from its vendor features, falling back to its ports
	/// when the vendor declared neither "instrument" nor "audio-effect".
	pub fn role_from(features: &[String], note_input: bool, audio_inputs: u32) -> PluginRole {
		let has = |f: &str| features.iter().any(|x| x.eq_ignore_ascii_case(f));
		if has("instrument") {
			PluginRole::Instrument
		} else if has("audio-effect") {
			PluginRole::Effect
		} else if note_input && audio_inputs == 0 {
			PluginRole::Instrument
		} else {
			PluginRole::Effect
		}
	}

	/// Builds the AI-facing description from the vendor text and the scanned
	/// capabilities.
	pub fn compose_description(&self, vendor_description: &str) -> String {
		let role = match self.role {
			PluginRole::Instrument => "instrument (plays MIDI notes; use as a track instrument)",
			PluginRole::Effect => "audio effect (use as a channel insert)",
		};
		let mut s = format!(
			"{} by {} — {} {role}.",
			self.name,
			if self.vendor.is_empty() {
				"unknown vendor"
			} else {
				&self.vendor
			},
			self.format.as_str()
		);
		let tags: Vec<&str> = self
			.features
			.iter()
			.map(String::as_str)
			.filter(|f| !matches!(*f, "instrument" | "audio-effect"))
			.collect();
		if !tags.is_empty() {
			s.push_str(&format!(" Categories: {}.", tags.join(", ")));
		}
		let vendor_description = vendor_description.trim();
		if !vendor_description.is_empty() {
			s.push(' ');
			s.push_str(vendor_description);
			if !vendor_description.ends_with('.') {
				s.push('.');
			}
		}
		let io = |n: u32| match n {
			0 => "no".to_string(),
			1 => "mono".to_string(),
			2 => "stereo".to_string(),
			n => format!("{n}-channel"),
		};
		let count = self.params.len();
		s.push_str(&format!(
			" Audio: {} in, {} out; {count} parameter{}",
			io(self.audio_inputs),
			io(self.audio_outputs),
			if count == 1 { "" } else { "s" }
		));
		if self.latency_samples > 0 {
			s.push_str(&format!(", {} samples latency", self.latency_samples));
		}
		s.push('.');
		s
	}

	/// Index of a parameter (as used by `Processor::set_param`) from its stable id.
	pub fn param_index(&self, id: &str) -> Option<usize> {
		self.params.iter().position(|p| p.id == id)
	}

	/// A `cadenza_dsp::DeviceInfo` view of this descriptor, as returned by the
	/// plugin's `Processor::info()`.
	///
	/// `Processor::info` must return `&'static DeviceInfo`, so the info (and its
	/// strings) is leaked exactly once per distinct descriptor content and then
	/// reused from a global cache. Memory is therefore bounded by the number of
	/// distinct plugins (and plugin revisions) ever instantiated in the process,
	/// not by the number of instances.
	pub fn device_info(&self) -> &'static DeviceInfo {
		static CACHE: OnceLock<Mutex<HashMap<u64, &'static DeviceInfo>>> = OnceLock::new();
		let mut hasher = DefaultHasher::new();
		self.format.hash(&mut hasher);
		self.id.hash(&mut hasher);
		self.name.hash(&mut hasher);
		self.role.hash(&mut hasher);
		self.description.hash(&mut hasher);
		for p in &self.params {
			p.id.hash(&mut hasher);
			p.name.hash(&mut hasher);
			p.module.hash(&mut hasher);
			p.min.to_bits().hash(&mut hasher);
			p.max.to_bits().hash(&mut hasher);
			p.default.to_bits().hash(&mut hasher);
			p.stepped.hash(&mut hasher);
			p.automatable.hash(&mut hasher);
			p.bypass.hash(&mut hasher);
		}
		let key = hasher.finish();
		let mut cache = CACHE
			.get_or_init(Default::default)
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		cache.entry(key).or_insert_with(|| self.leak_info())
	}

	fn leak_info(&self) -> &'static DeviceInfo {
		fn leak(s: &str) -> &'static str {
			Box::leak(s.to_owned().into_boxed_str())
		}
		let params: Vec<ParamInfo> = self
			.params
			.iter()
			.map(|p| {
				let mut notes = Vec::new();
				if !p.module.is_empty() {
					notes.push(format!("group: {}", p.module));
				}
				if p.stepped {
					notes.push("integer steps".to_string());
				}
				if !p.automatable {
					notes.push("not automatable".to_string());
				}
				if p.bypass {
					notes.push("plugin bypass switch".to_string());
				}
				ParamInfo {
					id: leak(&p.id),
					name: leak(&p.name),
					min: p.min as f32,
					max: p.max as f32,
					default: p.default as f32,
					unit: "",
					description: leak(&notes.join("; ")),
				}
			})
			.collect();
		Box::leak(Box::new(DeviceInfo {
			id: leak(&self.id),
			name: leak(&self.name),
			role: self.role.into(),
			description: leak(&self.description),
			params: Box::leak(params.into_boxed_slice()),
		}))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> PluginDescriptor {
		PluginDescriptor {
			format: PluginFormat::Clap,
			id: "x.y.z".into(),
			name: "Z".into(),
			vendor: "Y".into(),
			version: "1".into(),
			path: "/tmp/z.clap".into(),
			role: PluginRole::Effect,
			features: vec!["audio-effect".into(), "reverb".into()],
			description: String::new(),
			params: vec![PluginParam {
				id: "7".into(),
				name: "Mix".into(),
				module: "Main".into(),
				min: 0.0,
				max: 1.0,
				default: 0.5,
				stepped: false,
				automatable: true,
				bypass: false,
			}],
			audio_inputs: 2,
			audio_outputs: 2,
			note_input: false,
			latency_samples: 0,
		}
	}

	#[test]
	fn role_detection() {
		let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
		assert_eq!(
			PluginDescriptor::role_from(&f(&["instrument", "stereo"]), true, 0),
			PluginRole::Instrument
		);
		assert_eq!(
			PluginDescriptor::role_from(&f(&["audio-effect"]), true, 2),
			PluginRole::Effect
		);
		assert_eq!(
			PluginDescriptor::role_from(&[], true, 0),
			PluginRole::Instrument
		);
		assert_eq!(
			PluginDescriptor::role_from(&[], false, 2),
			PluginRole::Effect
		);
	}

	#[test]
	fn description_mentions_capabilities() {
		let d = sample();
		let text = d.compose_description("A plate reverb");
		assert!(text.contains("audio effect"), "{text}");
		assert!(text.contains("reverb"), "{text}");
		assert!(text.contains("A plate reverb."), "{text}");
		assert!(
			text.contains("Audio: stereo in, stereo out; 1 parameter."),
			"{text}"
		);
	}

	#[test]
	fn device_info_is_leaked_once_per_descriptor() {
		let d = sample();
		let a = d.device_info();
		let b = d.clone().device_info();
		assert!(std::ptr::eq(a, b));
		assert_eq!(a.params[0].id, "7");
		assert_eq!(a.param_index("7"), Some(0));
		let mut other = d.clone();
		other.params[0].max = 2.0;
		assert!(!std::ptr::eq(a, other.device_info()));
	}
}
