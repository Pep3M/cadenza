//! Pure logic of the Sampler editor (`sampler`, design `17 Instruments`, 17e): the zone
//! map (keys across, velocity up) with its zoomable window and the zone <-> box mapping,
//! the zone gestures (move, resize an edge, root key, nudge) and the menu builders (fit
//! to neighbours, split velocity, duplicate, auto-map by file name, a new zone in an
//! empty spot), the loop readout and crossfade ramp, and the global section's parameter
//! bindings (amp / filter envelopes, the filter node, the bottom row's readouts). Every
//! builder returns one command: one undo step per gesture.

use std::path::{Path, PathBuf};

use cadenza_dsp::{DeviceInfo, ParamInfo};
use cadenza_model::{Command, LoopMode, Project, SampleZone};

use super::device::KnobSpec;
use super::device_display::{AdsrRanges, AdsrStage, AdsrValues, short_hz};
use super::drum_sampler_editor::{labeled, ms_label};
use super::samples::{self as sm, Marker, device};
use super::selection::DeviceRef;

pub const GAIN: &str = "gain";
pub const POLYPHONY: &str = "polyphony";
pub const MODE: &str = "mode";
pub const VELOCITY: &str = "velocity_sensitivity";
pub const VEL_CURVE: &str = "velocity_curve";
pub const FILTER_TYPE: &str = "filter_type";
pub const CUTOFF: &str = "filter_cutoff";
pub const RESONANCE: &str = "filter_resonance";
pub const ENV_AMOUNT: &str = "filter_env_amount";
pub const FILTER_VELOCITY: &str = "filter_velocity";
pub const KEY_TRACK: &str = "filter_key_track";
pub const GLIDE: &str = "glide";
pub const LEGATO: &str = "legato";
pub const TRANSPOSE: &str = "transpose";

/// The filter types in parameter order.
pub const FILTER_TYPES: [&str; 4] = ["Off", "LP", "HP", "BP"];
/// The filter display draws the response at this rate (the engine default).
pub const DISPLAY_RATE: f32 = 48_000.0;
/// A full-height drag on the filter display sweeps resonance by this much (%).
pub const RES_PER_DB: f32 = 100.0 / 48.0;
/// The velocity curve's presets (word, value in %).
pub const CURVES: [(&str, f32); 3] = [("soft", -50.0), ("linear", 0.0), ("hard", 50.0)];
/// The zones' colours: slots of `theme::BAND_COLORS` (blue is left to loops and the
/// selection).
pub const PALETTE: [usize; 7] = [0, 1, 2, 3, 4, 6, 7];

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("sampler").expect("the sampler is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a sampler param")
}

pub fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

pub fn default(id: &str) -> f32 {
	param(id).default
}

// --- Velocity steps and labels -------------------------------------------------------

/// A zone velocity (0..=1) as a MIDI velocity step (0..=127).
pub fn vel_step(v: f32) -> u8 {
	(v * 127.0).round().clamp(0.0, 127.0) as u8
}

/// A MIDI velocity step as a zone velocity.
pub fn step_vel(n: u8) -> f32 {
	f32::from(n.min(127)) / 127.0
}

fn vel_range(z: &SampleZone) -> (u8, u8) {
	(vel_step(z.vel_low), vel_step(z.vel_high))
}

/// "C3–B3" (one key: "C3"; 60 = C4 like the rest of the app).
pub fn key_range_label(z: &SampleZone) -> String {
	if z.key_low == z.key_high {
		cadenza_model::note_name(z.key_low)
	} else {
		format!(
			"{}\u{2013}{}",
			cadenza_model::note_name(z.key_low),
			cadenza_model::note_name(z.key_high)
		)
	}
}

/// "80–127".
pub fn vel_range_label(z: &SampleZone) -> String {
	let (lo, hi) = vel_range(z);
	format!("{lo}\u{2013}{hi}")
}

/// "C3–B3 · vel 80–127".
pub fn zone_title(z: &SampleZone) -> String {
	format!("{} \u{b7} vel {}", key_range_label(z), vel_range_label(z))
}

/// A colour slot per zone: zones sharing a key range (velocity layers) share a colour,
/// the ranges take the palette in key order.
pub fn zone_colors(zones: &[SampleZone]) -> Vec<usize> {
	let mut ranges: Vec<(u8, u8)> = zones.iter().map(|z| (z.key_low, z.key_high)).collect();
	ranges.sort_unstable();
	ranges.dedup();
	zones
		.iter()
		.map(|z| {
			let rank = ranges
				.iter()
				.position(|r| *r == (z.key_low, z.key_high))
				.unwrap_or(0);
			PALETTE[rank % PALETTE.len()]
		})
		.collect()
}

// --- The zone map's window ------------------------------------------------------------

/// Keys a fresh map shows at least (six octaves).
pub const DEFAULT_KEYS: f32 = 72.0;
const MIN_KEYS: f32 = 12.0;
const MIN_VELS: f32 = 16.0;
const ALL: f32 = 128.0;

/// A box in unit coordinates of the map (0..1, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitBox {
	pub left: f32,
	pub right: f32,
	pub top: f32,
	pub bottom: f32,
}

/// The stretch of keys (x) and velocities (y, up) the zone map shows; key `k` covers
/// `k..k + 1`, velocity step `v` covers `v..v + 1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapWindow {
	pub key_from: f32,
	pub key_to: f32,
	pub vel_from: f32,
	pub vel_to: f32,
}

impl Default for MapWindow {
	fn default() -> Self {
		Self {
			key_from: 24.0,
			key_to: 24.0 + DEFAULT_KEYS,
			vel_from: 0.0,
			vel_to: ALL,
		}
	}
}

impl MapWindow {
	/// Whole octaves around the zones, at least [`DEFAULT_KEYS`] wide, every velocity.
	pub fn fit(zones: &[SampleZone]) -> Self {
		let (Some(lo), Some(hi)) = (
			zones.iter().map(|z| z.key_low).min(),
			zones.iter().map(|z| z.key_high).max(),
		) else {
			return Self::default();
		};
		let mut from = f32::from(lo / 12 * 12);
		let mut to = ((f32::from(hi) + 1.0) / 12.0).ceil() * 12.0;
		if to - from < DEFAULT_KEYS {
			let center = (from + to) * 0.5;
			from = ((center - DEFAULT_KEYS * 0.5) / 12.0).floor() * 12.0;
			to = from + DEFAULT_KEYS;
		}
		Self {
			key_from: from,
			key_to: to,
			vel_from: 0.0,
			vel_to: ALL,
		}
		.clamped()
	}

	pub fn keys(&self) -> f32 {
		self.key_to - self.key_from
	}

	pub fn vels(&self) -> f32 {
		self.vel_to - self.vel_from
	}

	/// Kept inside the 128 keys and velocities.
	pub fn clamped(self) -> Self {
		let keys = self.keys().clamp(MIN_KEYS, ALL);
		let vels = self.vels().clamp(MIN_VELS, ALL);
		let key_from = self.key_from.clamp(0.0, ALL - keys);
		let vel_from = self.vel_from.clamp(0.0, ALL - vels);
		Self {
			key_from,
			key_to: key_from + keys,
			vel_from,
			vel_to: vel_from + vels,
		}
	}

	/// Unit x of key position `key`.
	pub fn x(&self, key: f32) -> f32 {
		(key - self.key_from) / self.keys()
	}

	/// Unit y (down) of velocity position `vel`.
	pub fn y(&self, vel: f32) -> f32 {
		1.0 - (vel - self.vel_from) / self.vels()
	}

	/// The key position at unit x.
	pub fn key_at(&self, x: f32) -> f32 {
		self.key_from + x * self.keys()
	}

	/// The velocity position at unit y.
	pub fn vel_at(&self, y: f32) -> f32 {
		self.vel_from + (1.0 - y) * self.vels()
	}

	/// The key under unit x.
	pub fn key_index(&self, x: f32) -> u8 {
		self.key_at(x).floor().clamp(0.0, 127.0) as u8
	}

	/// The velocity step under unit y.
	pub fn vel_index(&self, y: f32) -> u8 {
		self.vel_at(y).floor().clamp(0.0, 127.0) as u8
	}

	/// Where zone `z` sits.
	pub fn zone_box(&self, z: &SampleZone) -> UnitBox {
		let (vl, vh) = vel_range(z);
		UnitBox {
			left: self.x(f32::from(z.key_low)),
			right: self.x(f32::from(z.key_high) + 1.0),
			top: self.y(f32::from(vh) + 1.0),
			bottom: self.y(f32::from(vl)),
		}
	}

	/// Unit x of the centre of key `k` (the root dot).
	pub fn key_center(&self, k: u8) -> f32 {
		self.x(f32::from(k) + 0.5)
	}

	/// Keys zoomed by `factor` (< 1 = in) around key position `at`.
	pub fn zoom_keys(self, at: f32, factor: f32) -> Self {
		let keys = (self.keys() * factor).clamp(MIN_KEYS, ALL);
		let k = ((at - self.key_from) / self.keys()).clamp(0.0, 1.0);
		let from = at - k * keys;
		Self {
			key_from: from,
			key_to: from + keys,
			..self
		}
		.clamped()
	}

	/// Velocities zoomed by `factor` around velocity position `at`.
	pub fn zoom_vels(self, at: f32, factor: f32) -> Self {
		let vels = (self.vels() * factor).clamp(MIN_VELS, ALL);
		let k = ((at - self.vel_from) / self.vels()).clamp(0.0, 1.0);
		let from = at - k * vels;
		Self {
			vel_from: from,
			vel_to: from + vels,
			..self
		}
		.clamped()
	}

	/// Velocity axis labels inside the window (127, 96, 64, 32, 0).
	pub fn vel_labels(&self) -> Vec<u8> {
		[127u8, 96, 64, 32, 0]
			.into_iter()
			.filter(|v| {
				let v = f32::from(*v);
				v >= self.vel_from && v <= self.vel_to - 1.0
			})
			.collect()
	}
}

// --- Zone gestures --------------------------------------------------------------------

/// What a press on a zone grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZonePart {
	/// Moves keys and velocity together (the root key follows).
	Body,
	Low,
	High,
	Top,
	Bottom,
	/// The root dot: horizontally only.
	Root,
}

impl ZonePart {
	/// The undo step's name.
	pub fn label(self) -> &'static str {
		match self {
			ZonePart::Body => "Move zone",
			ZonePart::Low => "Zone low key",
			ZonePart::High => "Zone high key",
			ZonePart::Top => "Zone top velocity",
			ZonePart::Bottom => "Zone bottom velocity",
			ZonePart::Root => "Zone root key",
		}
	}
}

/// `z` with `part` dragged by `keys` keys and `vels` velocity steps (up = positive),
/// kept valid: keys 0..=127, low <= high, velocities 0..=127.
pub fn drag_zone(z: &SampleZone, part: ZonePart, keys: i32, vels: i32) -> SampleZone {
	let mut out = z.clone();
	let (vl, vh) = vel_range(z);
	let (lo, hi) = (i32::from(z.key_low), i32::from(z.key_high));
	let set_vel = |out: &mut SampleZone, nl: i32, nh: i32| {
		if nl != i32::from(vl) {
			out.vel_low = step_vel(nl as u8);
		}
		if nh != i32::from(vh) {
			out.vel_high = step_vel(nh as u8);
		}
	};
	match part {
		ZonePart::Body => {
			let span = hi - lo;
			let new_lo = (lo + keys).clamp(0, 127 - span);
			let moved = new_lo - lo;
			out.key_low = new_lo as u8;
			out.key_high = (new_lo + span) as u8;
			out.root_key = (i32::from(z.root_key) + moved).clamp(0, 127) as u8;
			let vspan = i32::from(vh) - i32::from(vl);
			let nl = (i32::from(vl) + vels).clamp(0, 127 - vspan);
			set_vel(&mut out, nl, nl + vspan);
		}
		ZonePart::Low => out.key_low = (lo + keys).clamp(0, hi) as u8,
		ZonePart::High => out.key_high = (hi + keys).clamp(lo, 127) as u8,
		ZonePart::Top => {
			let nh = (i32::from(vh) + vels).clamp(i32::from(vl), 127);
			set_vel(&mut out, i32::from(vl), nh);
		}
		ZonePart::Bottom => {
			let nl = (i32::from(vl) + vels).clamp(0, i32::from(vh));
			set_vel(&mut out, nl, i32::from(vh));
		}
		ZonePart::Root => out.root_key = (i32::from(z.root_key) + keys).clamp(0, 127) as u8,
	}
	out
}

/// `z` grown to fill the empty space around it: keys out to the nearest zones on each
/// side that share some of its velocities, then velocities out to the nearest zones
/// above and below that share some of the new keys (zones already overlapping it are
/// ignored; no neighbour = the end of the range).
pub fn fit(z: &SampleZone, others: &[&SampleZone]) -> SampleZone {
	let (vl, vh) = vel_range(z);
	let vel_overlap = |o: &SampleZone| {
		let (ol, oh) = vel_range(o);
		ol <= vh && oh >= vl
	};
	let low = others
		.iter()
		.filter(|o| vel_overlap(o) && o.key_high < z.key_low)
		.map(|o| o.key_high + 1)
		.max()
		.unwrap_or(0);
	let high = others
		.iter()
		.filter(|o| vel_overlap(o) && o.key_low > z.key_high)
		.map(|o| o.key_low - 1)
		.min()
		.unwrap_or(127);
	let key_overlap = |o: &SampleZone| o.key_low <= high && o.key_high >= low;
	let vlow = others
		.iter()
		.filter(|o| key_overlap(o) && vel_range(o).1 < vl)
		.map(|o| vel_range(o).1 + 1)
		.max()
		.unwrap_or(0);
	let vhigh = others
		.iter()
		.filter(|o| key_overlap(o) && vel_range(o).0 > vh)
		.map(|o| vel_range(o).0 - 1)
		.min()
		.unwrap_or(127);
	let mut out = z.clone();
	out.key_low = low;
	out.key_high = high;
	if vlow != vl {
		out.vel_low = step_vel(vlow);
	}
	if vhigh != vh {
		out.vel_high = step_vel(vhigh);
	}
	out
}

fn others(zones: &[SampleZone], skip: Option<usize>) -> Vec<&SampleZone> {
	zones
		.iter()
		.enumerate()
		.filter(|(i, _)| Some(*i) != skip)
		.map(|(_, z)| z)
		.collect()
}

/// Zone `index` fitted to its neighbours (double-click on a zone).
pub fn fit_to_neighbours(zones: &[SampleZone], index: usize) -> Option<Vec<SampleZone>> {
	let z = zones.get(index)?;
	let mut out = zones.to_vec();
	out[index] = fit(z, &others(zones, Some(index)));
	Some(out)
}

/// A zone of `template`'s sample (root, tune and the rest of its settings) filling the
/// empty spot at `key` / `vel` (double-click or a file dropped on an empty spot).
pub fn new_zone(zones: &[SampleZone], template: &SampleZone, key: u8, vel: u8) -> SampleZone {
	let mut seed = template.clone();
	seed.key_low = key.min(127);
	seed.key_high = key.min(127);
	seed.vel_low = step_vel(vel);
	seed.vel_high = step_vel(vel);
	fit(&seed, &others(zones, None))
}

/// Zone `index` split into two velocity layers (the lower half stays at `index`, the
/// upper half follows it); `None` when it spans a single velocity.
pub fn split_velocity(zones: &[SampleZone], index: usize) -> Option<Vec<SampleZone>> {
	let z = zones.get(index)?;
	let (vl, vh) = vel_range(z);
	if vh <= vl {
		return None;
	}
	let mid = vl + (vh - vl).div_ceil(2);
	let mut lower = z.clone();
	lower.vel_high = step_vel(mid - 1);
	let mut upper = z.clone();
	upper.vel_low = step_vel(mid);
	let mut out = zones.to_vec();
	out[index] = lower;
	out.insert(index + 1, upper);
	Some(out)
}

/// A copy of zone `index` right after it, moved by its own width up the keyboard (or
/// down when there is no room above; in place when neither fits).
pub fn duplicate(zones: &[SampleZone], index: usize) -> Option<Vec<SampleZone>> {
	let z = zones.get(index)?;
	let span = i32::from(z.key_high) - i32::from(z.key_low) + 1;
	let shift = if i32::from(z.key_high) + span <= 127 {
		span
	} else if i32::from(z.key_low) >= span {
		-span
	} else {
		0
	};
	let copy = drag_zone(z, ZonePart::Body, shift, 0);
	let mut out = zones.to_vec();
	out.insert(index + 1, copy);
	Some(out)
}

/// Every zone's root key from its file name ("Piano C3.wav"; zones whose names announce
/// none keep theirs), then the keys of each velocity layer split halfway between
/// neighbouring roots, from key 0 to 127.
pub fn auto_map(zones: &[SampleZone]) -> Vec<SampleZone> {
	let mut out: Vec<SampleZone> = zones
		.iter()
		.map(|z| {
			let mut z = z.clone();
			if let Some(root) = cadenza_model::root_from_file_name(&z.sample.display_name()) {
				z.root_key = root;
			}
			z
		})
		.collect();
	let mut layers: Vec<(u8, u8)> = out.iter().map(vel_range).collect();
	layers.sort_unstable();
	layers.dedup();
	for layer in layers {
		let mut roots: Vec<u8> = out
			.iter()
			.filter(|z| vel_range(z) == layer)
			.map(|z| z.root_key)
			.collect();
		roots.sort_unstable();
		roots.dedup();
		for z in out.iter_mut().filter(|z| vel_range(z) == layer) {
			let j = roots.iter().position(|r| *r == z.root_key).unwrap_or(0);
			let mid = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
			z.key_low = if j == 0 {
				0
			} else {
				mid(roots[j - 1], roots[j]) + 1
			};
			z.key_high = roots.get(j + 1).map_or(127, |next| mid(roots[j], *next));
		}
	}
	out
}

/// The zone (index) under `key` / `vel`: the last one drawn on top wins.
pub fn zone_at(zones: &[SampleZone], key: u8, vel: u8) -> Option<usize> {
	zones.iter().rposition(|z| {
		let (vl, vh) = vel_range(z);
		(z.key_low..=z.key_high).contains(&key) && (vl..=vh).contains(&vel)
	})
}

// --- Commands -------------------------------------------------------------------------

/// `r`'s zones replaced by `zones` as one undo step named `label` (`None`: no change).
pub fn set(
	project: &Project,
	r: DeviceRef,
	zones: Vec<SampleZone>,
	label: &str,
) -> Option<Command> {
	sm::set_zones(project, r, zones, label).map(|c| labeled(c, label.to_owned()))
}

/// Zone `index` replaced by `zone`.
pub fn replace(
	project: &Project,
	r: DeviceRef,
	index: usize,
	zone: SampleZone,
	label: &str,
) -> Option<Command> {
	let mut zones = device(project, r)?.zones.clone();
	*zones.get_mut(index)? = zone;
	set(project, r, zones, label)
}

/// Zone `index` edited with `f`.
pub fn edit(
	project: &Project,
	r: DeviceRef,
	index: usize,
	label: &str,
	f: impl FnOnce(&mut SampleZone),
) -> Option<Command> {
	let mut zone = device(project, r)?.zones.get(index)?.clone();
	f(&mut zone);
	replace(project, r, index, zone, label)
}

/// Zone `index` with `part` dragged (one step per drag, or per arrow key).
pub fn drag_command(
	project: &Project,
	r: DeviceRef,
	index: usize,
	part: ZonePart,
	keys: i32,
	vels: i32,
) -> Option<Command> {
	let z = device(project, r)?.zones.get(index)?;
	let moved = drag_zone(z, part, keys, vels);
	replace(project, r, index, moved, part.label())
}

// --- Loop and crossfade ---------------------------------------------------------------

/// The crossfade the processor plays at the loop seam: forward loops only, at most the
/// loop's length and the room before its start.
pub fn effective_crossfade(z: &SampleZone, length: f64) -> f64 {
	if z.loop_mode != LoopMode::Forward || z.reverse {
		return 0.0;
	}
	let ls = Marker::LoopStart.get(z, length);
	let le = Marker::LoopEnd.get(z, length);
	z.loop_crossfade.min(ls).min((le - ls).max(0.0)).max(0.0)
}

/// The crossfade after dragging the ramp's foot to `at` seconds (it ends at the loop
/// end), in whole milliseconds.
pub fn crossfade_drag(z: &SampleZone, at: f64, length: f64) -> f64 {
	let ls = Marker::LoopStart.get(z, length);
	let le = Marker::LoopEnd.get(z, length);
	let max = ls.min(le - ls).clamp(0.0, 10.0);
	((le - at).clamp(0.0, max) * 1000.0).round() / 1000.0
}

/// "Loop 1.42 – 2.36 s · xfade 120 ms" (ping-pong: "· ping-pong"); off: `None`.
pub fn loop_label(z: &SampleZone, length: f64) -> Option<String> {
	if z.loop_mode.is_off() || z.reverse {
		return None;
	}
	let ls = Marker::LoopStart.get(z, length);
	let le = Marker::LoopEnd.get(z, length);
	let tail = match z.loop_mode {
		LoopMode::PingPong => "ping-pong".to_owned(),
		_ => format!("xfade {}", ms_label(effective_crossfade(z, length))),
	};
	Some(format!("Loop {ls:.2} \u{2013} {le:.2} s \u{b7} {tail}"))
}

/// The undo step name of a waveform flag drag.
pub fn marker_label(m: Marker) -> &'static str {
	match m {
		Marker::Start => "Zone start",
		Marker::End => "Zone end",
		Marker::LoopStart => "Loop start",
		Marker::LoopEnd => "Loop end",
	}
}

// --- Envelopes ------------------------------------------------------------------------

/// The amplitude or the filter envelope (every zone shares them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
	Amp,
	Filter,
}

impl Env {
	pub fn param(self, stage: AdsrStage) -> &'static str {
		match (self, stage) {
			(Env::Amp, AdsrStage::Attack) => "amp_attack",
			(Env::Amp, AdsrStage::Decay) => "amp_decay",
			(Env::Amp, AdsrStage::Sustain) => "amp_sustain",
			(Env::Amp, AdsrStage::Release) => "amp_release",
			(Env::Filter, AdsrStage::Attack) => "filter_attack",
			(Env::Filter, AdsrStage::Decay) => "filter_decay",
			(Env::Filter, AdsrStage::Sustain) => "filter_sustain",
			(Env::Filter, AdsrStage::Release) => "filter_release",
		}
	}

	pub fn ranges(self) -> AdsrRanges {
		let spec = |stage| {
			let p = param(self.param(stage));
			KnobSpec::new(p.min, p.max, p.default)
		};
		AdsrRanges {
			attack: spec(AdsrStage::Attack),
			decay: spec(AdsrStage::Decay),
			sustain: spec(AdsrStage::Sustain),
			release: spec(AdsrStage::Release),
			sustain_unit: "%",
		}
	}

	pub fn values(self, value: impl Fn(&str) -> f32) -> AdsrValues {
		AdsrValues {
			attack: value(self.param(AdsrStage::Attack)),
			decay: value(self.param(AdsrStage::Decay)),
			sustain: value(self.param(AdsrStage::Sustain)),
			release: value(self.param(AdsrStage::Release)),
		}
	}

	/// The undo step's name for a node gesture that moved `stages`.
	pub fn label(self, stages: &[AdsrStage]) -> String {
		let env = match self {
			Env::Amp => "Amp",
			Env::Filter => "Filter",
		};
		let names: Vec<&str> = stages
			.iter()
			.map(|s| crate::state::poly_synth_editor::stage_name(*s))
			.collect();
		format!("{env} {}", names.join(" and ").to_lowercase())
	}
}

/// "A 2 ms · D 1.20 s · S 80 % · R 900 ms".
pub fn env_summary(v: &AdsrValues) -> String {
	let t = |ms: f32| ms_label(f64::from(ms) * 0.001);
	format!(
		"A {} \u{b7} D {} \u{b7} S {:.0} % \u{b7} R {}",
		t(v.attack),
		t(v.decay),
		v.sustain,
		t(v.release)
	)
}

// --- Filter ---------------------------------------------------------------------------

/// The filter type (0 off, 1 LP, 2 HP, 3 BP) a parameter value selects.
pub fn filter_type(value: f32) -> usize {
	value.round().clamp(0.0, 3.0) as usize
}

/// The filter's magnitude at `freq` as the voice applies it (0 dB when off).
pub fn response_db(kind: usize, cutoff: f32, resonance: f32, freq: f32) -> f32 {
	cadenza_dsp::response::sampler_filter_response_db(kind, cutoff, resonance, DISPLAY_RATE, freq)
}

/// The filter node's start for a drag (the values when it was grabbed, its dB on screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeStart {
	pub resonance: f32,
	pub db: f32,
}

/// The node dragged to `freq` / `db`: cutoff follows x, resonance the vertical travel.
pub fn node_drag(start: NodeStart, freq: f32, db: f32) -> [Change; 2] {
	let res = start.resonance + (db - start.db) * RES_PER_DB;
	[
		(CUTOFF, clamp(CUTOFF, freq)),
		(RESONANCE, clamp(RESONANCE, (res * 10.0).round() / 10.0)),
	]
}

/// Double-click on the node: cutoff and resonance back to their defaults.
pub fn reset_node() -> [Change; 2] {
	[(CUTOFF, default(CUTOFF)), (RESONANCE, default(RESONANCE))]
}

/// Env amount after `notches` wheel notches from `start` (one semitone each).
pub fn wheel_env(start: f32, notches: f32) -> f32 {
	clamp(ENV_AMOUNT, (start + notches).round())
}

/// "4.2 kHz · res 20 % · env +12 st" ("Filter off" when off).
pub fn filter_summary(kind: usize, cutoff: f32, resonance: f32, env: f32) -> String {
	if kind == 0 {
		return "Filter off \u{b7} pick LP, HP or BP".to_owned();
	}
	let env =
		crate::state::device::minus(crate::state::device::format_value(env.round(), "semitones"));
	format!(
		"{} \u{b7} res {resonance:.0} % \u{b7} env {env}",
		short_hz(cutoff)
	)
}

// --- Bottom row -----------------------------------------------------------------------

/// The velocity curve's word: "soft" (soft notes louder), "linear", "hard".
pub fn curve_word(value: f32) -> &'static str {
	if value < -10.0 {
		CURVES[0].0
	} else if value > 10.0 {
		CURVES[2].0
	} else {
		CURVES[1].0
	}
}

/// The value of a bottom-row readout after a vertical drag of `dy` points from `start`
/// (up = more): whole voices, semitones and milliseconds of glide, half percents.
pub fn drag_readout(id: &str, start: f32, dy: f32) -> f32 {
	let steps = -dy;
	let v = match id {
		POLYPHONY => (start + steps / 4.0).round(),
		TRANSPOSE => (start + steps / 6.0).round(),
		GLIDE => (start + steps * 5.0).round(),
		_ => ((start + steps * 0.5) * 2.0).round() / 2.0,
	};
	clamp(id, v)
}

// --- App glue -------------------------------------------------------------------------

impl super::AppState {
	/// The file a double-click on an empty spot of `r`'s zone map makes a zone of: the
	/// sample loaded last, else the selected zone's.
	pub fn zone_source(&self, r: DeviceRef) -> Option<PathBuf> {
		if let Some(p) = &self.view.instrument.last_sample {
			return Some(p.clone());
		}
		let d = self.device(r)?;
		let i = self.view.instrument.zone.min(d.zones.len().checked_sub(1)?);
		Some(d.zones[i].sample.path.clone())
	}

	/// A new zone of `path` filling the empty spot at `key` / `vel` of sampler `r`'s map
	/// (one undo step "Add zone"): the settings of a zone already playing that file, else
	/// its root from the name or pitch. Selects it.
	pub fn add_zone_at(&mut self, r: DeviceRef, path: &Path, key: u8, vel: u8) -> bool {
		let Some(d) = self.device(r) else {
			return false;
		};
		let template = match d.zones.iter().find(|z| z.sample.path == path) {
			Some(z) => z.clone(),
			None => match cadenza_engine::auto_zone(path) {
				Ok(z) => z,
				Err(e) => {
					self.notices.error(e.to_string());
					return false;
				}
			},
		};
		let mut zones = d.zones.clone();
		zones.push(new_zone(&d.zones, &template, key, vel));
		let index = zones.len() - 1;
		let cmd = set(self.project(), r, zones, "Add zone");
		let ok = self.apply_sample_edit(cmd);
		if ok {
			self.view.instrument.zone = index;
			self.view.instrument.last_sample = Some(path.to_path_buf());
		}
		ok
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Device, Document, Track, TrackKind};

	fn zone(lo: u8, hi: u8, vl: u8, vh: u8) -> SampleZone {
		let mut z = SampleZone::new(
			cadenza_model::SampleRef::new(format!("/s/Felt {lo}.wav")),
			lo,
			hi,
			lo + 6,
		);
		z.vel_low = step_vel(vl);
		z.vel_high = step_vel(vh);
		z
	}

	/// 2 velocity layers x 6 octaves from C1 (24), like the design.
	fn layered() -> Vec<SampleZone> {
		(0..6u8)
			.flat_map(|o| {
				let lo = 24 + o * 12;
				[zone(lo, lo + 11, 0, 79), zone(lo, lo + 11, 80, 127)]
			})
			.collect()
	}

	fn fixture(zones: Vec<SampleZone>) -> (Project, DeviceRef) {
		let mut p = Project::new("s");
		let d = Device::builtin("sampler", "Sampler").with_zones(zones);
		let t = Track::new("Keys", TrackKind::Instrument).with_instrument(d);
		let r = DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		};
		p.tracks.push(t);
		(p, r)
	}

	fn run(p: &Project, cmd: Command) -> (Project, String) {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		assert_eq!(doc.history_labels().len(), 1, "one undo step");
		let label = doc.history_labels()[0].to_string();
		(doc.project().clone(), label)
	}

	#[test]
	fn labels_and_colours_follow_the_key_ranges() {
		let zones = layered();
		assert_eq!(key_range_label(&zones[4]), "C3\u{2013}B3");
		assert_eq!(vel_range_label(&zones[5]), "80\u{2013}127");
		assert_eq!(
			zone_title(&zones[5]),
			"C3\u{2013}B3 \u{b7} vel 80\u{2013}127"
		);
		let colors = zone_colors(&zones);
		assert_eq!(colors[0], colors[1], "velocity layers share a colour");
		assert_ne!(colors[0], colors[2]);
		assert!(!colors.contains(&5), "blue is kept for loops");
		assert_eq!(vel_step(step_vel(80)), 80);
	}

	#[test]
	fn the_window_maps_zones_to_boxes_and_back() {
		let zones = layered();
		let w = MapWindow::fit(&zones);
		assert_eq!((w.key_from, w.key_to), (24.0, 96.0), "six octaves from C1");
		let b = w.zone_box(&zones[1]);
		assert!((b.left - 0.0).abs() < 1e-6 && (b.right - 12.0 / 72.0).abs() < 1e-6);
		assert!((b.top - 0.0).abs() < 1e-6, "velocity 127 reaches the top");
		assert!((b.bottom - (1.0 - 80.0 / 128.0)).abs() < 1e-6);
		let b0 = w.zone_box(&zones[0]);
		assert!((b0.bottom - 1.0).abs() < 1e-6 && (b0.top - b.bottom).abs() < 1e-6);
		// Back: the middle of a box lands on its zone.
		let (x, y) = ((b.left + b.right) * 0.5, (b.top + b.bottom) * 0.5);
		assert_eq!(
			zone_at(&zones, w.key_index(x), w.vel_index(y)),
			Some(1),
			"key {} vel {}",
			w.key_index(x),
			w.vel_index(y)
		);
		assert_eq!(w.key_index(0.0), 24);
		assert_eq!(w.vel_index(0.999), 0);
		assert!((w.key_center(24) - 0.5 / 72.0).abs() < 1e-6);
		// A small zone set still shows six octaves; the whole keyboard shows everything.
		let one = MapWindow::fit(&[zone(60, 64, 0, 127)]);
		assert_eq!(one.keys(), 72.0);
		assert!(one.key_from <= 60.0 && one.key_to >= 65.0);
		let all = MapWindow::fit(&[SampleZone::full_keyboard("/a.wav", 60)]);
		assert_eq!((all.key_from, all.key_to), (0.0, 128.0));
		// Zooms keep the point under the pointer and stay inside the keyboard.
		let z = w.zoom_keys(60.0, 0.5);
		assert_eq!(z.keys(), 36.0);
		assert!((z.x(60.0) - w.x(60.0)).abs() < 1e-5);
		assert_eq!(w.zoom_keys(60.0, 10.0).keys(), 128.0);
		let v = w.zoom_vels(100.0, 0.25);
		assert_eq!(v.vels(), 32.0);
		assert!((v.y(100.0) - w.y(100.0)).abs() < 1e-5);
		assert_eq!(w.vel_labels(), vec![127, 96, 64, 32, 0]);
	}

	#[test]
	fn dragging_a_zone_moves_resizes_and_retunes_it_within_bounds() {
		let z = zone(60, 71, 80, 127);
		let moved = drag_zone(&z, ZonePart::Body, 5, -20);
		assert_eq!(
			(moved.key_low, moved.key_high, moved.root_key),
			(65, 76, 71)
		);
		assert_eq!(vel_range(&moved), (60, 107));
		let far = drag_zone(&z, ZonePart::Body, 100, 40);
		assert_eq!(
			(far.key_low, far.key_high),
			(116, 127),
			"stops at the top key"
		);
		assert_eq!(vel_range(&far), (80, 127), "already at the top velocity");
		assert_eq!(far.vel_high, z.vel_high, "unmoved velocities stay exact");
		let low = drag_zone(&z, ZonePart::Low, 20, 0);
		assert_eq!(
			(low.key_low, low.key_high),
			(71, 71),
			"low never passes high"
		);
		let high = drag_zone(&z, ZonePart::High, -3, 0);
		assert_eq!(high.key_high, 68);
		let top = drag_zone(&z, ZonePart::Top, 0, -60);
		assert_eq!(vel_range(&top), (80, 80));
		let bottom = drag_zone(&z, ZonePart::Bottom, 0, -100);
		assert_eq!(vel_range(&bottom), (0, 127));
		let root = drag_zone(&z, ZonePart::Root, -2, 9);
		assert_eq!(
			(root.root_key, root.key_low, root.key_high),
			(64, 60, 71),
			"the root dot moves only the root"
		);
		assert_eq!(drag_zone(&z, ZonePart::Root, -100, 0).root_key, 0);
		// One command, one undo step, named after the gesture.
		let (p, r) = fixture(layered());
		let (after, label) = run(&p, drag_command(&p, r, 3, ZonePart::Body, 2, 0).unwrap());
		assert_eq!(label, "Move zone");
		assert_eq!(device(&after, r).unwrap().zones[3].key_low, 38);
		assert!(drag_command(&p, r, 3, ZonePart::Body, 0, 0).is_none());
	}

	#[test]
	fn fit_to_neighbours_fills_the_gap_around_a_zone() {
		let mut zones = layered();
		// Shrink C3–B3 hard to D3–E3 80–100, then fit it back.
		zones[5] = zone(50, 52, 80, 100);
		let fitted = fit_to_neighbours(&zones, 5).unwrap();
		let z = &fitted[5];
		assert_eq!((z.key_low, z.key_high), (48, 59));
		assert_eq!(vel_range(z), (80, 127));
		// Alone, a zone fills the whole map.
		let one = fit_to_neighbours(&[zone(60, 61, 10, 20)], 0).unwrap();
		assert_eq!((one[0].key_low, one[0].key_high), (0, 127));
		assert_eq!((one[0].vel_low, one[0].vel_high), (0.0, 1.0));
		// A new zone in the empty spot of a missing layer.
		let mut gap = layered();
		gap.remove(5);
		let n = new_zone(&gap, &gap[0], 54, 100);
		assert_eq!((n.key_low, n.key_high), (48, 59));
		assert_eq!(vel_range(&n), (80, 127));
		assert_eq!(n.sample.path, gap[0].sample.path);
		assert!(fit_to_neighbours(&gap, 99).is_none());
	}

	#[test]
	fn split_duplicate_and_auto_map_build_valid_zone_sets() {
		let zones = vec![zone(60, 71, 0, 127)];
		let split = split_velocity(&zones, 0).unwrap();
		assert_eq!(split.len(), 2);
		assert_eq!(vel_range(&split[0]), (0, 63));
		assert_eq!(vel_range(&split[1]), (64, 127));
		assert!(split_velocity(&[zone(60, 71, 9, 9)], 0).is_none());
		let dup = duplicate(&zones, 0).unwrap();
		assert_eq!(dup.len(), 2);
		assert_eq!(
			(dup[1].key_low, dup[1].key_high, dup[1].root_key),
			(72, 83, 78)
		);
		let top = duplicate(&[zone(120, 127, 0, 127)], 0).unwrap();
		assert_eq!(
			(top[1].key_low, top[1].key_high),
			(112, 119),
			"no room above"
		);
		// Auto-map: roots from the names, keys split halfway between them per layer.
		let named = |name: &str, vl: u8, vh: u8| {
			let mut z = zone(0, 127, vl, vh);
			z.sample = cadenza_model::SampleRef::new(format!("/s/{name}"));
			z.root_key = 60;
			z
		};
		let mapped = auto_map(&[
			named("Piano C3 soft.wav", 0, 79),
			named("Piano C4 soft.wav", 0, 79),
			named("Piano C5 soft.wav", 0, 79),
			named("Piano C4 hard.wav", 80, 127),
			named("Noise.wav", 80, 127),
		]);
		let keys: Vec<(u8, u8, u8)> = mapped
			.iter()
			.map(|z| (z.key_low, z.key_high, z.root_key))
			.collect();
		assert_eq!(
			keys,
			vec![
				(0, 54, 48),
				(55, 66, 60),
				(67, 127, 72),
				(0, 127, 60),
				(0, 127, 60)
			],
			"the hard layer shares C4: both span the keyboard"
		);
		let (p, r) = fixture(layered());
		for zones in [split, dup, mapped] {
			let (_, label) = run(&p, set(&p, r, zones, "Edit").unwrap());
			assert_eq!(label, "Edit");
		}
	}

	#[test]
	fn the_loop_readout_and_crossfade_ramp_follow_the_processor() {
		let mut z = SampleZone::full_keyboard("/a.wav", 60);
		assert_eq!(loop_label(&z, 3.0), None);
		z.loop_mode = LoopMode::Forward;
		z.loop_start = 1.42;
		z.loop_end = Some(2.36);
		z.loop_crossfade = 0.12;
		assert_eq!(
			loop_label(&z, 3.0).unwrap(),
			"Loop 1.42 \u{2013} 2.36 s \u{b7} xfade 120 ms"
		);
		assert!((effective_crossfade(&z, 3.0) - 0.12).abs() < 1e-9);
		z.loop_crossfade = 5.0;
		assert!(
			(effective_crossfade(&z, 3.0) - 0.94).abs() < 1e-9,
			"at most the loop length"
		);
		assert!((crossfade_drag(&z, 2.0, 3.0) - 0.36).abs() < 1e-9);
		assert!((crossfade_drag(&z, 0.0, 3.0) - 0.94).abs() < 1e-9);
		assert_eq!(crossfade_drag(&z, 2.9, 3.0), 0.0);
		z.loop_mode = LoopMode::PingPong;
		assert_eq!(effective_crossfade(&z, 3.0), 0.0);
		assert!(loop_label(&z, 3.0).unwrap().ends_with("ping-pong"));
		assert_eq!(marker_label(Marker::LoopEnd), "Loop end");
	}

	#[test]
	fn globals_bind_the_sampler_params() {
		let ranges = Env::Filter.ranges();
		assert_eq!(ranges.release.max, 20_000.0);
		let v = Env::Amp.values(|id| match id {
			"amp_attack" => 2.0,
			"amp_decay" => 1200.0,
			"amp_sustain" => 80.0,
			_ => 900.0,
		});
		assert_eq!(
			env_summary(&v),
			"A 2 ms \u{b7} D 1.20 s \u{b7} S 80 % \u{b7} R 900 ms"
		);
		assert_eq!(
			Env::Amp.label(&[AdsrStage::Decay, AdsrStage::Sustain]),
			"Amp decay and sustain"
		);
		assert_eq!(filter_type(2.4), 2);
		assert_eq!(response_db(0, 1000.0, 50.0, 5000.0), 0.0);
		assert!(response_db(1, 1000.0, 10.0, 8000.0) < -20.0, "low-pass");
		assert!(response_db(2, 1000.0, 10.0, 100.0) < -20.0, "high-pass");
		let [(c, cut), (_, res)] = node_drag(
			NodeStart {
				resonance: 20.0,
				db: 0.0,
			},
			4200.0,
			4.8,
		);
		assert_eq!(c, CUTOFF);
		assert_eq!((cut, res), (4200.0, 30.0));
		assert_eq!(wheel_env(12.0, 2.0), 14.0);
		assert_eq!(wheel_env(95.0, 9.0), 96.0);
		assert_eq!(
			filter_summary(1, 4200.0, 20.0, 12.0),
			"4.2 kHz \u{b7} res 20 % \u{b7} env +12 st"
		);
		assert_eq!(curve_word(-50.0), "soft");
		assert_eq!(curve_word(3.0), "linear");
		assert_eq!(drag_readout(POLYPHONY, 16.0, -40.0), 26.0);
		assert_eq!(drag_readout(POLYPHONY, 60.0, -400.0), 64.0);
		assert_eq!(drag_readout(TRANSPOSE, 0.0, 12.0), -2.0);
		assert_eq!(drag_readout(VELOCITY, 70.0, -10.0), 75.0);
	}
}
