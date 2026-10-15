//! Automation recording ("write"): moving a mixer / inspector control or a plugin editor
//! knob during playback writes its values into the parameter's automation lane.
//!
//! Modes (global, like a DAW's automation mode): `Touch` writes while a control is held
//! and returns to the existing curve on release; `Latch` keeps writing the last value
//! from the release until playback stops. Values are sampled per UI frame on the song
//! position, thinned on commit, and every lane written in one playback pass lands as ONE
//! undo step ("Write automation"). While a lane is being written its playback is suspended
//! so the moving control is what is heard.

use cadenza_core::{ParamAddress, ParamTarget, TempoMap, Tick};
use cadenza_model::{AutomationPoint, Command, Project, automation_value};
use serde::{Deserialize, Serialize};

/// Automation mode of the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WriteMode {
	/// Read: automation plays, control moves do not write.
	#[default]
	Off,
	Touch,
	Latch,
}

impl WriteMode {
	pub fn name(self) -> &'static str {
		match self {
			WriteMode::Off => "Read",
			WriteMode::Touch => "Touch",
			WriteMode::Latch => "Latch",
		}
	}
}

/// One control written in the current pass.
#[derive(Debug, Clone, PartialEq)]
struct Pass {
	address: ParamAddress,
	/// Contiguous stretches of (tick, value); a loop wrap starts a new one.
	segments: Vec<Vec<(Tick, f32)>>,
	/// The control is held (Touch and Latch) or latched after release (Latch).
	writing: bool,
	/// Released in Touch mode: where the existing curve takes over again.
	released_at: Option<Tick>,
	last: f32,
}

/// The automation write state of a session.
#[derive(Debug, Clone, Default)]
pub struct Writer {
	pub mode: WriteMode,
	passes: Vec<Pass>,
	last_tick: Option<Tick>,
}

impl Writer {
	/// Whether control moves write now.
	pub fn armed(&self, playing: bool) -> bool {
		playing && self.mode != WriteMode::Off
	}

	/// Whether anything was written in this pass.
	pub fn has_pass(&self) -> bool {
		!self.passes.is_empty()
	}

	/// Addresses whose lane playback must be suspended (being written).
	pub fn suspended(&self) -> Vec<ParamAddress> {
		self.passes
			.iter()
			.filter(|p| p.writing)
			.map(|p| p.address.clone())
			.collect()
	}

	/// Whether `address` is being written (its static value must not be committed).
	pub fn owns(&self, address: &ParamAddress) -> bool {
		self.passes.iter().any(|p| &p.address == address)
	}

	/// A control moved to `value` at `tick` while writing.
	pub fn touch(&mut self, address: &ParamAddress, value: f32, tick: Tick) {
		if !value.is_finite() {
			return;
		}
		let pass = match self.passes.iter_mut().position(|p| &p.address == address) {
			Some(i) => &mut self.passes[i],
			None => {
				self.passes.push(Pass {
					address: address.clone(),
					segments: vec![Vec::new()],
					writing: true,
					released_at: None,
					last: value,
				});
				self.passes.last_mut().expect("just pushed")
			}
		};
		if !pass.writing {
			// Touched again after a Touch release: a new stretch.
			pass.writing = true;
			pass.released_at = None;
			pass.segments.push(Vec::new());
		}
		pass.last = value;
		push_sample(pass, tick, value);
	}

	/// The control was let go at `tick`.
	pub fn release(&mut self, address: &ParamAddress, tick: Tick) {
		let mode = self.mode;
		if let Some(pass) = self.passes.iter_mut().find(|p| &p.address == address) {
			let v = pass.last;
			push_sample(pass, tick, v);
			if mode != WriteMode::Latch {
				pass.writing = false;
				pass.released_at = Some(tick);
			}
		}
	}

	/// Advances with the playhead each frame: latched controls keep writing their value; a
	/// jump back (loop wrap) starts new stretches.
	pub fn advance(&mut self, tick: Tick) {
		let wrapped = self.last_tick.is_some_and(|t| tick < t);
		self.last_tick = Some(tick);
		for pass in &mut self.passes {
			if wrapped {
				pass.segments.push(Vec::new());
			}
			if pass.writing {
				let v = pass.last;
				push_sample(pass, tick, v);
			}
		}
	}

	/// Ends the pass (playback stopped or the mode changed): the commands that write the
	/// lanes, as one undo step. Clears the pass.
	pub fn finish(&mut self, project: &Project) -> Option<Command> {
		self.last_tick = None;
		let passes = std::mem::take(&mut self.passes);
		let commands: Vec<Command> = passes
			.into_iter()
			.filter_map(|pass| lane_command(project, &pass))
			.collect();
		match commands.len() {
			0 => None,
			_ => Some(Command::Batch {
				label: "Write automation".into(),
				commands,
			}),
		}
	}
}

fn push_sample(pass: &mut Pass, tick: Tick, value: f32) {
	let seg = match pass.segments.last_mut() {
		Some(s) => s,
		None => {
			pass.segments.push(Vec::new());
			pass.segments.last_mut().expect("just pushed")
		}
	};
	match seg.last_mut() {
		Some(last) if last.0 == tick => last.1 = value,
		Some(last) if last.0 > tick => {}
		_ => seg.push((tick, value)),
	}
}

/// The value `address` has without automation (the channel / device setting).
fn static_value(project: &Project, address: &ParamAddress) -> Option<f32> {
	match &address.target {
		ParamTarget::Volume => project.channel(address.track).map(|c| c.volume_db),
		ParamTarget::Pan => project.channel(address.track).map(|c| c.pan),
		ParamTarget::Send { send } => address
			.track
			.and_then(|t| project.track(t))
			.and_then(|t| t.channel.sends.iter().find(|s| s.id == *send))
			.map(|s| s.level_db),
		ParamTarget::Device { device, param } => project
			.device(address.track, *device)
			.and_then(|d| d.params.get(param).copied()),
	}
}

/// The lane after writing `pass`: each stretch replaces the points inside it with its
/// thinned samples; a Touch release returns to the old curve (or the static value).
fn lane_command(project: &Project, pass: &Pass) -> Option<Command> {
	let old: Vec<AutomationPoint> = project
		.automation
		.iter()
		.find(|l| l.address == pass.address)
		.map(|l| l.points.clone())
		.unwrap_or_default();
	let fallback = static_value(project, &pass.address);
	let value_at = |points: &[AutomationPoint], tick: Tick, tempo: &TempoMap| {
		automation_value(points, tick, tempo).or(fallback)
	};
	let mut points = old.clone();
	let mut wrote = false;
	let last_segment = pass.segments.iter().rposition(|s| !s.is_empty());
	for (i, seg) in pass.segments.iter().enumerate() {
		let (Some(first), Some(last)) = (seg.first(), seg.last()) else {
			continue;
		};
		let (t0, t1) = (first.0, last.0);
		// The curve resumes right after a released stretch (Touch).
		let resume = (Some(i) != last_segment || pass.released_at.is_some())
			.then(|| value_at(&old, t1 + Tick(1), &project.tempo))
			.flatten();
		points.retain(|p| p.tick < t0 || p.tick > t1);
		points.extend(
			thin(seg)
				.into_iter()
				.map(|(t, v)| AutomationPoint::new(t, v)),
		);
		if let Some(v) = resume
			&& !old.is_empty()
			&& !points.iter().any(|p| p.tick == t1 + Tick(1))
		{
			points.push(AutomationPoint::new(t1 + Tick(1), v));
		}
		wrote = true;
	}
	if !wrote {
		return None;
	}
	points.sort_by_key(|p| p.tick);
	points.dedup_by_key(|p| p.tick);
	(points != old).then(|| Command::SetAutomation {
		address: pass.address.clone(),
		points,
	})
}

/// Drops samples a straight line between their neighbours already reproduces (within 1 %
/// of the stretch's value range), keeping both ends: a held control becomes two points, a
/// sweep a handful.
pub fn thin(samples: &[(Tick, f32)]) -> Vec<(Tick, f32)> {
	if samples.len() <= 2 {
		return samples.to_vec();
	}
	let (lo, hi) = samples
		.iter()
		.fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
			(lo.min(s.1), hi.max(s.1))
		});
	let eps = ((hi - lo) * 0.01).max(1e-4);
	let mut keep = vec![false; samples.len()];
	keep[0] = true;
	*keep.last_mut().expect("non-empty") = true;
	rdp(samples, 0, samples.len() - 1, eps, &mut keep);
	samples
		.iter()
		.zip(keep)
		.filter_map(|(s, k)| k.then_some(*s))
		.collect()
}

fn rdp(s: &[(Tick, f32)], a: usize, b: usize, eps: f32, keep: &mut [bool]) {
	if b <= a + 1 {
		return;
	}
	let (ta, va) = (s[a].0.0 as f32, s[a].1);
	let (tb, vb) = (s[b].0.0 as f32, s[b].1);
	let mut worst = (0.0f32, a);
	for (i, p) in s.iter().enumerate().take(b).skip(a + 1) {
		let t = p.0.0 as f32;
		let line = if tb > ta {
			va + (vb - va) * (t - ta) / (tb - ta)
		} else {
			va
		};
		let d = (p.1 - line).abs();
		if d > worst.0 {
			worst = (d, i);
		}
	}
	if worst.0 > eps {
		keep[worst.1] = true;
		rdp(s, a, worst.1, eps, keep);
		rdp(s, worst.1, b, eps, keep);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::{Document, Track, TrackKind};

	fn song() -> (Project, ParamAddress) {
		let mut p = Project::new("w");
		let t = Track::new("Lead", TrackKind::Audio);
		let address = ParamAddress::track_volume(t.id);
		p.tracks.push(t);
		(p, address)
	}

	#[test]
	fn thinning_keeps_shape_and_ends() {
		let flat: Vec<(Tick, f32)> = (0..50).map(|i| (Tick(i * 10), -6.0)).collect();
		assert_eq!(thin(&flat).len(), 2);
		let ramp: Vec<(Tick, f32)> = (0..50).map(|i| (Tick(i * 10), i as f32)).collect();
		assert_eq!(thin(&ramp).len(), 2, "a straight ramp needs its ends only");
		let mut vee = ramp.clone();
		vee.extend((0..50).map(|i| (Tick(500 + i * 10), 49.0 - i as f32)));
		let t = thin(&vee);
		assert_eq!(
			t.len(),
			4,
			"the corner (two samples at the top) and both ends: {t:?}"
		);
	}

	#[test]
	fn touch_writes_while_held_and_returns_to_the_curve() {
		let (mut p, address) = song();
		p.automation.push(cadenza_model::AutomationLane::new(
			address.clone(),
			vec![AutomationPoint::new(Tick(0), -10.0)],
		));
		let mut w = Writer {
			mode: WriteMode::Touch,
			..Writer::default()
		};
		w.advance(Tick(0));
		for i in 0..=10 {
			let t = Tick(PPQ * 4 + i * 96);
			w.advance(t);
			w.touch(&address, -10.0 + i as f32, t);
		}
		w.release(&address, Tick(PPQ * 5));
		assert!(
			w.suspended().is_empty(),
			"touch: released controls read again"
		);
		w.advance(Tick(PPQ * 8));
		let cmd = w.finish(&p).unwrap();
		let mut doc = Document::new(p);
		doc.execute(cmd).unwrap();
		assert_eq!(doc.history_labels(), vec!["Write automation"]);
		let lane = &doc.project().automation[0].points;
		assert_eq!(
			lane.first().unwrap().value,
			-10.0,
			"before the pass: untouched"
		);
		assert!(lane.iter().any(|p| p.tick == Tick(PPQ * 4)));
		let back = lane.last().unwrap();
		assert_eq!((back.tick, back.value), (Tick(PPQ * 5 + 1), -10.0));
		assert!(lane.len() <= 5, "thinned: {lane:?}");
	}

	#[test]
	fn latch_keeps_writing_until_stop_and_new_lanes_are_created() {
		let (p, address) = song();
		let mut w = Writer {
			mode: WriteMode::Latch,
			..Writer::default()
		};
		w.advance(Tick(0));
		w.touch(&address, -3.0, Tick(PPQ));
		w.release(&address, Tick(PPQ * 2));
		assert_eq!(w.suspended(), vec![address.clone()], "latched");
		w.advance(Tick(PPQ * 6));
		let cmd = w.finish(&p).unwrap();
		let Command::Batch { commands, .. } = &cmd else {
			panic!("one batch")
		};
		let Command::SetAutomation { points, .. } = &commands[0] else {
			panic!("a lane")
		};
		assert_eq!(points.first().unwrap().tick, Tick(PPQ));
		assert_eq!(points.last().unwrap().tick, Tick(PPQ * 6));
		assert!(points.iter().all(|p| p.value == -3.0));
		assert!(!w.has_pass());
	}

	#[test]
	fn loop_wraps_write_the_later_pass_over_the_earlier() {
		let (p, address) = song();
		let mut w = Writer {
			mode: WriteMode::Touch,
			..Writer::default()
		};
		w.advance(Tick(PPQ));
		w.touch(&address, -1.0, Tick(PPQ));
		w.advance(Tick(PPQ * 2));
		w.touch(&address, -1.0, Tick(PPQ * 2));
		// Wrap back to the loop start, still holding the control.
		w.advance(Tick(PPQ));
		w.touch(&address, -9.0, Tick(PPQ));
		w.advance(Tick(PPQ * 2));
		w.touch(&address, -9.0, Tick(PPQ * 2));
		w.release(&address, Tick(PPQ * 2));
		let Some(Command::Batch { commands, .. }) = w.finish(&p) else {
			panic!("a batch")
		};
		let Command::SetAutomation { points, .. } = &commands[0] else {
			panic!("a lane")
		};
		assert!(points.iter().all(|p| p.value == -9.0), "{points:?}");
	}
}
