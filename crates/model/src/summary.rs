//! Compact, AI-oriented view of a project: positions in beats, pitches as note names,
//! notes counted rather than listed.

use std::collections::BTreeMap;

use cadenza_core::{ClipId, DeviceId, ParamAddress, Tick, TrackId};
use schemars::JsonSchema;
use serde::Serialize;

use crate::project::{
	Channel, Clip, ClipContent, Device, DeviceKind, Output, OutputRoute, Project, SidechainSource,
	Take, Track, TrackInput, TrackKind,
};
use crate::sample::{LoopMode, SampleZone};

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ProjectSummary {
	pub name: String,
	pub tempo: Vec<TempoSummary>,
	/// Time signature of bar 1, e.g. "4/4".
	pub time_signature: String,
	/// Later time signature changes (empty: one signature for the whole song).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub time_signature_changes: Vec<TimeSignatureSummary>,
	pub length_beats: f64,
	pub length_bars: f64,
	pub markers: Vec<MarkerSummary>,
	pub master: MasterSummary,
	pub tracks: Vec<TrackSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TimeSignatureSummary {
	/// 1-based bar the signature starts at.
	pub bar: u32,
	pub beat: f64,
	/// e.g. "3/4".
	pub time_signature: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TempoSummary {
	pub beat: f64,
	pub bpm: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MarkerSummary {
	pub beat: f64,
	/// 1-based bar position (fractional inside a bar), following time signature changes.
	pub bar: f64,
	pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MasterSummary {
	pub volume_db: f32,
	pub pan: f32,
	pub mute: bool,
	pub inserts: Vec<DeviceSummary>,
	pub automation: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TrackSummary {
	pub id: TrackId,
	pub name: String,
	pub kind: TrackKind,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub color: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub instrument: Option<DeviceSummary>,
	pub inserts: Vec<DeviceSummary>,
	pub volume_db: f32,
	pub pan: f32,
	pub mute: bool,
	pub solo: bool,
	/// "master" or the id of the Bus track this track feeds.
	pub output: String,
	pub sends: Vec<SendSummary>,
	pub clips: Vec<ClipSummary>,
	/// Addresses of automated parameters on this track.
	pub automation: Vec<String>,
	/// The folder track this track is in.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub folder: Option<TrackId>,
	/// Audio input it records from (audio tracks).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub input: Option<TrackInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DeviceSummary {
	pub id: DeviceId,
	/// "builtin:{id}", "clap:{plugin_id}" or "vst3:{class_id}".
	pub kind: String,
	pub name: String,
	pub bypass: bool,
	/// Only explicitly set values; other params are at their defaults.
	pub params: BTreeMap<String, f32>,
	/// Plugin state is stored (presets/captured settings are restored on load).
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub has_state: bool,
	/// Track keying this device's sidechain input.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub sidechain: Option<SidechainSource>,
	/// Extra instrument outputs in use (main output only when empty).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub aux_outputs: Vec<OutputRoute>,
	/// Sample zones (sampler / drum_sampler), by index.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub zones: Vec<ZoneSummary>,
}

/// One sample zone, compactly: `keys` is a note ("C1 (36)") for drum pads, a range
/// ("C2..B3") otherwise; settings appear only when not at their defaults.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ZoneSummary {
	pub index: usize,
	pub keys: String,
	/// Original file name of the sample.
	pub sample: String,
	pub root: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub velocity: Option<String>,
	/// Non-default settings, e.g. "gain_db=-3 choke=1 loop=forward".
	#[serde(skip_serializing_if = "String::is_empty")]
	pub settings: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SendSummary {
	pub id: DeviceId,
	pub target: TrackId,
	pub target_name: String,
	pub level_db: f32,
	pub pre_fader: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ClipSummary {
	pub id: ClipId,
	pub name: String,
	/// "midi" or "audio".
	pub kind: &'static str,
	pub start_beats: f64,
	pub length_beats: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub note_count: Option<usize>,
	/// Lowest and highest pitch, e.g. "C3..G4".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pitch_range: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub file: Option<String>,
	/// Audio clip fades, e.g. "in 0.010 s linear, out 2.000 s exponential".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fades: Option<String>,
	/// One of several alternative takes of a region; only the active take plays.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub take: Option<Take>,
	/// MIDI controller lanes, e.g. "cc1 (12 events, 0.00..1.00)", "pitch_bend (8 events,
	/// -1.00..0.50)".
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub controllers: Vec<String>,
}

impl Project {
	pub fn summary(&self) -> ProjectSummary {
		let meter = self.meter();
		let length = beats(self.end_tick());
		ProjectSummary {
			name: self.name.clone(),
			tempo: self
				.tempo
				.points()
				.iter()
				.map(|p| TempoSummary {
					beat: beats(p.tick),
					bpm: p.bpm,
				})
				.collect(),
			time_signature: format!(
				"{}/{}",
				self.time_signature.numerator, self.time_signature.denominator
			),
			time_signature_changes: meter
				.change_ticks()
				.map(|(bar, tick, ts)| TimeSignatureSummary {
					bar: bar + 1,
					beat: beats(tick),
					time_signature: format!("{}/{}", ts.numerator, ts.denominator),
				})
				.collect(),
			length_beats: length,
			length_bars: round(meter.bar_position(self.end_tick())),
			markers: self
				.markers
				.iter()
				.map(|m| MarkerSummary {
					beat: beats(m.tick),
					bar: round(meter.bar_position(m.tick) + 1.0),
					name: m.name.clone(),
				})
				.collect(),
			master: MasterSummary {
				volume_db: self.master.volume_db,
				pan: self.master.pan,
				mute: self.master.mute,
				inserts: self.master.inserts.iter().map(device_summary).collect(),
				automation: self.automation_addresses(None),
			},
			tracks: self.tracks.iter().map(|t| self.track_summary(t)).collect(),
		}
	}

	fn track_summary(&self, track: &Track) -> TrackSummary {
		let Channel {
			volume_db,
			pan,
			mute,
			solo,
			inserts,
			sends,
			output,
		} = &track.channel;
		TrackSummary {
			id: track.id,
			name: track.name.clone(),
			kind: track.kind,
			color: track.color.clone(),
			instrument: track.instrument.as_ref().map(device_summary),
			inserts: inserts.iter().map(device_summary).collect(),
			volume_db: *volume_db,
			pan: *pan,
			mute: *mute,
			solo: *solo,
			output: match output {
				Output::Master => "master".into(),
				Output::Bus(id) => id.to_string(),
			},
			sends: sends
				.iter()
				.map(|s| SendSummary {
					id: s.id,
					target: s.target,
					target_name: self
						.track(s.target)
						.map(|t| t.name.clone())
						.unwrap_or_default(),
					level_db: s.level_db,
					pre_fader: s.pre_fader,
				})
				.collect(),
			clips: track.clips.iter().map(clip_summary).collect(),
			automation: self.automation_addresses(Some(track.id)),
			input: track.input,
			folder: track.folder,
		}
	}

	fn automation_addresses(&self, track: Option<TrackId>) -> Vec<String> {
		self.automation
			.iter()
			.map(|l| &l.address)
			.filter(|a: &&ParamAddress| a.track == track)
			.map(ToString::to_string)
			.collect()
	}
}

fn device_summary(device: &Device) -> DeviceSummary {
	let kind = match &device.kind {
		DeviceKind::Builtin { id } => format!("builtin:{id}"),
		DeviceKind::Clap { plugin_id, .. } => format!("clap:{plugin_id}"),
		DeviceKind::Vst3 { class_id, .. } => format!("vst3:{class_id}"),
	};
	DeviceSummary {
		id: device.id,
		kind,
		name: device.name.clone(),
		bypass: device.bypass,
		params: device.params.clone(),
		has_state: device.state.is_some(),
		sidechain: device.sidechain,
		aux_outputs: device.aux_outputs.clone(),
		zones: zone_summaries(&device.zones),
	}
}

/// Compact views of a zone map, by index (as in [`DeviceSummary::zones`]).
pub fn zone_summaries(zones: &[SampleZone]) -> Vec<ZoneSummary> {
	zones
		.iter()
		.enumerate()
		.map(|(i, z)| zone_summary(i, z))
		.collect()
}

fn zone_summary(index: usize, z: &SampleZone) -> ZoneSummary {
	let keys = if z.is_single_key() {
		format!("{} ({})", note_name(z.key_low), z.key_low)
	} else {
		format!("{}..{}", note_name(z.key_low), note_name(z.key_high))
	};
	let velocity = (z.vel_low > 0.0 || z.vel_high < 1.0)
		.then(|| format!("{:.2}..{:.2}", z.vel_low, z.vel_high));
	let mut settings: Vec<String> = Vec::new();
	let num = |settings: &mut Vec<String>, name: &str, v: f64| {
		if v != 0.0 {
			settings.push(format!("{name}={}", (v * 1000.0).round() / 1000.0));
		}
	};
	num(&mut settings, "tune", f64::from(z.tune));
	num(&mut settings, "gain_db", f64::from(z.gain_db));
	num(&mut settings, "pan", f64::from(z.pan));
	num(&mut settings, "start", z.start);
	num(&mut settings, "end", z.end.unwrap_or(0.0));
	num(&mut settings, "attack_ms", f64::from(z.attack_ms));
	num(
		&mut settings,
		"decay_ms",
		f64::from(z.decay_ms.unwrap_or(0.0)),
	);
	num(&mut settings, "choke", f64::from(z.choke));
	num(&mut settings, "output", f64::from(z.output));
	num(&mut settings, "round_robin", f64::from(z.round_robin));
	if !z.loop_mode.is_off() {
		let mode = match z.loop_mode {
			LoopMode::PingPong => "ping_pong",
			_ => "forward",
		};
		settings.push(format!("loop={mode}"));
		num(&mut settings, "loop_start", z.loop_start);
		num(&mut settings, "loop_end", z.loop_end.unwrap_or(0.0));
		num(&mut settings, "loop_crossfade", z.loop_crossfade);
	}
	if z.reverse {
		settings.push("reverse".into());
	}
	ZoneSummary {
		index,
		keys,
		sample: z.sample.display_name(),
		root: note_name(z.root_key),
		velocity,
		settings: settings.join(" "),
	}
}

fn clip_summary(clip: &Clip) -> ClipSummary {
	let (kind, note_count, pitch_range, file) = match &clip.content {
		ClipContent::Midi { notes, .. } => {
			let low = notes.iter().map(|n| n.pitch).min();
			let high = notes.iter().map(|n| n.pitch).max();
			let range = low
				.zip(high)
				.map(|(l, h)| format!("{}..{}", note_name(l), note_name(h)));
			("midi", Some(notes.len()), range, None)
		}
		ClipContent::Audio { file, .. } => ("audio", None, None, Some(file.display().to_string())),
	};
	let fades = match clip.fades() {
		(None, None) => None,
		(fade_in, fade_out) => {
			let describe = |dir: &str, f: crate::ClipFade| {
				let curve = serde_json::to_value(f.curve)
					.ok()
					.and_then(|v| v.as_str().map(str::to_owned))
					.unwrap_or_default();
				format!("{dir} {:.3} s {curve}", f.seconds)
			};
			let parts: Vec<String> = [("in", fade_in), ("out", fade_out)]
				.into_iter()
				.filter_map(|(d, f)| f.map(|f| describe(d, f)))
				.collect();
			Some(parts.join(", "))
		}
	};
	ClipSummary {
		id: clip.id,
		name: clip.name.clone(),
		kind,
		start_beats: beats(clip.start),
		length_beats: beats(clip.length),
		note_count,
		pitch_range,
		file,
		fades,
		take: clip.take,
		controllers: clip
			.controllers()
			.iter()
			.map(|l| {
				let lo = l
					.points
					.iter()
					.map(|p| p.value)
					.fold(f32::INFINITY, f32::min);
				let hi = l
					.points
					.iter()
					.map(|p| p.value)
					.fold(f32::NEG_INFINITY, f32::max);
				format!(
					"{} ({} events, {lo:.2}..{hi:.2})",
					l.controller,
					l.points.len()
				)
			})
			.collect(),
	}
}

/// Scientific pitch notation with sharps; MIDI 60 = "C4".
pub fn note_name(pitch: u8) -> String {
	const NAMES: [&str; 12] = [
		"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
	];
	let octave = i32::from(pitch / 12) - 1;
	format!("{}{octave}", NAMES[usize::from(pitch % 12)])
}

fn beats(tick: Tick) -> f64 {
	round(tick.beats())
}

/// Keeps the JSON readable (no 0.33333333333 noise).
fn round(value: f64) -> f64 {
	(value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::project::{Marker, Note};
	use cadenza_core::PPQ;

	#[test]
	fn note_names() {
		assert_eq!(note_name(60), "C4");
		assert_eq!(note_name(0), "C-1");
		assert_eq!(note_name(69), "A4");
		assert_eq!(note_name(61), "C#4");
		assert_eq!(note_name(127), "G9");
	}

	#[test]
	fn summary_shape() {
		let mut p = Project::new("Demo");
		let bus = Track::new("Reverb", TrackKind::Bus);
		let mut lead = Track::new("Lead", TrackKind::Instrument)
			.with_instrument(Device::builtin("subtractive", "Synth").with_param("cutoff", 1200.0));
		lead.channel.output = Output::Bus(bus.id);
		lead.clips.push(
			Clip::midi("Riff", Tick(PPQ * 4), Tick(PPQ * 8)).with_notes([
				Note::new(48, Tick(0), Tick(PPQ), 0.8),
				Note::new(67, Tick(PPQ), Tick(PPQ), 0.8),
				Note::new(60, Tick(PPQ * 2), Tick(PPQ / 3), 0.8),
			]),
		);
		p.tracks.push(lead.clone());
		p.tracks.push(bus.clone());
		p.markers.push(Marker::new(Tick(PPQ * 8), "Verse"));
		p.automation.push(crate::AutomationLane::new(
			ParamAddress::track_volume(lead.id),
			Vec::new(),
		));

		let s = p.summary();
		assert_eq!(s.time_signature, "4/4");
		assert_eq!(s.length_beats, 12.0);
		assert_eq!(s.length_bars, 3.0);
		assert_eq!(s.markers[0].bar, 3.0);
		let t = &s.tracks[0];
		assert_eq!(t.output, bus.id.to_string());
		assert_eq!(t.instrument.as_ref().unwrap().kind, "builtin:subtractive");
		assert_eq!(t.clips[0].note_count, Some(3));
		assert_eq!(t.clips[0].pitch_range.as_deref(), Some("C3..G4"));
		assert_eq!(t.clips[0].start_beats, 4.0);
		assert_eq!(t.automation, vec![format!("track/{}/volume", lead.id)]);
		assert!(s.master.automation.is_empty());

		let json = serde_json::to_value(&s).unwrap();
		assert!(json["tracks"][0]["clips"][0].get("notes").is_none());
		assert!(json["tracks"][1].get("instrument").is_none());
	}
}
