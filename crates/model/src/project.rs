//! Project document data types and read-only queries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cadenza_core::{
	ClipId, DeviceId, NoteId, ParamAddress, TempoMap, Tick, TimeSignature, TrackId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Current on-disk format version written by [`Project::save_json`].
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
	pub format_version: u32,
	pub name: String,
	pub tempo: TempoMap,
	pub time_signature: TimeSignature,
	/// Display order.
	#[serde(default)]
	pub tracks: Vec<Track>,
	#[serde(default)]
	pub master: Channel,
	/// Kept sorted by address string; at most one lane per address.
	#[serde(default)]
	pub automation: Vec<AutomationLane>,
	/// Song sections (Intro, Verse 1, Chorus...). Kept sorted by tick, one per tick.
	#[serde(default)]
	pub markers: Vec<Marker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
	Instrument,
	Audio,
	Bus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
	pub id: TrackId,
	pub name: String,
	pub kind: TrackKind,
	/// `#RRGGBB`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub color: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub instrument: Option<Device>,
	/// Kept sorted by (start, id).
	#[serde(default)]
	pub clips: Vec<Clip>,
	#[serde(default)]
	pub channel: Channel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Channel {
	pub volume_db: f32,
	/// -1 (left) ..= 1 (right).
	pub pan: f32,
	pub mute: bool,
	pub solo: bool,
	/// Effect chain in processing order.
	pub inserts: Vec<Device>,
	pub sends: Vec<Send>,
	pub output: Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Output {
	#[default]
	Master,
	Bus(TrackId),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Send {
	pub id: DeviceId,
	/// Must be a Bus track.
	pub target: TrackId,
	pub level_db: f32,
	#[serde(default)]
	pub pre_fader: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Device {
	pub id: DeviceId,
	pub kind: DeviceKind,
	pub name: String,
	#[serde(default)]
	pub bypass: bool,
	/// Plain (unnormalized) values by param id; missing params use the device default.
	#[serde(default)]
	pub params: BTreeMap<String, f32>,
	/// Opaque plugin state, base64. Explicit `params` are applied on top of it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub state: Option<String>,
	/// Track feeding the device's sidechain (key) input, e.g. a kick ducking a bass
	/// compressor. Missing = no sidechain.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sidechain: Option<SidechainSource>,
	/// Instruments only: extra output ports to mix in (default: only the main output).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub aux_outputs: Vec<OutputRoute>,
}

/// Where a device's sidechain signal comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SidechainSource {
	/// Any track other than the device's own; it must not (directly or through buses and
	/// other sidechains) be fed by the device's track.
	pub track: TrackId,
	/// `true`: the source's signal after its inserts, before fader/pan/mute (a muted
	/// "ghost" kick still keys). `false` (default): what the source sends to its output.
	#[serde(default)]
	pub pre_fader: bool,
}

/// One extra output port of a multi-output instrument (e.g. a drum sampler's separate
/// kick/snare outputs) and where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputRoute {
	/// Extra output port, 1-based (port 0 is the main output, always mixed into the track).
	pub port: u32,
	/// `null`: mixed into the instrument's own track (before its inserts). A Bus track id:
	/// sent there at unity gain instead, to get its own inserts and fader.
	#[serde(default)]
	pub bus: Option<TrackId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceKind {
	Builtin { id: String },
	Clap { plugin_id: String, path: PathBuf },
	Vst3 { class_id: String, path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Clip {
	pub id: ClipId,
	pub name: String,
	pub start: Tick,
	pub length: Tick,
	pub content: ClipContent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipContent {
	/// Notes kept sorted by (start, pitch, id).
	Midi { notes: Vec<Note> },
	Audio {
		file: PathBuf,
		offset_seconds: f64,
		gain_db: f32,
		/// Fade at the clip start (missing = none).
		#[serde(default, skip_serializing_if = "Option::is_none")]
		fade_in: Option<ClipFade>,
		/// Fade at the clip end (missing = none).
		#[serde(default, skip_serializing_if = "Option::is_none")]
		fade_out: Option<ClipFade>,
	},
}

/// A clip fade. Its length is in seconds, not beats: like `offset_seconds` it shapes the
/// recording itself (de-click, natural decay), so a tempo change must not stretch it.
/// AI-facing tools also accept beats and convert at the clip position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClipFade {
	/// Fade length in seconds (> 0).
	pub seconds: f64,
	#[serde(default)]
	pub curve: FadeCurve,
}

impl ClipFade {
	pub fn new(seconds: f64, curve: FadeCurve) -> Self {
		Self { seconds, curve }
	}
}

/// Shape of a clip fade.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
	/// Straight gain ramp; crossfades of correlated material keep their level.
	#[default]
	Linear,
	/// Sine/cosine quarter wave; crossfades of unrelated material keep their loudness.
	EqualPower,
	/// Even-sounding in dB (60 dB range); best for long fade-outs.
	Exponential,
	/// Slow start and end (raised cosine).
	SCurve,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Note {
	pub id: NoteId,
	/// MIDI pitch 0..=127 (60 = C4).
	pub pitch: u8,
	/// 0..=1.
	pub velocity: f32,
	/// Relative to the clip start.
	pub start: Tick,
	pub length: Tick,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutomationLane {
	pub address: ParamAddress,
	/// Kept sorted by tick, one point per tick.
	pub points: Vec<AutomationPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutomationPoint {
	pub tick: Tick,
	pub value: f32,
	/// How the value travels from this point to the next one (default: linear). Missing
	/// in older files, so `format_version` stays 1.
	#[serde(default, skip_serializing_if = "PointShape::is_linear")]
	pub shape: PointShape,
}

/// Shape of the automation segment that starts at a point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PointShape {
	/// Straight line in time to the next point.
	#[default]
	Linear,
	/// Keeps this value until the next point, then jumps (steps).
	Hold,
	/// Slow start and end (raised cosine), for smooth sweeps.
	Smooth,
}

impl PointShape {
	pub fn is_linear(&self) -> bool {
		*self == Self::Linear
	}

	/// Value at `t` (0..=1) of the way from `from` to `to`.
	pub fn interpolate(self, from: f32, to: f32, t: f64) -> f32 {
		let t = t.clamp(0.0, 1.0);
		let k = match self {
			Self::Linear => t,
			Self::Hold => 0.0,
			Self::Smooth => 0.5 - 0.5 * (std::f64::consts::PI * t).cos(),
		};
		(f64::from(from) + f64::from(to - from) * k) as f32
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Marker {
	pub tick: Tick,
	pub name: String,
}

impl Project {
	/// Empty project at 120 bpm, 4/4, master at 0 dB centered.
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			format_version: FORMAT_VERSION,
			name: name.into(),
			tempo: TempoMap::constant(120.0),
			time_signature: TimeSignature::default(),
			tracks: Vec::new(),
			master: Channel::default(),
			automation: Vec::new(),
			markers: Vec::new(),
		}
	}

	pub fn track(&self, id: TrackId) -> Option<&Track> {
		self.tracks.iter().find(|t| t.id == id)
	}

	pub(crate) fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
		self.tracks.iter_mut().find(|t| t.id == id)
	}

	pub fn track_index(&self, id: TrackId) -> Option<usize> {
		self.tracks.iter().position(|t| t.id == id)
	}

	pub fn find_clip(&self, id: ClipId) -> Option<(&Track, &Clip)> {
		self.tracks
			.iter()
			.find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t, c)))
	}

	pub(crate) fn find_clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
		self.tracks
			.iter_mut()
			.flat_map(|t| t.clips.iter_mut())
			.find(|c| c.id == id)
	}

	/// Channel of a track, or the master channel for `None`.
	pub fn channel(&self, track: Option<TrackId>) -> Option<&Channel> {
		match track {
			Some(id) => self.track(id).map(|t| &t.channel),
			None => Some(&self.master),
		}
	}

	pub(crate) fn channel_mut(&mut self, track: Option<TrackId>) -> Option<&mut Channel> {
		match track {
			Some(id) => self.track_mut(id).map(|t| &mut t.channel),
			None => Some(&mut self.master),
		}
	}

	/// Instrument or insert `device` on `track` (`None` = master inserts).
	pub fn device(&self, track: Option<TrackId>, device: DeviceId) -> Option<&Device> {
		match track {
			Some(id) => self.track(id)?.device(device),
			None => self.master.inserts.iter().find(|d| d.id == device),
		}
	}

	pub(crate) fn device_mut(
		&mut self,
		track: Option<TrackId>,
		device: DeviceId,
	) -> Option<&mut Device> {
		match track {
			Some(id) => {
				let track = self.track_mut(id)?;
				track
					.instrument
					.iter_mut()
					.chain(track.channel.inserts.iter_mut())
					.find(|d| d.id == device)
			}
			None => self.master.inserts.iter_mut().find(|d| d.id == device),
		}
	}

	pub fn automation_lane(&self, address: &ParamAddress) -> Option<&AutomationLane> {
		self.automation.iter().find(|l| &l.address == address)
	}

	/// End of the last clip or marker (whichever is later).
	pub fn end_tick(&self) -> Tick {
		let clips = self
			.tracks
			.iter()
			.flat_map(|t| t.clips.iter())
			.map(Clip::end);
		let markers = self.markers.iter().map(|m| m.tick);
		clips.chain(markers).max().unwrap_or(Tick::ZERO)
	}

	/// Whether any device or send in the project already uses `id`.
	pub(crate) fn device_id_in_use(&self, id: DeviceId) -> bool {
		let channel_uses = |c: &Channel| {
			c.inserts.iter().any(|d| d.id == id) || c.sends.iter().any(|s| s.id == id)
		};
		channel_uses(&self.master)
			|| self.tracks.iter().any(|t| {
				t.instrument.as_ref().is_some_and(|d| d.id == id) || channel_uses(&t.channel)
			})
	}

	pub(crate) fn clip_id_in_use(&self, id: ClipId) -> bool {
		self.find_clip(id).is_some()
	}
}

impl Default for Channel {
	fn default() -> Self {
		Self {
			volume_db: 0.0,
			pan: 0.0,
			mute: false,
			solo: false,
			inserts: Vec::new(),
			sends: Vec::new(),
			output: Output::Master,
		}
	}
}

impl Track {
	/// New track with a fresh id, default channel and no content.
	pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
		Self {
			id: TrackId::new(),
			name: name.into(),
			kind,
			color: None,
			instrument: None,
			clips: Vec::new(),
			channel: Channel::default(),
		}
	}

	pub fn with_instrument(mut self, device: Device) -> Self {
		self.instrument = Some(device);
		self
	}

	pub fn with_color(mut self, color: impl Into<String>) -> Self {
		self.color = Some(color.into());
		self
	}

	/// Instrument (first) and inserts, in processing order.
	pub fn devices(&self) -> impl Iterator<Item = &Device> {
		self.instrument.iter().chain(self.channel.inserts.iter())
	}

	/// Instrument or insert with this id.
	pub fn device(&self, id: DeviceId) -> Option<&Device> {
		self.instrument
			.iter()
			.chain(self.channel.inserts.iter())
			.find(|d| d.id == id)
	}

	pub fn clip(&self, id: ClipId) -> Option<&Clip> {
		self.clips.iter().find(|c| c.id == id)
	}

	pub(crate) fn sort_clips(&mut self) {
		self.clips.sort_by_key(|c| (c.start, c.id));
	}
}

impl Send {
	pub fn new(target: TrackId, level_db: f32) -> Self {
		Self {
			id: DeviceId::new(),
			target,
			level_db,
			pre_fader: false,
		}
	}
}

impl Device {
	/// Built-in device (`cadenza-dsp` registry id) with a fresh id and default params.
	pub fn builtin(id: impl Into<String>, name: impl Into<String>) -> Self {
		Self {
			id: DeviceId::new(),
			kind: DeviceKind::Builtin { id: id.into() },
			name: name.into(),
			bypass: false,
			params: BTreeMap::new(),
			state: None,
			sidechain: None,
			aux_outputs: Vec::new(),
		}
	}

	pub fn with_param(mut self, param: impl Into<String>, value: f32) -> Self {
		self.params.insert(param.into(), value);
		self
	}

	pub fn with_sidechain(mut self, track: TrackId, pre_fader: bool) -> Self {
		self.sidechain = Some(SidechainSource { track, pre_fader });
		self
	}

	/// Plugin device (`Clap`/`Vst3`), i.e. not built-in.
	pub fn is_plugin(&self) -> bool {
		!matches!(self.kind, DeviceKind::Builtin { .. })
	}
}

impl Clip {
	/// Empty MIDI clip with a fresh id.
	pub fn midi(name: impl Into<String>, start: Tick, length: Tick) -> Self {
		Self {
			id: ClipId::new(),
			name: name.into(),
			start,
			length,
			content: ClipContent::Midi { notes: Vec::new() },
		}
	}

	/// Audio clip with a fresh id, no offset and unity gain.
	pub fn audio(
		name: impl Into<String>,
		start: Tick,
		length: Tick,
		file: impl Into<PathBuf>,
	) -> Self {
		Self {
			id: ClipId::new(),
			name: name.into(),
			start,
			length,
			content: ClipContent::Audio {
				file: file.into(),
				offset_seconds: 0.0,
				gain_db: 0.0,
				fade_in: None,
				fade_out: None,
			},
		}
	}

	/// Adds notes keeping them sorted; intended for building a clip before it enters a project.
	pub fn with_notes(mut self, notes: impl IntoIterator<Item = Note>) -> Self {
		if let ClipContent::Midi { notes: existing } = &mut self.content {
			existing.extend(notes);
			sort_notes(existing);
		}
		self
	}

	pub fn end(&self) -> Tick {
		self.start + self.length
	}

	/// Notes of a MIDI clip; empty for audio clips.
	pub fn notes(&self) -> &[Note] {
		match &self.content {
			ClipContent::Midi { notes } => notes,
			ClipContent::Audio { .. } => &[],
		}
	}

	/// Clip length in seconds under `tempo`.
	pub fn duration_seconds(&self, tempo: &TempoMap) -> f64 {
		tempo.tick_to_seconds(self.end()) - tempo.tick_to_seconds(self.start)
	}

	/// `(fade_in, fade_out)` of an audio clip; `(None, None)` for MIDI clips.
	pub fn fades(&self) -> (Option<ClipFade>, Option<ClipFade>) {
		match &self.content {
			ClipContent::Audio {
				fade_in, fade_out, ..
			} => (*fade_in, *fade_out),
			ClipContent::Midi { .. } => (None, None),
		}
	}

	pub fn is_midi(&self) -> bool {
		matches!(self.content, ClipContent::Midi { .. })
	}
}

pub(crate) fn sort_notes(notes: &mut [Note]) {
	notes.sort_by_key(|n| (n.start, n.pitch, n.id));
}

impl Note {
	/// Note with a fresh id; `start` is relative to the clip.
	pub fn new(pitch: u8, start: Tick, length: Tick, velocity: f32) -> Self {
		Self {
			id: NoteId::new(),
			pitch,
			velocity,
			start,
			length,
		}
	}

	pub fn end(&self) -> Tick {
		self.start + self.length
	}
}

impl AutomationLane {
	pub fn new(address: ParamAddress, points: Vec<AutomationPoint>) -> Self {
		Self { address, points }
	}

	/// Value of the lane at `tick` (see [`automation_value`]); `None` without points.
	pub fn value_at(&self, tick: Tick, tempo: &TempoMap) -> Option<f32> {
		automation_value(&self.points, tick, tempo)
	}
}

/// Value of sorted automation `points` at `tick`: held before the first and after the
/// last point, and shaped by each point's [`PointShape`] in between. Interpolation runs
/// in seconds (through `tempo`), exactly like the engine, which interpolates in samples.
pub fn automation_value(points: &[AutomationPoint], tick: Tick, tempo: &TempoMap) -> Option<f32> {
	let i = points.partition_point(|p| p.tick <= tick);
	let first = points.first()?;
	if i == 0 {
		return Some(first.value);
	}
	let a = points[i - 1];
	let Some(b) = points.get(i) else {
		return Some(a.value);
	};
	let s0 = tempo.tick_to_seconds(a.tick);
	let span = tempo.tick_to_seconds(b.tick) - s0;
	let t = if span > 0.0 {
		(tempo.tick_to_seconds(tick) - s0) / span
	} else {
		1.0
	};
	Some(a.shape.interpolate(a.value, b.value, t))
}

impl AutomationPoint {
	pub fn new(tick: Tick, value: f32) -> Self {
		Self {
			tick,
			value,
			shape: PointShape::Linear,
		}
	}

	pub fn with_shape(mut self, shape: PointShape) -> Self {
		self.shape = shape;
		self
	}
}

impl Marker {
	pub fn new(tick: Tick, name: impl Into<String>) -> Self {
		Self {
			tick,
			name: name.into(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;

	#[test]
	fn new_project_defaults() {
		let p = Project::new("Song");
		assert_eq!(p.format_version, FORMAT_VERSION);
		assert_eq!(p.tempo.bpm_at(Tick::ZERO), 120.0);
		assert_eq!(
			p.time_signature,
			TimeSignature {
				numerator: 4,
				denominator: 4
			}
		);
		assert_eq!(p.master.volume_db, 0.0);
		assert_eq!(p.master.pan, 0.0);
		assert_eq!(p.master.output, Output::Master);
		assert_eq!(p.end_tick(), Tick::ZERO);
	}

	#[test]
	fn queries() {
		let mut p = Project::new("Song");
		let synth = Device::builtin("subtractive", "Synth");
		let eq = Device::builtin("eq", "EQ");
		let master_comp = Device::builtin("compressor", "Glue");
		let mut track = Track::new("Lead", TrackKind::Instrument).with_instrument(synth.clone());
		track.channel.inserts.push(eq.clone());
		let clip = Clip::midi("A", Tick(PPQ * 4), Tick(PPQ * 8));
		track.clips.push(clip.clone());
		p.tracks.push(track.clone());
		p.master.inserts.push(master_comp.clone());
		p.markers.push(Marker::new(Tick(PPQ * 32), "Outro"));

		assert_eq!(p.track(track.id).unwrap().name, "Lead");
		let (t, c) = p.find_clip(clip.id).unwrap();
		assert_eq!((t.id, c.id), (track.id, clip.id));
		assert_eq!(p.device(Some(track.id), synth.id), Some(&synth));
		assert_eq!(p.device(Some(track.id), eq.id), Some(&eq));
		assert_eq!(p.device(None, master_comp.id), Some(&master_comp));
		assert_eq!(p.device(None, eq.id), None);
		assert_eq!(p.end_tick(), Tick(PPQ * 32));
		p.markers.clear();
		assert_eq!(p.end_tick(), Tick(PPQ * 12));
		assert!(p.device_id_in_use(eq.id));
		assert!(!p.device_id_in_use(DeviceId::new()));
	}
}
