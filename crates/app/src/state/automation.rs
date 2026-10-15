//! Automation lanes in the arrange view: which lanes are shown, what each parameter is
//! (range, unit, display), and the pure gesture -> [`Command`] builders (add, move,
//! delete, reshape points; tempo points). The view in `ui::automation` only hit-tests
//! and previews; every gesture ends in one `SetAutomation` (or one tempo command).

use std::collections::HashMap;

use cadenza_core::{ParamAddress, ParamTarget, TempoPoint, Tick, TrackId};
use cadenza_model::{AutomationPoint, Command, DeviceKind, PointShape, Project};
use cadenza_plugin_host::PluginCatalog;

/// Height of one automation lane (and of the tempo lane) in the arrange view.
pub const LANE_HEIGHT: f32 = 52.0;
/// Range of the tempo lane.
pub const TEMPO_MIN: f64 = 40.0;
pub const TEMPO_MAX: f64 = 240.0;
/// Fader-law range used to draw volume-like lanes.
const DB_MAX: f32 = 6.0;
const DB_MIN: f32 = -96.0;

/// Automation lanes of one channel (`None` key = master) in the arrange view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaneView {
	/// The lanes are expanded under the track.
	pub open: bool,
	/// Lanes added from the picker (they may still be empty, i.e. not in the project).
	pub shown: Vec<ParamAddress>,
}

/// Arrange-view automation state (not part of the project).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutomationView {
	pub lanes: HashMap<Option<TrackId>, LaneView>,
	/// The tempo lane is shown under the markers.
	pub tempo: bool,
	/// Selected points (lane address + point tick; ticks are unique within a lane).
	pub selected: Vec<(ParamAddress, Tick)>,
	/// The master's lanes are listed after the tracks.
	pub master: bool,
}

impl AutomationView {
	pub fn is_open(&self, owner: Option<TrackId>) -> bool {
		self.lanes.get(&owner).is_some_and(|l| l.open)
	}

	/// Lanes listed under `owner`: the ones added in the view, then the project's other
	/// lanes of that channel (addresses that no longer resolve are dropped).
	pub fn visible(&self, project: &Project, owner: Option<TrackId>) -> Vec<ParamAddress> {
		let mut out: Vec<ParamAddress> = self
			.lanes
			.get(&owner)
			.map(|l| l.shown.clone())
			.unwrap_or_default();
		for lane in &project.automation {
			if lane.address.track == owner && !out.contains(&lane.address) {
				out.push(lane.address.clone());
			}
		}
		out.retain(|a| resolves(project, a));
		out
	}

	/// Toggles the lanes of `owner`; opening a channel without lanes shows its volume.
	pub fn toggle(&mut self, project: &Project, owner: Option<TrackId>) {
		let empty = self.visible(project, owner).is_empty();
		let view = self.lanes.entry(owner).or_default();
		view.open = !view.open;
		if view.open && empty {
			view.shown.push(ParamAddress {
				track: owner,
				target: ParamTarget::Volume,
			});
		}
		if owner.is_none() {
			self.master = true;
		}
	}

	/// Shows (and opens) the lane for `address`.
	pub fn show(&mut self, address: ParamAddress) {
		if address.track.is_none() {
			self.master = true;
		}
		let view = self.lanes.entry(address.track).or_default();
		view.open = true;
		if !view.shown.contains(&address) {
			view.shown.push(address);
		}
	}

	/// Hides a lane (its points stay in the project).
	pub fn hide(&mut self, address: &ParamAddress) {
		if let Some(v) = self.lanes.get_mut(&address.track) {
			v.shown.retain(|a| a != address);
		}
		self.selected.retain(|(a, _)| a != address);
	}

	/// Replaces the lane shown as `old` by `new` (parameter picker).
	pub fn replace(&mut self, old: &ParamAddress, new: ParamAddress) {
		let view = self.lanes.entry(old.track).or_default();
		match view.shown.iter().position(|a| a == old) {
			Some(i) if !view.shown.contains(&new) => view.shown[i] = new,
			Some(i) => {
				view.shown.remove(i);
			}
			None => {
				if !view.shown.contains(&new) {
					view.shown.push(new);
				}
			}
		}
		self.selected.retain(|(a, _)| a != old);
	}

	/// Selected point ticks of one lane.
	pub fn selected_in(&self, address: &ParamAddress) -> Vec<Tick> {
		self.selected
			.iter()
			.filter(|(a, _)| a == address)
			.map(|(_, t)| *t)
			.collect()
	}

	/// Drops selections of points that no longer exist.
	pub fn prune(&mut self, project: &Project) {
		self.selected.retain(|(a, t)| {
			project
				.automation_lane(a)
				.is_some_and(|l| l.points.iter().any(|p| p.tick == *t))
		});
		for view in self.lanes.values_mut() {
			view.shown.retain(|a| resolves(project, a));
		}
	}
}

/// Whether `address` still names something in `project`.
pub fn resolves(project: &Project, address: &ParamAddress) -> bool {
	if let Some(t) = address.track
		&& project.track(t).is_none()
	{
		return false;
	}
	match &address.target {
		ParamTarget::Volume | ParamTarget::Pan => true,
		ParamTarget::Device { device, .. } => project.device(address.track, *device).is_some(),
		ParamTarget::Send { send } => address.track.is_some_and(|t| {
			project
				.track(t)
				.is_some_and(|t| t.channel.sends.iter().any(|s| s.id == *send))
		}),
	}
}

/// How a lane's values map to its height and print.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
	/// dB on the fader law (volume, sends).
	Fader,
	/// -1..1, printed as L/C/R.
	Pan,
	Linear,
	/// Logarithmic (frequencies).
	Log,
}

/// What an automatable parameter is.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamMeta {
	pub name: String,
	pub min: f32,
	pub max: f32,
	pub default: f32,
	pub unit: String,
	pub scale: Scale,
	/// Only whole values are meaningful.
	pub stepped: bool,
}

impl ParamMeta {
	fn db(name: impl Into<String>, default: f32) -> Self {
		Self {
			name: name.into(),
			min: DB_MIN,
			max: DB_MAX,
			default,
			unit: "dB".into(),
			scale: Scale::Fader,
			stepped: false,
		}
	}

	/// Value -> 0..1 (bottom..top of the lane).
	pub fn to_norm(&self, v: f32) -> f32 {
		match self.scale {
			Scale::Fader => fader_pos(v),
			Scale::Pan => (v.clamp(-1.0, 1.0) + 1.0) * 0.5,
			Scale::Log if self.min > 0.0 => {
				let (lo, hi) = (self.min.ln(), self.max.ln());
				((v.max(self.min).ln() - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0)
			}
			_ => ((v - self.min) / (self.max - self.min).max(1e-9)).clamp(0.0, 1.0),
		}
	}

	/// 0..1 -> value (clamped, rounded when stepped).
	pub fn from_norm(&self, n: f32) -> f32 {
		let n = n.clamp(0.0, 1.0);
		let v = match self.scale {
			Scale::Fader => fader_db(n),
			Scale::Pan => n * 2.0 - 1.0,
			Scale::Log if self.min > 0.0 => {
				(self.min.ln() + n * (self.max.ln() - self.min.ln())).exp()
			}
			_ => self.min + n * (self.max - self.min),
		};
		if self.stepped { v.round() } else { v }
	}

	/// Value in real units for tooltips: `-6.0 dB`, `L30`, `1.20 kHz`, `45 %`.
	pub fn format(&self, v: f32) -> String {
		match self.scale {
			Scale::Fader if v <= DB_MIN + 0.05 => "-inf dB".into(),
			Scale::Fader => format!("{v:+.1} dB"),
			Scale::Pan => {
				let p = (v * 100.0).round() as i32;
				match p {
					0 => "C".into(),
					p if p < 0 => format!("L{}", -p),
					p => format!("R{p}"),
				}
			}
			_ if self.unit == "Hz" && v.abs() >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
			_ if self.stepped => format!("{v:.0} {}", self.unit).trim_end().to_owned(),
			_ => format!("{v:.2} {}", self.unit).trim_end().to_owned(),
		}
	}
}

fn fader_pos(db: f32) -> f32 {
	if db <= DB_MIN {
		return 0.0;
	}
	let g = 10f32.powf(db / 20.0) / 10f32.powf(DB_MAX / 20.0);
	g.clamp(0.0, 1.0).powf(0.25)
}

fn fader_db(pos: f32) -> f32 {
	if pos <= 0.0 {
		return DB_MIN;
	}
	(20.0 * (pos.powi(4) * 10f32.powf(DB_MAX / 20.0)).log10()).clamp(DB_MIN, DB_MAX)
}

/// One automatable parameter of a channel, for the lane picker.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
	pub address: ParamAddress,
	/// Group (channel, device name) and parameter name.
	pub group: String,
	pub name: String,
}

/// Parameters of a device (built-in registry or scanned plugin descriptor).
pub fn device_params(
	device: &cadenza_model::Device,
	catalog: Option<&PluginCatalog>,
) -> Vec<(String, ParamMeta)> {
	match &device.kind {
		DeviceKind::Builtin { id } => cadenza_dsp::device_info(id)
			.map(|info| {
				info.params
					.iter()
					.map(|p| {
						let log = p.unit == "Hz" && p.min > 0.0 && p.max / p.min > 50.0;
						(
							p.id.to_owned(),
							ParamMeta {
								name: p.name.to_owned(),
								min: p.min,
								max: p.max,
								default: p.default,
								unit: p.unit.to_owned(),
								scale: if log { Scale::Log } else { Scale::Linear },
								stepped: false,
							},
						)
					})
					.collect()
			})
			.unwrap_or_default(),
		DeviceKind::Clap { plugin_id, path }
		| DeviceKind::Vst3 {
			class_id: plugin_id,
			path,
		} => catalog
			.and_then(|c| c.find_at(plugin_id, path))
			.map(|desc| {
				desc.params
					.iter()
					.filter(|p| p.automatable && !p.bypass)
					.map(|p| {
						(
							p.id.clone(),
							ParamMeta {
								name: p.name.clone(),
								min: p.min as f32,
								max: p.max as f32,
								default: p.default as f32,
								unit: String::new(),
								scale: Scale::Linear,
								stepped: p.stepped,
							},
						)
					})
					.collect()
			})
			.unwrap_or_default(),
	}
}

/// What `address` controls, or `None` when it does not resolve.
pub fn meta(
	project: &Project,
	catalog: Option<&PluginCatalog>,
	address: &ParamAddress,
) -> Option<ParamMeta> {
	let channel = project.channel(address.track)?;
	match &address.target {
		ParamTarget::Volume => Some(ParamMeta::db("Volume", 0.0)),
		ParamTarget::Pan => Some(ParamMeta {
			name: "Pan".into(),
			min: -1.0,
			max: 1.0,
			default: 0.0,
			unit: String::new(),
			scale: Scale::Pan,
			stepped: false,
		}),
		ParamTarget::Send { send } => {
			let s = channel.sends.iter().find(|s| s.id == *send)?;
			let bus = project.track(s.target).map_or("?", |t| t.name.as_str());
			Some(ParamMeta::db(format!("Send → {bus}"), -12.0))
		}
		ParamTarget::Device { device, param } => {
			let d = project.device(address.track, *device)?;
			device_params(d, catalog)
				.into_iter()
				.find(|(id, _)| id == param)
				.map(|(_, mut m)| {
					m.name = format!("{}: {}", d.name, m.name);
					m
				})
		}
	}
}

/// Every automatable parameter of a channel (`None` = master): volume, pan, sends, then
/// each device's parameters.
pub fn choices(
	project: &Project,
	catalog: Option<&PluginCatalog>,
	owner: Option<TrackId>,
) -> Vec<Choice> {
	let Some(channel) = project.channel(owner) else {
		return Vec::new();
	};
	let mix = "Mixer".to_owned();
	let mut out = vec![
		Choice {
			address: ParamAddress {
				track: owner,
				target: ParamTarget::Volume,
			},
			group: mix.clone(),
			name: "Volume".into(),
		},
		Choice {
			address: ParamAddress {
				track: owner,
				target: ParamTarget::Pan,
			},
			group: mix.clone(),
			name: "Pan".into(),
		},
	];
	if let Some(t) = owner {
		for s in &channel.sends {
			let bus = project.track(s.target).map_or("?", |t| t.name.as_str());
			out.push(Choice {
				address: ParamAddress::send(t, s.id),
				group: mix.clone(),
				name: format!("Send → {bus}"),
			});
		}
	}
	let instrument = owner
		.and_then(|t| project.track(t))
		.and_then(|t| t.instrument.as_ref());
	for d in instrument.into_iter().chain(channel.inserts.iter()) {
		for (id, m) in device_params(d, catalog) {
			out.push(Choice {
				address: ParamAddress::device(owner, d.id, id),
				group: d.name.clone(),
				name: m.name,
			});
		}
	}
	out
}

/// Current static (non-automated) value of `address` in the project.
pub fn static_value(
	project: &Project,
	catalog: Option<&PluginCatalog>,
	address: &ParamAddress,
) -> Option<f32> {
	let channel = project.channel(address.track)?;
	match &address.target {
		ParamTarget::Volume => Some(channel.volume_db),
		ParamTarget::Pan => Some(channel.pan),
		ParamTarget::Send { send } => channel
			.sends
			.iter()
			.find(|s| s.id == *send)
			.map(|s| s.level_db),
		ParamTarget::Device { device, param } => {
			let d = project.device(address.track, *device)?;
			d.params
				.get(param)
				.copied()
				.or_else(|| meta(project, catalog, address).map(|m| m.default))
		}
	}
}

/// Points of the lane for `address` (empty without a lane).
pub fn points(project: &Project, address: &ParamAddress) -> Vec<AutomationPoint> {
	project
		.automation_lane(address)
		.map(|l| l.points.clone())
		.unwrap_or_default()
}

fn set(address: &ParamAddress, mut points: Vec<AutomationPoint>) -> Command {
	points.sort_by_key(|p| p.tick);
	Command::SetAutomation {
		address: address.clone(),
		points,
	}
}

/// Adds (or replaces, at the same tick) a point. The new point takes the shape of the
/// segment it lands in.
pub fn add_point(project: &Project, address: &ParamAddress, tick: Tick, value: f32) -> Command {
	let mut pts = points(project, address);
	let shape = pts
		.iter()
		.rev()
		.find(|p| p.tick <= tick)
		.map_or(PointShape::Linear, |p| p.shape);
	pts.retain(|p| p.tick != tick);
	pts.push(AutomationPoint::new(tick, value).with_shape(shape));
	set(address, pts)
}

/// Moves the points at `ticks` by `dt` and their values by `dnorm` (in 0..1 lane units,
/// through `meta`). Moved points replace unselected points they land on. Returns the
/// command and the new ticks of the moved points.
pub fn move_points(
	project: &Project,
	address: &ParamAddress,
	meta: &ParamMeta,
	ticks: &[Tick],
	dt: Tick,
	dnorm: f32,
) -> Option<(Command, Vec<Tick>)> {
	let pts = points(project, address);
	let moved_preview = moved(&pts, meta, ticks, dt, dnorm);
	let new_ticks: Vec<Tick> = moved_preview
		.iter()
		.filter(|(_, sel)| *sel)
		.map(|(p, _)| p.tick)
		.collect();
	let result: Vec<AutomationPoint> = moved_preview.into_iter().map(|(p, _)| p).collect();
	(result != pts).then(|| (set(address, result), new_ticks))
}

/// The lane after a move (for drawing the preview): `(point, was selected)`.
pub fn moved(
	pts: &[AutomationPoint],
	meta: &ParamMeta,
	ticks: &[Tick],
	dt: Tick,
	dnorm: f32,
) -> Vec<(AutomationPoint, bool)> {
	// Don't move the earliest selected point before 0.
	let earliest = pts
		.iter()
		.filter(|p| ticks.contains(&p.tick))
		.map(|p| p.tick)
		.min()
		.unwrap_or(Tick::ZERO);
	let dt = Tick(dt.0.max(-earliest.0));
	let mut selected: Vec<AutomationPoint> = pts
		.iter()
		.filter(|p| ticks.contains(&p.tick))
		.map(|p| AutomationPoint {
			tick: p.tick + dt,
			value: meta.from_norm(meta.to_norm(p.value) + dnorm),
			shape: p.shape,
		})
		.collect();
	selected.dedup_by_key(|p| p.tick);
	let mut out: Vec<(AutomationPoint, bool)> = pts
		.iter()
		.filter(|p| !ticks.contains(&p.tick) && !selected.iter().any(|s| s.tick == p.tick))
		.map(|p| (*p, false))
		.collect();
	out.extend(selected.into_iter().map(|p| (p, true)));
	out.sort_by_key(|(p, _)| p.tick);
	out
}

/// Removes the points at `ticks` (the whole lane when none remain).
pub fn delete_points(project: &Project, address: &ParamAddress, ticks: &[Tick]) -> Option<Command> {
	let pts = points(project, address);
	let kept: Vec<_> = pts
		.iter()
		.filter(|p| !ticks.contains(&p.tick))
		.copied()
		.collect();
	(kept.len() != pts.len()).then(|| set(address, kept))
}

/// Sets the segment shape of the points at `ticks`.
pub fn set_shape(
	project: &Project,
	address: &ParamAddress,
	ticks: &[Tick],
	shape: PointShape,
) -> Option<Command> {
	let pts = points(project, address);
	let changed: Vec<_> = pts
		.iter()
		.map(|p| {
			if ticks.contains(&p.tick) {
				p.with_shape(shape)
			} else {
				*p
			}
		})
		.collect();
	(changed != pts).then(|| set(address, changed))
}

/// A straight ramp of points from `(t0, v0)` to `(t1, v1)` every `step` ticks (line
/// tool): replaces the points in between.
pub fn draw_line(
	project: &Project,
	address: &ParamAddress,
	(t0, v0): (Tick, f32),
	(t1, v1): (Tick, f32),
) -> Command {
	let ((t0, v0), (t1, v1)) = if t1 < t0 {
		((t1, v1), (t0, v0))
	} else {
		((t0, v0), (t1, v1))
	};
	let mut pts: Vec<_> = points(project, address)
		.into_iter()
		.filter(|p| p.tick < t0 || p.tick > t1)
		.collect();
	pts.push(AutomationPoint::new(t0, v0));
	if t1 > t0 {
		pts.push(AutomationPoint::new(t1, v1));
	}
	set(address, pts)
}

/// Ticks of the points inside `t0..=t1` whose normalized value is inside `n0..=n1`
/// (rubber band).
pub fn points_in(
	pts: &[AutomationPoint],
	meta: &ParamMeta,
	(t0, t1): (Tick, Tick),
	(n0, n1): (f32, f32),
) -> Vec<Tick> {
	let (t0, t1) = (t0.min(t1), t0.max(t1));
	let (n0, n1) = (n0.min(n1), n0.max(n1));
	pts.iter()
		.filter(|p| {
			let n = meta.to_norm(p.value);
			p.tick >= t0 && p.tick <= t1 && n >= n0 && n <= n1
		})
		.map(|p| p.tick)
		.collect()
}

// --- Tempo lane --------------------------------------------------------------------

pub fn tempo_norm(bpm: f64) -> f32 {
	((bpm - TEMPO_MIN) / (TEMPO_MAX - TEMPO_MIN)).clamp(0.0, 1.0) as f32
}

pub fn tempo_from_norm(n: f32) -> f64 {
	let bpm = TEMPO_MIN + f64::from(n.clamp(0.0, 1.0)) * (TEMPO_MAX - TEMPO_MIN);
	(bpm * 10.0).round() / 10.0
}

/// Adds a tempo change at `tick` (replacing one there).
pub fn add_tempo(tick: Tick, bpm: f64) -> Command {
	Command::SetTempo { tick, bpm }
}

/// Moves the tempo point at `from` to `(to, bpm)`. The first point (tick 0) only changes
/// its tempo. One undo step.
pub fn move_tempo(project: &Project, from: Tick, to: Tick, bpm: f64) -> Option<Command> {
	let points = project.tempo.points();
	let old = points.iter().find(|p| p.tick == from)?;
	let to = if from == Tick::ZERO {
		Tick::ZERO
	} else {
		Tick(to.0.max(1))
	};
	if to == from && (bpm - old.bpm).abs() < 1e-9 {
		return None;
	}
	if to == from {
		return Some(Command::SetTempo { tick: to, bpm });
	}
	let mut commands = vec![Command::RemoveTempoPoint { tick: from }];
	commands.push(Command::SetTempo { tick: to, bpm });
	Some(Command::Batch {
		label: "Move tempo change".into(),
		commands,
	})
}

/// Removes a tempo change (never the first point).
pub fn delete_tempo(tick: Tick) -> Option<Command> {
	(tick != Tick::ZERO).then_some(Command::RemoveTempoPoint { tick })
}

/// The tempo point in effect at `tick` (the BPM field edits it).
pub fn tempo_segment(project: &Project, tick: Tick) -> TempoPoint {
	let points = project.tempo.points();
	let i = points.partition_point(|p| p.tick <= tick);
	points[i.saturating_sub(1)]
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::DeviceId;
	use cadenza_core::PPQ;
	use cadenza_model::{Device, Document, Send, Track, TrackKind};

	fn project() -> (Project, TrackId, DeviceId, DeviceId) {
		let mut p = Project::new("auto");
		let bus = Track::new("Verb", TrackKind::Bus);
		let mut t = Track::new("Synth", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		let s = Send::new(bus.id, -12.0);
		let (sid, did) = (s.id, t.instrument.as_ref().unwrap().id);
		t.channel.sends.push(s);
		let id = t.id;
		p.tracks = vec![t, bus];
		(p, id, did, sid)
	}

	fn exec(p: &mut Project, cmd: Command) {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		*p = doc.into_project();
	}

	macro_rules! exec {
		($p:ident, $e:expr $(,)?) => {{
			let c = $e;
			exec(&mut $p, c);
		}};
	}

	fn beats(b: f64) -> Tick {
		Tick::from_beats(b)
	}

	#[test]
	fn picker_lists_mix_sends_and_device_params() {
		let (p, t, synth, send) = project();
		let c = choices(&p, None, Some(t));
		assert_eq!(c[0].name, "Volume");
		assert_eq!(c[1].name, "Pan");
		assert_eq!(c[2].address, ParamAddress::send(t, send));
		assert_eq!(c[2].name, "Send → Verb");
		assert!(c.iter().any(|c| c.address
			== ParamAddress::device(Some(t), synth, "filter_cutoff")
			&& c.group == "Synth"));
		let master = choices(&p, None, None);
		assert_eq!(master.len(), 2, "master: volume and pan only");
		let m = meta(
			&p,
			None,
			&ParamAddress::device(Some(t), synth, "filter_cutoff"),
		)
		.unwrap();
		assert_eq!(m.scale, Scale::Log);
		assert_eq!(m.format(1500.0), "1.50 kHz");
		let vol = meta(&p, None, &ParamAddress::track_volume(t)).unwrap();
		assert_eq!(vol.format(-6.0), "-6.0 dB");
		assert!((vol.from_norm(vol.to_norm(-6.0)) + 6.0).abs() < 1e-3);
		let pan = meta(&p, None, &ParamAddress::track_pan(t)).unwrap();
		assert_eq!(pan.format(-0.3), "L30");
		assert_eq!(
			static_value(&p, None, &ParamAddress::send(t, send)),
			Some(-12.0)
		);
	}

	#[test]
	fn point_gestures_are_single_commands() {
		let (mut p, t, _, _) = project();
		let a = ParamAddress::track_pan(t);
		let m = meta(&p, None, &a).unwrap();
		exec!(p, add_point(&p, &a, beats(0.0), -1.0));
		exec!(p, add_point(&p, &a, beats(4.0), 1.0));
		exec!(p, add_point(&p, &a, beats(8.0), 0.0));
		assert_eq!(points(&p, &a).len(), 3);
		// Re-adding at the same tick replaces.
		exec!(p, add_point(&p, &a, beats(8.0), 0.5));
		assert_eq!(points(&p, &a).len(), 3);

		// Move two points one beat later and up a quarter of the lane.
		let (cmd, ticks) =
			move_points(&p, &a, &m, &[beats(4.0), beats(8.0)], beats(1.0), 0.25).unwrap();
		exec!(p, cmd);
		assert_eq!(ticks, vec![beats(5.0), beats(9.0)]);
		let pts = points(&p, &a);
		assert_eq!(pts[1].value, 1.0, "clamped at the top");
		assert!((pts[2].value - 1.0).abs() < 1e-6);
		// Moving onto an unselected point replaces it; never before 0.
		let (cmd, ticks) = move_points(&p, &a, &m, &[beats(5.0)], beats(-10.0), 0.0).unwrap();
		exec!(p, cmd);
		assert_eq!(ticks, vec![Tick::ZERO]);
		assert_eq!(points(&p, &a).len(), 2);
		assert!(move_points(&p, &a, &m, &[Tick::ZERO], Tick::ZERO, 0.0).is_none());

		exec!(
			p,
			set_shape(&p, &a, &[Tick::ZERO], PointShape::Hold).unwrap(),
		);
		assert_eq!(points(&p, &a)[0].shape, PointShape::Hold);
		// A point added in a held segment holds too.
		exec!(p, add_point(&p, &a, beats(2.0), 0.0));
		assert_eq!(points(&p, &a)[1].shape, PointShape::Hold);

		let band = points_in(&points(&p, &a), &m, (beats(1.0), beats(10.0)), (0.0, 1.0));
		assert_eq!(band, vec![beats(2.0), beats(9.0)]);
		exec!(p, delete_points(&p, &a, &band).unwrap());
		assert_eq!(points(&p, &a).len(), 1);
		exec!(p, delete_points(&p, &a, &[Tick::ZERO]).unwrap());
		assert!(p.automation_lane(&a).is_none(), "empty lane removed");

		exec!(p, draw_line(&p, &a, (beats(4.0), 1.0), (beats(0.0), -1.0)),);
		let pts = points(&p, &a);
		assert_eq!((pts[0].tick, pts[0].value), (Tick::ZERO, -1.0));
		assert_eq!((pts[1].tick, pts[1].value), (beats(4.0), 1.0));
	}

	#[test]
	fn lanes_shown_follow_the_project() {
		let (mut p, t, synth, send) = project();
		let mut v = AutomationView::default();
		v.toggle(&p, Some(t));
		assert!(v.is_open(Some(t)));
		assert_eq!(
			v.visible(&p, Some(t)),
			vec![ParamAddress::track_volume(t)],
			"an empty channel opens on its volume"
		);
		let cutoff = ParamAddress::device(Some(t), synth, "filter_cutoff");
		exec!(p, add_point(&p, &cutoff, Tick(0), 500.0));
		assert_eq!(v.visible(&p, Some(t)).len(), 2);
		v.replace(&ParamAddress::track_volume(t), ParamAddress::send(t, send));
		assert_eq!(v.visible(&p, Some(t))[0], ParamAddress::send(t, send));
		// Removing the send hides its lane.
		exec!(p, Command::RemoveSend { track: t, send });
		v.prune(&p);
		assert_eq!(v.visible(&p, Some(t)), vec![cutoff.clone()]);
		v.selected.push((cutoff.clone(), Tick(0)));
		v.selected.push((cutoff.clone(), Tick(5)));
		v.prune(&p);
		assert_eq!(v.selected.len(), 1);
		v.toggle(&p, None);
		assert!(v.master && v.is_open(None));
	}

	#[test]
	fn tempo_points_edit_as_steps() {
		let (mut p, _, _, _) = project();
		exec!(p, add_tempo(Tick(PPQ * 16), 90.0));
		assert_eq!(tempo_segment(&p, Tick(PPQ * 20)).bpm, 90.0);
		assert_eq!(tempo_segment(&p, Tick(PPQ * 2)).tick, Tick::ZERO);
		exec!(
			p,
			move_tempo(&p, Tick(PPQ * 16), Tick(PPQ * 8), 100.0).unwrap(),
		);
		assert_eq!(p.tempo.points()[1].tick, Tick(PPQ * 8));
		assert_eq!(p.tempo.points()[1].bpm, 100.0);
		// The first point keeps its tick.
		exec!(p, move_tempo(&p, Tick::ZERO, Tick(PPQ), 130.0).unwrap());
		assert_eq!(p.tempo.points()[0].tick, Tick::ZERO);
		assert_eq!(p.tempo.points()[0].bpm, 130.0);
		assert!(delete_tempo(Tick::ZERO).is_none());
		exec!(p, delete_tempo(Tick(PPQ * 8)).unwrap());
		assert_eq!(p.tempo.points().len(), 1);
		assert_eq!(tempo_from_norm(tempo_norm(123.4)), 123.4);
	}
}
