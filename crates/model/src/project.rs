//! Project document data types and read-only queries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cadenza_core::{
	ClipId, DeviceId, LinkId, Meter, MeterChange, NoteId, ParamAddress, TakeGroupId, TempoMap,
	Tick, TimeSignature, TrackId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::controller::{Controller, ControllerLane};
use crate::modulation::Modulator;
use crate::sample::SampleZone;

/// Current on-disk format version written by [`Project::save_json`].
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
	pub format_version: u32,
	pub name: String,
	pub tempo: TempoMap,
	/// Time signature of bar 1.
	pub time_signature: TimeSignature,
	/// Later time signatures, sorted by bar (0-based index >= 1, one per bar). Missing in
	/// older files (= one signature for the whole song).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub time_signature_changes: Vec<MeterChange>,
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
	/// What the song is (its style guide); missing in older files.
	#[serde(default, skip_serializing_if = "ProjectMeta::is_empty")]
	pub meta: ProjectMeta,
	/// Tracks armed for recording, in arming order (the last armed instrument track plays
	/// the live MIDI input). Saved with the song; missing in older files.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub armed: Vec<TrackId>,
	/// Hardware MIDI controller assignments (MIDI learn); missing in older files.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub midi_map: Vec<crate::midi_map::MidiMapping>,
}

/// Song-level metadata that has no effect on the sound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectMeta {
	/// Library style guide the song follows ("core/latin_pop"), so any session can re-read
	/// the plan with `library_get`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub style: Option<String>,
}

impl ProjectMeta {
	pub fn is_empty(&self) -> bool {
		self.style.is_none()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
	Instrument,
	Audio,
	Bus,
	/// Holds other tracks (their `folder`): no audio of its own; its volume is an offset
	/// for every track inside (VCA-like), its mute/solo apply to them.
	Folder,
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
	/// Audio tracks: the audio input it records from and when it plays that input
	/// (missing = no input assigned).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub input: Option<TrackInput>,
	/// The folder track this track is in (missing = top level).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub folder: Option<TrackId>,
	/// Frozen: the track plays this render (instrument, clips and inserts, before the
	/// fader) instead of computing them; its devices are not loaded.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub freeze: Option<Box<Freeze>>,
}

/// A frozen track's render: a stereo file that starts at the song start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Freeze {
	pub file: PathBuf,
	/// Fingerprint of what the render was made from ([`Project::freeze_source`]); a
	/// different one now means the track was edited after freezing. Optional in JSON.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
}

/// Which channels of the audio input device an audio track records, and when the track
/// plays its input (through its inserts) instead of its clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TrackInput {
	/// First input channel, 0-based (0 = input 1 / left).
	pub channel: u16,
	/// `true`: channels `channel` and `channel + 1` as a stereo pair; `false`: mono.
	#[serde(default)]
	pub stereo: bool,
	#[serde(default)]
	pub monitor: MonitorMode,
}

impl TrackInput {
	/// Mono input on `channel`.
	pub fn mono(channel: u16) -> Self {
		Self {
			channel,
			stereo: false,
			monitor: MonitorMode::default(),
		}
	}

	/// Stereo pair starting at `channel`.
	pub fn stereo(channel: u16) -> Self {
		Self {
			channel,
			stereo: true,
			monitor: MonitorMode::default(),
		}
	}

	/// Input channels used (one or two).
	pub fn channels(&self) -> std::ops::RangeInclusive<u16> {
		self.channel..=self.channel + u16::from(self.stereo)
	}

	/// "In 1", "In 1+2".
	pub fn label(&self) -> String {
		if self.stereo {
			format!("In {}+{}", self.channel + 1, self.channel + 2)
		} else {
			format!("In {}", self.channel + 1)
		}
	}
}

/// When an audio track plays its live input (input monitoring).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMode {
	/// Never: the track plays its clips (monitor through the audio interface instead).
	Off,
	/// While the track is armed and the transport is stopped or recording.
	#[default]
	Auto,
	/// Always (the input replaces the track's clips).
	On,
}

impl MonitorMode {
	pub fn name(self) -> &'static str {
		match self {
			Self::Off => "off",
			Self::Auto => "auto",
			Self::On => "on",
		}
	}
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
	/// Sample-based built-ins (`sampler`, `drum_sampler`): the samples and where they
	/// play. Replaced as a whole by `Command::SetSampleZones`.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub zones: Vec<SampleZone>,
	/// Built-ins only: LFOs / envelope followers moving params around their values.
	/// Replaced as a whole by `Command::SetModulators`.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub modulators: Vec<Modulator>,
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
	/// Set when the clip is one of several alternative takes of a region (loop
	/// recording, comping); missing = an ordinary clip that always plays.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub take: Option<Take>,
	/// A muted clip stays on the track but is never heard (FL playlist mute).
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub muted: bool,
	/// Linked MIDI clips (FL pattern instances): every clip sharing a link holds the same
	/// notes and controller lanes, and note/controller commands on one edit them all.
	/// Lengths may differ: a shorter copy simply does not reach the later notes.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub link: Option<LinkId>,
}

/// A clip's membership in a group of alternative takes. Takes of a group are stacked
/// over the same stretch of the track; only the active one plays, the others stay
/// available for comping (pick another take, or split the group and pick per part).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Take {
	pub group: TakeGroupId,
	/// Take lane, 0 = the first take (oldest), for the expanded takes view.
	pub lane: u32,
	/// Whether this take plays. Commands keep exactly one active take per group.
	#[serde(default)]
	pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipContent {
	/// Notes kept sorted by (start, pitch, id); controller lanes (CC, pitch bend,
	/// pressure) sorted by controller, one per controller, missing in older files.
	Midi {
		notes: Vec<Note>,
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		controllers: Vec<ControllerLane>,
	},
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
		/// Notes detected in the recording and their edits (missing = none).
		#[serde(default, skip_serializing_if = "Option::is_none")]
		notes: Option<Box<crate::audio_notes::NoteEdits>>,
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
			time_signature_changes: Vec::new(),
			tracks: Vec::new(),
			master: Channel::default(),
			automation: Vec::new(),
			markers: Vec::new(),
			meta: ProjectMeta::default(),
			armed: Vec::new(),
			midi_map: Vec::new(),
		}
	}

	/// The song's time signatures (bar 1 plus changes) for bar/beat math.
	pub fn meter(&self) -> Meter<'_> {
		Meter::new(self.time_signature, &self.time_signature_changes)
	}

	/// The folders holding `track`, innermost first.
	pub fn folders_of(&self, track: TrackId) -> Vec<TrackId> {
		let mut out = Vec::new();
		let mut at = self.track(track).and_then(|t| t.folder);
		while let Some(f) = at {
			if out.contains(&f) || out.len() > self.tracks.len() {
				break;
			}
			out.push(f);
			at = self.track(f).and_then(|t| t.folder);
		}
		out
	}

	/// Tracks inside `folder`, at any depth, in display order.
	pub fn folder_contents(&self, folder: TrackId) -> Vec<TrackId> {
		self.tracks
			.iter()
			.filter(|t| t.id != folder && self.folders_of(t.id).contains(&folder))
			.map(|t| t.id)
			.collect()
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

	/// Every clip sharing `link` (linked copies), in track then time order.
	pub fn linked_clips(&self, link: LinkId) -> impl Iterator<Item = &Clip> {
		self.tracks
			.iter()
			.flat_map(|t| t.clips.iter())
			.filter(move |c| c.link == Some(link))
	}

	/// The clips linked to `clip` (itself included); just `clip` when it is not linked.
	pub fn link_siblings(&self, clip: ClipId) -> Vec<ClipId> {
		match self.find_clip(clip).and_then(|(_, c)| c.link) {
			Some(link) => self.linked_clips(link).map(|c| c.id).collect(),
			None => vec![clip],
		}
	}

	/// How far the shared content of `link` may reach: the longest linked clip.
	pub fn link_span(&self, link: LinkId) -> Tick {
		self.linked_clips(link)
			.map(|c| c.length)
			.max()
			.unwrap_or(Tick::ZERO)
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
	/// Audio and instrument tracks can be armed for recording.
	pub fn can_record(&self) -> bool {
		matches!(self.kind, TrackKind::Audio | TrackKind::Instrument)
	}

	pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
		Self {
			id: TrackId::new(),
			name: name.into(),
			kind,
			color: None,
			instrument: None,
			clips: Vec::new(),
			channel: Channel::default(),
			input: None,
			folder: None,
			freeze: None,
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
			zones: Vec::new(),
			modulators: Vec::new(),
		}
	}

	pub fn with_zones(mut self, zones: Vec<SampleZone>) -> Self {
		self.zones = zones;
		self
	}

	/// The built-in registry id, `None` for plugins.
	pub fn builtin_id(&self) -> Option<&str> {
		match &self.kind {
			DeviceKind::Builtin { id } => Some(id),
			_ => None,
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
			content: ClipContent::Midi {
				notes: Vec::new(),
				controllers: Vec::new(),
			},
			take: None,
			muted: false,
			link: None,
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
				notes: None,
			},
			take: None,
			muted: false,
			link: None,
		}
	}

	/// Adds notes keeping them sorted; intended for building a clip before it enters a project.
	pub fn with_notes(mut self, notes: impl IntoIterator<Item = Note>) -> Self {
		if let ClipContent::Midi {
			notes: existing, ..
		} = &mut self.content
		{
			existing.extend(notes);
			sort_notes(existing);
		}
		self
	}

	pub fn end(&self) -> Tick {
		self.start + self.length
	}

	/// Whether two clips hold the same MIDI content (notes and controller lanes), the
	/// invariant of linked clips.
	pub fn same_midi_content(&self, other: &Clip) -> bool {
		self.is_midi() && other.is_midi() && self.content == other.content
	}

	/// Notes of a MIDI clip; empty for audio clips.
	pub fn notes(&self) -> &[Note] {
		match &self.content {
			ClipContent::Midi { notes, .. } => notes,
			ClipContent::Audio { .. } => &[],
		}
	}

	/// Controller lanes of a MIDI clip; empty for audio clips.
	pub fn controllers(&self) -> &[ControllerLane] {
		match &self.content {
			ClipContent::Midi { controllers, .. } => controllers,
			ClipContent::Audio { .. } => &[],
		}
	}

	/// The lane of `controller`, if the clip has one.
	pub fn controller(&self, controller: Controller) -> Option<&ControllerLane> {
		self.controllers()
			.iter()
			.find(|l| l.controller == controller)
	}

	/// Adds controller lanes (replacing lanes of the same controller); intended for
	/// building a clip before it enters a project.
	pub fn with_controllers(mut self, lanes: impl IntoIterator<Item = ControllerLane>) -> Self {
		if let ClipContent::Midi { controllers, .. } = &mut self.content {
			for mut lane in lanes {
				lane.points.sort_by_key(|p| p.tick);
				lane.points.dedup_by_key(|p| p.tick);
				controllers.retain(|l| l.controller != lane.controller);
				controllers.push(lane);
			}
			controllers.sort_by_key(|l| l.controller);
		}
		self
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

	/// Whether the clip is heard: ordinary clips always, takes only when active.
	pub fn plays(&self) -> bool {
		self.take.is_none_or(|t| t.active)
	}

	/// Whether the engine renders it: it plays (see [`Clip::plays`]) and is not muted.
	pub fn sounds(&self) -> bool {
		self.plays() && !self.muted
	}

	/// The take group this clip belongs to.
	pub fn take_group(&self) -> Option<TakeGroupId> {
		self.take.map(|t| t.group)
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
