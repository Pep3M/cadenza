//! Plugin editor windows as seen by the app: which are open, and how what the user does
//! in them becomes project commands (pure logic, no audio, unit-tested).
//!
//! - A gesture (grab a knob, drag, release) is heard live while it runs (the plugin
//!   already plays its own value) and shown by the inspector, then committed as ONE
//!   undoable `SetParam` on release.
//! - Values outside a gesture (mouse wheel, typed values, MIDI learn...) are gathered per
//!   device and committed after a short quiet period: a few parameters as one `Batch`
//!   of `SetParam`s; a burst touching many parameters (a patch loaded in the plugin's
//!   own browser) as the plugin's captured state instead (`SetDeviceState`), so the
//!   project does not fill up with hundreds of explicit parameters.
//! - Plugin state changes that are not parameters ask for a state capture; captured
//!   states that differ from the project are stored with one `SetDeviceState`. They are
//!   what the running instance already holds, so storing them needs no reload
//!   ([`PluginEditors::live_state`], consulted by `live::diff`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use cadenza_core::{DeviceId, ParamAddress, TrackId};
use cadenza_engine::{EditorEvent, EditorState, EditorUpdate};
use cadenza_model::{Command, Device, Project};

/// One-off values are committed after this long without another one for the device.
pub const BURST_QUIET: Duration = Duration::from_millis(350);
/// A burst touching more parameters than this is stored as the plugin's state.
pub const BURST_MAX_PARAMS: usize = 16;
/// State-change notifications are captured after this long without another one.
pub const STATE_QUIET: Duration = Duration::from_millis(500);

/// What the app must do after handling editor updates.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Actions {
	/// Commands to apply, in order.
	pub commands: Vec<Command>,
	/// Devices whose live state should be captured now.
	pub capture: Vec<DeviceId>,
	/// Editors that closed (by the user or because their plugin went away).
	pub closed: Vec<DeviceId>,
}

#[derive(Debug, Default)]
struct Burst {
	values: BTreeMap<String, f32>,
	last: Option<Instant>,
}

/// Open plugin editors and the edits in flight.
#[derive(Debug, Default)]
pub struct PluginEditors {
	/// Devices whose editor is open (or being opened).
	open: HashSet<DeviceId>,
	/// Editors asked for before the device reached the player (reload pending): title.
	queued: Vec<(DeviceId, String)>,
	/// Last value each editor reported per parameter (shown by the inspector until the
	/// project agrees).
	live: HashMap<(DeviceId, String), f32>,
	/// Gestures in progress and their latest value.
	gestures: HashMap<(DeviceId, String), Option<f32>>,
	bursts: HashMap<DeviceId, Burst>,
	/// Values to overlay on a device's explicit params when its captured state arrives
	/// (bursts too large for `SetParam`s).
	overlays: HashMap<DeviceId, BTreeMap<String, f32>>,
	/// State changes waiting for a capture, with the time of the last notification.
	state_changes: HashMap<DeviceId, Instant>,
	/// Plugin states the running instances hold beyond what the player last loaded.
	live_states: HashMap<DeviceId, String>,
}

/// Where `device` is: `Some(track)` (`Some(None)` = master), `None` when it is gone.
pub fn locate(project: &Project, device: DeviceId) -> Option<Option<TrackId>> {
	project
		.tracks
		.iter()
		.find(|t| t.device(device).is_some())
		.map(|t| Some(t.id))
		.or_else(|| {
			project
				.master
				.inserts
				.iter()
				.any(|d| d.id == device)
				.then_some(None)
		})
}

fn find_device(project: &Project, device: DeviceId) -> Option<(Option<TrackId>, &Device)> {
	let track = locate(project, device)?;
	Some((track, project.device(track, device)?))
}

/// Editor window title: "<device> — <track>" (or "— Master").
pub fn title(project: &Project, device: DeviceId) -> Option<String> {
	let (track, d) = find_device(project, device)?;
	let place = match track {
		Some(t) => project.track(t)?.name.clone(),
		None => "Master".to_owned(),
	};
	Some(format!("{} — {place}", d.name))
}

impl PluginEditors {
	pub fn is_open(&self, device: DeviceId) -> bool {
		self.open.contains(&device)
	}

	pub fn any_open(&self) -> bool {
		!self.open.is_empty()
	}

	pub fn open_devices(&self) -> impl Iterator<Item = DeviceId> + '_ {
		self.open.iter().copied()
	}

	pub fn opened(&mut self, device: DeviceId) {
		self.open.insert(device);
	}

	/// The editor is gone (closed by the app, the user or with its plugin): pending
	/// gestures end without a command; queued one-off values still commit.
	pub fn closed(&mut self, device: DeviceId) {
		self.open.remove(&device);
		self.queued.retain(|(d, _)| *d != device);
		self.gestures.retain(|(d, _), _| *d != device);
	}

	/// Remembers an editor to open once the device is loaded.
	pub fn queue(&mut self, device: DeviceId, title: String) {
		self.open.insert(device);
		self.queued.retain(|(d, _)| *d != device);
		self.queued.push((device, title));
	}

	pub fn take_queued(&mut self) -> Vec<(DeviceId, String)> {
		std::mem::take(&mut self.queued)
	}

	/// The value an editor reported for a parameter the project does not show yet.
	pub fn live_value(&self, device: DeviceId, param: &str) -> Option<f32> {
		self.live.get(&(device, param.to_owned())).copied()
	}

	/// Whether `state` is what the running instance of `device` holds (no reload
	/// needed to store it).
	pub fn live_state(&self, device: DeviceId, state: Option<&str>) -> bool {
		state.is_some() && self.live_states.get(&device).map(String::as_str) == state
	}

	/// A reload re-creates every instance from the project: nothing is live-only any
	/// more.
	pub fn reloaded(&mut self) {
		self.live_states.clear();
	}

	/// Handles updates from the player.
	pub fn handle(
		&mut self,
		project: &Project,
		updates: Vec<EditorUpdate>,
		now: Instant,
	) -> Actions {
		let mut out = Actions::default();
		for EditorUpdate { device, event } in updates {
			match event {
				EditorEvent::GestureBegin { param } => {
					self.gestures.insert((device, param), None);
				}
				EditorEvent::Value { param, value } => {
					let value = value as f32;
					if !value.is_finite() {
						continue;
					}
					self.live.insert((device, param.clone()), value);
					match self.gestures.get_mut(&(device, param.clone())) {
						Some(last) => *last = Some(value),
						None => {
							let burst = self.bursts.entry(device).or_default();
							burst.values.insert(param, value);
							burst.last = Some(now);
						}
					}
				}
				EditorEvent::GestureEnd { param } => {
					if let Some(Some(value)) = self.gestures.remove(&(device, param.clone()))
						&& let Some(cmd) = set_param(project, device, &param, value)
					{
						out.commands.push(cmd);
					}
				}
				EditorEvent::StateChanged => {
					self.state_changes.insert(device, now);
				}
				EditorEvent::Closed => {
					self.closed(device);
					out.closed.push(device);
					// Whatever changed in the editor is captured on the way out.
					self.state_changes.remove(&device);
					out.capture.push(device);
				}
			}
		}
		out
	}

	/// Commits one-off values and asks for state captures whose quiet period is over
	/// (`force`: all of them, e.g. before saving).
	pub fn flush(&mut self, project: &Project, now: Instant, force: bool) -> Actions {
		let mut out = Actions::default();
		let due: Vec<DeviceId> = self
			.bursts
			.iter()
			.filter(|(_, b)| {
				force
					|| b.last
						.is_some_and(|t| now.saturating_duration_since(t) >= BURST_QUIET)
			})
			.map(|(d, _)| *d)
			.collect();
		for device in due {
			let Some(burst) = self.bursts.remove(&device) else {
				continue;
			};
			if burst.values.len() > BURST_MAX_PARAMS {
				self.overlays
					.entry(device)
					.or_default()
					.extend(burst.values);
				out.capture.push(device);
				continue;
			}
			let commands: Vec<Command> = burst
				.values
				.iter()
				.filter_map(|(param, &value)| set_param(project, device, param, value))
				.collect();
			match commands.len() {
				0 => {}
				1 => out.commands.extend(commands),
				_ => out.commands.push(Command::Batch {
					label: "Plugin edit".into(),
					commands,
				}),
			}
		}
		let due: Vec<DeviceId> = self
			.state_changes
			.iter()
			.filter(|(_, t)| force || now.saturating_duration_since(**t) >= STATE_QUIET)
			.map(|(d, _)| *d)
			.collect();
		for device in due {
			self.state_changes.remove(&device);
			out.capture.push(device);
		}
		out.capture.sort_unstable_by_key(|d| d.to_string());
		out.capture.dedup();
		// Values the project now agrees with no longer need showing.
		self.live.retain(|(device, param), v| {
			find_device(project, *device).is_some_and(|(_, d)| d.params.get(param) != Some(v))
		});
		out
	}

	/// Commands storing captured live states: one `SetDeviceState` per device whose
	/// state differs from the project (or that has values to overlay).
	pub fn store_states(&mut self, project: &Project, states: Vec<EditorState>) -> Vec<Command> {
		let mut out = Vec::new();
		for s in states {
			let overlay = self.overlays.remove(&s.device);
			let Some((track, d)) = find_device(project, s.device) else {
				continue;
			};
			let params = overlay.map(|values| {
				let mut params = d.params.clone();
				for (k, v) in values {
					if let Some(p) = params.get_mut(&k) {
						*p = v;
					}
				}
				params
			});
			let unchanged_params = params.as_ref().is_none_or(|p| *p == d.params);
			if d.state.as_deref() == Some(s.state.as_str()) && unchanged_params {
				continue;
			}
			self.live_states.insert(s.device, s.state.clone());
			out.push(Command::SetDeviceState {
				track,
				device: s.device,
				state: Some(s.state),
				params: params.filter(|_| !unchanged_params),
			});
		}
		out
	}

	/// Whether something waits for its quiet period (the UI should keep ticking).
	pub fn busy(&self) -> bool {
		!self.bursts.is_empty() || !self.state_changes.is_empty()
	}

	/// Devices with editor activity not yet in the project (captured before saving).
	pub fn needs_capture(&self) -> Vec<DeviceId> {
		let mut devices: Vec<DeviceId> = self
			.open
			.iter()
			.chain(self.state_changes.keys())
			.chain(self.overlays.keys())
			.copied()
			.collect();
		devices.sort_unstable_by_key(|d| d.to_string());
		devices.dedup();
		devices
	}

	/// Forgets editors and edits of devices that no longer exist.
	pub fn prune(&mut self, project: &Project) {
		let alive = |d: &DeviceId| locate(project, *d).is_some();
		self.open.retain(alive);
		self.queued.retain(|(d, _)| alive(d));
		self.live.retain(|(d, _), _| alive(d));
		self.gestures.retain(|(d, _), _| alive(d));
		self.bursts.retain(|d, _| alive(d));
		self.overlays.retain(|d, _| alive(d));
		self.state_changes.retain(|d, _| alive(d));
		self.live_states.retain(|d, _| alive(d));
	}
}

/// `SetParam` for a device parameter, unless the project already has that value.
fn set_param(project: &Project, device: DeviceId, param: &str, value: f32) -> Option<Command> {
	let (track, d) = find_device(project, device)?;
	if d.params.get(param) == Some(&value) {
		return None;
	}
	Some(Command::SetParam {
		address: ParamAddress::device(track, device, param),
		value,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Track, TrackKind};

	fn project() -> (Project, TrackId, DeviceId, DeviceId) {
		let mut p = Project::new("editors");
		let synth = Device::builtin("poly_synth", "Surge XT");
		let fx = Device::builtin("gain", "Comp");
		let (sid, fid) = (synth.id, fx.id);
		let mut t = Track::new("Lead", TrackKind::Instrument).with_instrument(synth);
		t.channel.inserts.push(fx);
		let tid = t.id;
		p.tracks.push(t);
		(p, tid, sid, fid)
	}

	fn up(device: DeviceId, event: EditorEvent) -> EditorUpdate {
		EditorUpdate { device, event }
	}

	fn value(param: &str, value: f64) -> EditorEvent {
		EditorEvent::Value {
			param: param.into(),
			value,
		}
	}

	#[test]
	fn titles_name_device_and_place() {
		let (mut p, _, synth, _) = project();
		assert_eq!(title(&p, synth).unwrap(), "Surge XT — Lead");
		let master_fx = Device::builtin("gain", "Limiter");
		let id = master_fx.id;
		p.master.inserts.push(master_fx);
		assert_eq!(title(&p, id).unwrap(), "Limiter — Master");
		assert_eq!(locate(&p, id), Some(None));
		assert!(title(&p, DeviceId::new()).is_none());
	}

	#[test]
	fn a_gesture_commits_one_set_param_on_release() {
		let (p, track, synth, _) = project();
		let mut e = PluginEditors::default();
		let now = Instant::now();
		let a = e.handle(
			&p,
			vec![
				up(synth, EditorEvent::GestureBegin { param: "7".into() }),
				up(synth, value("7", 0.1)),
				up(synth, value("7", 0.4)),
			],
			now,
		);
		assert!(a.commands.is_empty());
		assert_eq!(e.live_value(synth, "7"), Some(0.4));
		let a = e.handle(
			&p,
			vec![
				up(synth, value("7", 0.5)),
				up(synth, EditorEvent::GestureEnd { param: "7".into() }),
			],
			now,
		);
		assert_eq!(
			a.commands,
			vec![Command::SetParam {
				address: ParamAddress::device(Some(track), synth, "7"),
				value: 0.5,
			}]
		);
		// Values inside the gesture never became one-off edits.
		assert!(
			e.flush(&p, now + BURST_QUIET * 2, false)
				.commands
				.is_empty()
		);
	}

	#[test]
	fn gestures_without_values_or_with_the_current_value_commit_nothing() {
		let (mut p, _, synth, _) = project();
		p.tracks[0]
			.instrument
			.as_mut()
			.unwrap()
			.params
			.insert("7".into(), 0.5);
		let mut e = PluginEditors::default();
		let now = Instant::now();
		let a = e.handle(
			&p,
			vec![
				up(synth, EditorEvent::GestureBegin { param: "1".into() }),
				up(synth, EditorEvent::GestureEnd { param: "1".into() }),
				up(synth, EditorEvent::GestureBegin { param: "7".into() }),
				up(synth, value("7", 0.5)),
				up(synth, EditorEvent::GestureEnd { param: "7".into() }),
			],
			now,
		);
		assert!(a.commands.is_empty());
	}

	#[test]
	fn one_off_values_commit_after_a_quiet_period_as_one_batch() {
		let (p, track, synth, fx) = project();
		let mut e = PluginEditors::default();
		let t0 = Instant::now();
		e.handle(
			&p,
			vec![up(synth, value("1", 0.2)), up(fx, value("3", -4.0))],
			t0,
		);
		e.handle(
			&p,
			vec![up(synth, value("2", 0.8)), up(synth, value("1", 0.25))],
			t0 + BURST_QUIET / 2,
		);
		assert!(e.busy());
		// The effect's burst is due, the synth's is not yet.
		let a = e.flush(&p, t0 + BURST_QUIET, false);
		assert_eq!(a.commands.len(), 1);
		let a = e.flush(&p, t0 + BURST_QUIET * 2, false);
		match &a.commands[..] {
			[Command::Batch { commands, .. }] => assert_eq!(
				commands,
				&vec![
					Command::SetParam {
						address: ParamAddress::device(Some(track), synth, "1"),
						value: 0.25,
					},
					Command::SetParam {
						address: ParamAddress::device(Some(track), synth, "2"),
						value: 0.8,
					},
				]
			),
			other => panic!("{other:?}"),
		}
		assert!(!e.busy());
	}

	#[test]
	fn large_bursts_become_a_state_capture_with_overlaid_params() {
		let (mut p, track, synth, _) = project();
		p.tracks[0]
			.instrument
			.as_mut()
			.unwrap()
			.params
			.insert("5".into(), 0.0);
		let mut e = PluginEditors::default();
		let t0 = Instant::now();
		let updates = (0..=BURST_MAX_PARAMS)
			.map(|i| up(synth, value(&i.to_string(), 0.9)))
			.collect();
		e.handle(&p, updates, t0);
		let a = e.flush(&p, t0 + BURST_QUIET, false);
		assert!(a.commands.is_empty());
		assert_eq!(a.capture, vec![synth]);
		let cmds = e.store_states(
			&p,
			vec![EditorState {
				track: Some(track),
				device: synth,
				state: "STATE".into(),
			}],
		);
		let mut params = BTreeMap::new();
		params.insert("5".to_owned(), 0.9);
		assert_eq!(
			cmds,
			vec![Command::SetDeviceState {
				track: Some(track),
				device: synth,
				state: Some("STATE".into()),
				params: Some(params),
			}]
		);
		assert!(e.live_state(synth, Some("STATE")));
		assert!(!e.live_state(synth, Some("OTHER")));
		e.reloaded();
		assert!(!e.live_state(synth, Some("STATE")));
	}

	#[test]
	fn state_changes_and_closes_ask_for_captures_and_equal_states_are_skipped() {
		let (mut p, track, synth, fx) = project();
		let mut e = PluginEditors::default();
		e.opened(synth);
		e.opened(fx);
		let t0 = Instant::now();
		let a = e.handle(
			&p,
			vec![
				up(synth, EditorEvent::StateChanged),
				up(fx, EditorEvent::GestureBegin { param: "1".into() }),
				up(fx, EditorEvent::Closed),
			],
			t0,
		);
		assert_eq!(a.closed, vec![fx]);
		assert_eq!(a.capture, vec![fx]);
		assert!(!e.is_open(fx) && e.is_open(synth));
		assert!(e.flush(&p, t0 + STATE_QUIET / 2, false).capture.is_empty());
		assert_eq!(e.flush(&p, t0 + STATE_QUIET, false).capture, vec![synth]);
		p.tracks[0].instrument.as_mut().unwrap().state = Some("SAME".into());
		let same = EditorState {
			track: Some(track),
			device: synth,
			state: "SAME".into(),
		};
		assert!(e.store_states(&p, vec![same]).is_empty());
		assert_eq!(e.needs_capture(), vec![synth]);
	}

	#[test]
	fn removed_devices_are_forgotten() {
		let (mut p, _, synth, fx) = project();
		let mut e = PluginEditors::default();
		e.queue(fx, "Comp — Lead".into());
		e.opened(synth);
		e.handle(&p, vec![up(fx, value("1", 0.3))], Instant::now());
		p.tracks[0].channel.inserts.clear();
		e.prune(&p);
		assert!(!e.is_open(fx) && e.is_open(synth));
		assert!(e.take_queued().is_empty());
		assert!(!e.busy());
	}
}
