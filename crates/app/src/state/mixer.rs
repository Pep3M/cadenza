//! Mixer view options (strip width, visible sections, meter mode; per user, not in the
//! project), the routing builders of its output menu (new bus, sidechain targets) and the
//! master strip's live loudness.

use crate::i18n::{tr, trf};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use cadenza_core::{DeviceId, TrackId};
use cadenza_engine::{DeviceProbe, ProbeFrame};
use cadenza_model::{
	ChannelPatch, Command, Device, DeviceKind, Document, Output, Project, SidechainSource,
	TrackKind,
};
use serde::{Deserialize, Serialize};

use super::loudness_meter::{LiveLoudness, Side};

use super::edits;

/// What the mixer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerView {
	/// Narrow strips (more channels on screen; names and inserts elided).
	pub narrow: bool,
	pub input: bool,
	pub inserts: bool,
	pub sends: bool,
	/// The output selector.
	pub routing: bool,
	pub meter: MeterMode,
	/// Normal strips did not fit the last frame: drawn narrow anyway (design 12).
	#[serde(skip)]
	pub auto_narrow: bool,
}

impl MixerView {
	/// Whether strips are drawn narrow (chosen, or because Normal ones do not fit).
	pub fn narrow_now(&self) -> bool {
		self.narrow || self.auto_narrow
	}
}

impl Default for MixerView {
	fn default() -> Self {
		Self {
			narrow: false,
			input: true,
			inserts: true,
			sends: true,
			routing: true,
			meter: MeterMode::default(),
			auto_narrow: false,
		}
	}
}

/// How strip meters draw their level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterMode {
	/// One bar to the peak level, coloured by zone.
	Peak,
	/// A bright RMS body under a dim peak bar topped by a peak line.
	#[default]
	PeakRms,
}

impl MeterMode {
	pub fn label(self) -> &'static str {
		match self {
			Self::Peak => tr("Peak"),
			Self::PeakRms => tr("Peak+RMS"),
		}
	}

	pub fn toggled(self) -> Self {
		match self {
			Self::Peak => Self::PeakRms,
			Self::PeakRms => Self::Peak,
		}
	}
}

/// What one meter channel draws, as dB tops (`None` = nothing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterBars {
	/// The bright, zone-coloured bar.
	pub body: Option<f32>,
	/// The dim bar behind it.
	pub ghost: Option<f32>,
	/// A 1 px line at the current peak.
	pub line: Option<f32>,
}

pub fn meter_bars(peak_db: f32, rms_db: f32, mode: MeterMode) -> MeterBars {
	let live = |db: f32| (db > super::meters::FLOOR_DB).then_some(db);
	match mode {
		MeterMode::Peak => MeterBars {
			body: live(peak_db),
			ghost: None,
			line: None,
		},
		MeterMode::PeakRms => MeterBars {
			body: live(rms_db),
			ghost: live(peak_db),
			line: live(peak_db),
		},
	}
}

/// "New bus from this track": a bus named after the track, right below it, with the
/// track's output routed to it — one undo step. `None` for buses, folders and unknown ids.
pub fn new_bus_from(project: &Project, track: TrackId) -> Option<(Command, TrackId)> {
	let t = project.track(track)?;
	if matches!(t.kind, TrackKind::Bus | TrackKind::Folder) {
		return None;
	}
	let mut bus = edits::new_track(project, TrackKind::Bus);
	bus.name = format!("{} Bus", t.name);
	bus.color = t.color.clone();
	// The bus goes where the track went before.
	bus.channel.output = t.channel.output;
	let id = bus.id;
	let commands = vec![
		edits::add_track(project, bus, Some(track)),
		Command::SetChannel {
			track: Some(track),
			patch: ChannelPatch {
				output: Some(Output::Bus(id)),
				..ChannelPatch::default()
			},
		},
	];
	Some((
		Command::Batch {
			label: format!("New bus from '{}'", t.name),
			commands,
		},
		id,
	))
}

/// A device on another strip that can take `source` as its sidechain key.
#[derive(Debug, Clone, PartialEq)]
pub struct SidechainTarget {
	/// The device's track (`None` = master insert).
	pub track: Option<TrackId>,
	pub device: DeviceId,
	/// "Sidechain → Bass · Compressor".
	pub label: String,
	/// The device is keyed by the source now (the menu shows it checked).
	pub keyed: bool,
}

/// Whether the built-in `id` takes a key input (computed once per process).
pub fn builtin_has_sidechain(id: &str) -> bool {
	static IDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
	IDS.get_or_init(|| {
		cadenza_dsp::registry()
			.iter()
			.filter(|info| cadenza_dsp::create(info.id).is_some_and(|p| p.has_sidechain()))
			.map(|info| info.id)
			.collect()
	})
	.contains(id)
}

/// Every device (instruments, inserts, master inserts) outside `source` that accepts a
/// key (`accepts`: built-ins via [`builtin_has_sidechain`], plugins by their scan) and
/// can be keyed by `source` without a routing cycle, in display order.
pub fn sidechain_targets(
	project: &Project,
	source: TrackId,
	accepts: impl Fn(&Device) -> bool,
) -> Vec<SidechainTarget> {
	let mut scratch: Option<Document> = None;
	let mut out = Vec::new();
	let strips = project
		.tracks
		.iter()
		.filter(|t| t.id != source && t.kind != TrackKind::Folder)
		.map(|t| {
			(
				Some(t.id),
				t.name.as_str(),
				t.instrument.iter().chain(&t.channel.inserts),
			)
		})
		.chain(std::iter::once((
			None,
			tr("Master"),
			None.iter().chain(&project.master.inserts),
		)));
	for (track, name, devices) in strips {
		for d in devices.filter(|d| accepts(d)) {
			let keyed = d.sidechain.is_some_and(|k| k.track == source);
			let valid = keyed || {
				let doc = scratch.get_or_insert_with(|| Document::new(project.clone()));
				let ok = doc.execute(key_command(track, d.id, Some(source))).is_ok();
				if ok {
					let _ = doc.undo();
				}
				ok
			};
			if valid {
				out.push(SidechainTarget {
					track,
					device: d.id,
					label: trf(
						"Sidechain → {track} · {device}",
						&[("track", &name), ("device", &d.name)],
					),
					keyed,
				});
			}
		}
	}
	out
}

fn key_command(track: Option<TrackId>, device: DeviceId, source: Option<TrackId>) -> Command {
	Command::SetSidechain {
		track,
		device,
		source: source.map(|track| SidechainSource {
			track,
			pre_fader: false,
		}),
	}
}

/// Choosing a target in the menu: keys it by `source` (post-fader, as heard), or removes
/// the key when it is already keyed by it. One undo step.
pub fn toggle_sidechain(target: &SidechainTarget, source: TrackId) -> Command {
	key_command(
		target.track,
		target.device,
		(!target.keyed).then_some(source),
	)
}

/// Whether a device accepts a key: built-ins by their processor, plugins by the scan
/// (unknown plugins are left out).
pub fn accepts_key(device: &Device, catalog: &cadenza_plugin_host::PluginCatalog) -> bool {
	match &device.kind {
		DeviceKind::Builtin { id } => builtin_has_sidechain(id),
		_ => cadenza_mcp::convert::plugin_ref_of(device, catalog)
			.is_some_and(|d| d.sidechain_channels > 0),
	}
}

/// Live loudness of the master strip, fed from the player's master tap on the UI thread.
#[derive(Debug)]
pub struct MasterLoudness {
	tap: Option<Arc<DeviceProbe>>,
	pub meter: LiveLoudness,
	frames: Vec<ProbeFrame>,
	silence: Vec<f32>,
}

impl Default for MasterLoudness {
	fn default() -> Self {
		Self {
			tap: None,
			meter: LiveLoudness::new(48_000),
			frames: Vec::new(),
			silence: Vec::new(),
		}
	}
}

impl MasterLoudness {
	/// The tap of a (re)opened player: a new rate starts a fresh measurement.
	pub fn attach(&mut self, tap: Arc<DeviceProbe>) {
		if tap.sample_rate() != self.meter.sample_rate() {
			self.meter = LiveLoudness::new(tap.sample_rate());
		}
		self.tap = Some(tap);
	}

	pub fn detach(&mut self) {
		self.tap = None;
	}

	/// Drains the tap into the meter. While stopped nothing renders, so `dt` seconds of
	/// silence go in instead: the short-term reading falls like a hardware meter's (the
	/// integrated one ignores it: silence is below the absolute gate).
	pub fn update(&mut self, playing: bool, dt: f32) {
		let Some(tap) = &self.tap else { return };
		self.frames.clear();
		tap.drain(&mut self.frames);
		if !self.frames.is_empty() {
			self.meter.push_frames(&self.frames, Side::Output);
		} else if !playing {
			let n = ((dt.clamp(0.0, 0.25)) * self.meter.sample_rate() as f32) as usize;
			if n > 0 && self.meter.short_term() > super::meters::FLOOR_DB {
				self.silence.resize(n, 0.0);
				self.meter.push(&self.silence[..n], &self.silence[..n]);
			}
		}
	}

	/// Integrated loudness and true peak start again (click on the readout).
	pub fn reset(&mut self) {
		self.meter.reset_integrated();
	}
}

/// "-14.2" or "-inf" for a LUFS / dBTP readout.
pub fn format_lufs(db: f32) -> String {
	if db <= super::meters::FLOOR_DB + 1.0 || !db.is_finite() {
		"-inf".to_owned()
	} else {
		format!("{db:.1}")
	}
}

#[cfg(test)]
mod tests {
	use cadenza_model::{Document, Track};

	use super::*;

	#[test]
	fn a_new_bus_takes_over_the_track_output() {
		let mut p = Project::new("Mix");
		let mut drums = Track::new("Drums", TrackKind::Instrument).with_color("#E8705F");
		let id = drums.id;
		let group = Track::new("Group", TrackKind::Bus);
		drums.channel.output = Output::Bus(group.id);
		let group_id = group.id;
		p.tracks.push(drums);
		p.tracks.push(group);
		let (cmd, bus) = new_bus_from(&p, id).unwrap();
		let mut doc = Document::new(p);
		doc.execute(cmd).unwrap();
		let p = doc.project();
		let b = p.track(bus).unwrap();
		assert_eq!(b.name, "Drums Bus");
		assert_eq!(b.kind, TrackKind::Bus);
		assert_eq!(p.track_index(bus), Some(1), "right below the track");
		assert_eq!(
			b.channel.output,
			Output::Bus(group_id),
			"keeps the old route"
		);
		assert_eq!(p.track(id).unwrap().channel.output, Output::Bus(bus));
		assert_eq!(doc.history_labels().len(), 1);
		assert!(new_bus_from(p, bus).is_none(), "not from a bus");
	}

	#[test]
	fn meter_modes_pick_their_bars() {
		let b = meter_bars(-6.0, -12.0, MeterMode::Peak);
		assert_eq!(
			(b.body, b.ghost, b.line),
			(Some(-6.0), None, None),
			"one bar to the peak"
		);
		let b = meter_bars(-6.0, -12.0, MeterMode::PeakRms);
		assert_eq!(
			(b.body, b.ghost, b.line),
			(Some(-12.0), Some(-6.0), Some(-6.0))
		);
		let floor = crate::state::meters::FLOOR_DB;
		let b = meter_bars(floor, floor, MeterMode::PeakRms);
		assert_eq!((b.body, b.ghost, b.line), (None, None, None), "silence");
		assert_eq!(MeterMode::Peak.toggled(), MeterMode::PeakRms);
		// Older prefs without the field keep the design default.
		let v: MixerView = serde_json::from_str(r#"{"narrow":true}"#).unwrap();
		assert_eq!(v.meter, MeterMode::PeakRms);
		let json = serde_json::to_string(&MixerView {
			meter: MeterMode::Peak,
			..MixerView::default()
		})
		.unwrap();
		assert!(json.contains(r#""meter":"peak""#), "{json}");
	}

	#[test]
	fn sidechain_targets_list_keyable_devices_and_toggle_the_key() {
		let mut p = Project::new("Mix");
		let kick = Track::new("Kick", TrackKind::Instrument);
		let mut bass = Track::new("Bass", TrackKind::Instrument);
		let comp = Device::builtin("compressor", "Compressor");
		let comp_id = comp.id;
		bass.channel.inserts.push(comp);
		bass.channel
			.inserts
			.push(Device::builtin("chorus", "Chorus"));
		// The bus the bass feeds cannot key the bass (cycle); the kick may key the bus.
		let mut bus = Track::new("Drum Bus", TrackKind::Bus);
		bus.channel
			.inserts
			.push(Device::builtin("compressor", "Bus Comp"));
		bass.channel.output = Output::Bus(bus.id);
		let bus_id = bus.id;
		let master_comp = Device::builtin("compressor", "Glue");
		p.master.inserts.push(master_comp);
		let (kick_id, bass_id) = (kick.id, bass.id);
		p.tracks.extend([kick, bass, bus]);

		assert!(builtin_has_sidechain("compressor"));
		assert!(builtin_has_sidechain("eq"), "dynamic EQ bands");
		assert!(builtin_has_sidechain("reverb"), "ducking key");
		assert!(!builtin_has_sidechain("chorus"));
		let accepts = |d: &Device| match &d.kind {
			DeviceKind::Builtin { id } => builtin_has_sidechain(id),
			_ => false,
		};
		let targets = sidechain_targets(&p, kick_id, accepts);
		let labels: Vec<&str> = targets.iter().map(|t| t.label.as_str()).collect();
		assert_eq!(
			labels,
			[
				"Sidechain → Bass · Compressor",
				"Sidechain → Drum Bus · Bus Comp",
				"Sidechain → Master · Glue"
			]
		);
		let from_bus: Vec<String> = sidechain_targets(&p, bus_id, accepts)
			.into_iter()
			.map(|t| t.label)
			.collect();
		assert_eq!(
			from_bus,
			["Sidechain → Master · Glue"],
			"no cycle through the bass"
		);
		assert!(targets.iter().all(|t| !t.keyed));

		let mut doc = Document::new(p);
		doc.execute(toggle_sidechain(&targets[0], kick_id)).unwrap();
		let d = doc.project().device(Some(bass_id), comp_id).unwrap();
		assert_eq!(
			d.sidechain.map(|k| (k.track, k.pre_fader)),
			Some((kick_id, false))
		);
		assert_eq!(doc.history_labels().len(), 1, "one undo step");
		let again = sidechain_targets(doc.project(), kick_id, accepts);
		assert!(again[0].keyed, "shown checked");
		doc.execute(toggle_sidechain(&again[0], kick_id)).unwrap();
		assert!(
			doc.project()
				.device(Some(bass_id), comp_id)
				.unwrap()
				.sidechain
				.is_none(),
			"choosing it again removes the key"
		);
		// The bass is not a target of itself.
		assert!(
			sidechain_targets(doc.project(), bass_id, accepts)
				.iter()
				.all(|t| t.track != Some(bass_id))
		);
	}

	#[test]
	fn lufs_readouts_and_an_unattached_meter() {
		assert_eq!(format_lufs(-14.24), "-14.2");
		assert_eq!(format_lufs(crate::state::meters::FLOOR_DB), "-inf");
		let mut m = MasterLoudness::default();
		m.update(false, 0.1);
		assert_eq!(format_lufs(m.meter.short_term()), "-inf");
		m.reset();
		assert!(m.meter.is_empty());
	}
}
