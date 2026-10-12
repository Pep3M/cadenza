//! Application state and logic, independent of any window so it can be unit-tested.
//!
//! [`AppState`] owns the editing [`Session`] (the same type the MCP server and the AI
//! agent work on: document + undo history, file path, plugin catalog, hosting). Every
//! edit, whatever its origin, goes through [`AppState::apply`] (or
//! [`AppState::edit_session`] for the shared MCP tool functions such as audio/MIDI
//! import): the command is validated and recorded by the `Document`, the project is
//! marked dirty, the selection is pruned and a debounced player reload is scheduled.
//! Views only read state and call these methods.

pub mod automation;
pub mod browser;
pub mod dnd;
pub mod edits;
pub mod files;
pub mod layout;
pub mod live;
pub mod meters;
pub mod notices;
pub mod piano;
pub mod rows;
pub mod selection;
pub mod timeline;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cadenza_core::{ClipId, DeviceId, ParamAddress, ParamTarget, Tick, TrackId};
use cadenza_dsp::DeviceRole;
use cadenza_engine::{MeterBank, MixReport, PlayerClock, RenderOptions};
use cadenza_mcp::{McpError, Session};
use cadenza_model::{ChannelPatch, Command, Document, Project, TrackKind};
use cadenza_plugin_host::{PluginCatalog, ScanReport};

use crate::audio::{AudioEngine, AudioEvent, Waker};
use crate::jobs::{FileAction, Job, PeakCache};
use browser::BrowserItem;
use dnd::{DropAction, DropTarget, Payload};
use files::FileBrowser;
use layout::{FollowMode, PanelKind, PanelLayout, Prefs};
use meters::Meters;
use notices::Notices;
use piano::PianoView;
use selection::{DeviceRef, Selection};
use timeline::Timeline;

pub use cadenza_mcp::tools::audio::AUDIO_EXTENSIONS;

/// Address of a channel's volume (`None` = master).
pub fn volume_address(track: Option<TrackId>) -> ParamAddress {
	ParamAddress {
		track,
		target: ParamTarget::Volume,
	}
}

pub fn pan_address(track: Option<TrackId>) -> ParamAddress {
	ParamAddress {
		track,
		target: ParamTarget::Pan,
	}
}

/// Edits within this window are compiled into the player once.
pub const RELOAD_DEBOUNCE: Duration = Duration::from_millis(120);

pub const DEFAULT_TRACK_HEIGHT: f32 = 64.0;
pub const MIN_TRACK_HEIGHT: f32 = 36.0;
pub const MAX_TRACK_HEIGHT: f32 = 220.0;

/// Panels and arrange-view settings (not part of the project).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
	pub timeline: Timeline,
	pub track_height: f32,
	/// Vertical scroll of the track list, in pixels.
	pub scroll_y: f32,
	pub panels: PanelLayout,
	/// Snap clips and notes to the grid (shared by the arrange view and the piano roll).
	pub snap: bool,
	pub follow: FollowMode,
	pub piano: PianoView,
	/// Bookmarked folders of the browser's Files section.
	pub bookmarks: Vec<PathBuf>,
	/// The F1 keyboard-shortcut overlay is open.
	pub help: bool,
	/// Automation and tempo lanes of the arrange view.
	pub automation: automation::AutomationView,
}

impl Default for ViewState {
	fn default() -> Self {
		Self {
			timeline: Timeline::default(),
			track_height: DEFAULT_TRACK_HEIGHT,
			scroll_y: 0.0,
			panels: PanelLayout::default(),
			snap: true,
			follow: FollowMode::Page,
			piano: PianoView::default(),
			bookmarks: Vec::new(),
			help: false,
			automation: automation::AutomationView::default(),
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioStatus {
	/// No audio engine (tests, headless).
	Off,
	Starting,
	Ready {
		sample_rate: u32,
	},
	Unavailable(String),
}

/// Something waiting for the "unsaved changes" decision.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
	New,
	Open(Option<PathBuf>),
	Quit,
}

/// Background work in flight.
#[derive(Default)]
pub struct Jobs {
	pub scan: Option<Job<Result<(PluginCatalog, ScanReport), String>>>,
	pub export: Option<Job<Result<(PathBuf, MixReport), String>>>,
	pub dialog: Option<(FileAction, Job<Option<PathBuf>>)>,
}

/// Values being dragged (faders, knobs, send levels): heard live and drawn, committed
/// as one command on release.
#[derive(Debug, Default, Clone)]
pub struct Previews {
	pub volume: HashMap<Option<TrackId>, f32>,
	pub pan: HashMap<Option<TrackId>, f32>,
	pub sends: HashMap<DeviceId, f32>,
	pub params: HashMap<(DeviceId, String), f32>,
}

pub struct AppState {
	pub session: Session,
	pub dirty: bool,
	pub selection: Selection,
	pub view: ViewState,
	pub notices: Notices,
	pub meters: Meters,
	pub previews: Previews,
	pub audio_status: AudioStatus,
	/// Warnings of the last player load (missing files, unknown devices...).
	pub load_warnings: Vec<String>,
	pub peaks: PeakCache,
	pub jobs: Jobs,
	pub files: FileBrowser,
	/// Notes sounding through [`AppState::preview_note_on`] (released on mouse up, on
	/// piano roll close and when the previewed track changes).
	pub previewing: Vec<(TrackId, u8)>,
	/// Awaiting the unsaved-changes prompt.
	pub confirm: Option<Pending>,
	/// Set when the app may close (nothing unsaved, or the user chose to discard/save).
	pub quit_confirmed: bool,
	/// What to continue with once a "Save As" dialog finished.
	after_save: Option<Pending>,
	browser_items: Option<Vec<BrowserItem>>,
	audio: Option<AudioEngine>,
	clock: Option<PlayerClock>,
	bank: Option<Arc<MeterBank>>,
	/// Playhead when there is no audio engine, or while stopped before it is ready.
	position: Tick,
	reload_since: Option<Instant>,
	generation: u64,
	loaded_generation: u64,
	waker: Option<Waker>,
	last_tick: Option<Instant>,
	/// Lane points last sent to the player by a running gesture.
	lane_preview: Option<(ParamAddress, Vec<cadenza_model::AutomationPoint>)>,
}

impl AppState {
	/// State with a live audio engine (the app).
	pub fn new(waker: Option<Waker>) -> Self {
		let mut s = Self::headless_with(waker.clone());
		s.audio = Some(AudioEngine::start(waker));
		s.audio_status = AudioStatus::Starting;
		s
	}

	/// State without audio (tests, screenshots in CI).
	pub fn headless() -> Self {
		Self::headless_with(None)
	}

	fn headless_with(waker: Option<Waker>) -> Self {
		let mut session = Session::new();
		session.reset(Document::new(Project::new("Untitled")), None);
		Self {
			session,
			dirty: false,
			selection: Selection::default(),
			view: ViewState::default(),
			notices: Notices::default(),
			meters: Meters::default(),
			previews: Previews::default(),
			audio_status: AudioStatus::Off,
			load_warnings: Vec::new(),
			peaks: PeakCache::new(waker.clone()),
			jobs: Jobs::default(),
			files: FileBrowser::default(),
			previewing: Vec::new(),
			confirm: None,
			quit_confirmed: false,
			after_save: None,
			browser_items: None,
			audio: None,
			clock: None,
			bank: None,
			position: Tick::ZERO,
			reload_since: Some(Instant::now()),
			generation: 0,
			loaded_generation: 0,
			waker,
			last_tick: None,
			lane_preview: None,
		}
	}

	pub fn project(&self) -> &Project {
		self.session.document().project()
	}

	pub fn document(&self) -> &Document {
		self.session.document()
	}

	pub fn path(&self) -> Option<&Path> {
		self.session.path()
	}

	/// Window title: project name, file and unsaved marker.
	pub fn title(&self) -> String {
		let file = self
			.path()
			.and_then(|p| p.file_name())
			.map(|f| f.to_string_lossy().into_owned())
			.unwrap_or_else(|| "not saved".into());
		format!(
			"{}{} ({file}) — Cadenza",
			if self.dirty { "• " } else { "" },
			self.project().name
		)
	}

	// --- Editing -------------------------------------------------------------------

	/// Executes one edit. Returns false (with an error notice) if the model rejected it.
	pub fn apply(&mut self, cmd: Command) -> bool {
		self.apply_inner(cmd)
	}

	fn apply_inner(&mut self, cmd: Command) -> bool {
		let before = self.project().clone();
		match self.session.document_mut().execute(cmd) {
			Ok(()) => {
				self.after_edit(Some(&before));
				true
			}
			Err(e) => {
				self.notices.error(e.to_string());
				false
			}
		}
	}

	/// Runs one of the shared MCP tool functions (import, presets...) on the session; any
	/// change it makes counts as an edit. Errors become notices.
	pub fn edit_session<T>(
		&mut self,
		f: impl FnOnce(&mut Session) -> Result<T, McpError>,
	) -> Option<T> {
		let steps = self.document().history_labels().len();
		let before = self.project().clone();
		let result = f(&mut self.session);
		if self.document().history_labels().len() != steps {
			self.after_edit(Some(&before));
		}
		match result {
			Ok(v) => Some(v),
			Err(e) => {
				self.notices.error(e.to_string());
				None
			}
		}
	}

	/// Bookkeeping after the project changed from `before`: values the player can take
	/// live (mix, parameters, automation) are sent to it, anything structural schedules a
	/// reload (which cuts sounding notes).
	fn after_edit(&mut self, before: Option<&Project>) {
		self.dirty = true;
		self.selection.prune(self.session.document().project());
		self.view.piano.prune(self.session.document().project());
		self.view
			.automation
			.prune(self.session.document().project());
		let project = self.session.document().project();
		self.meters
			.retain(|id| project.tracks.iter().any(|t| t.id == id));
		match before.and_then(|b| live::diff(b, self.project())) {
			Some(changes) => self.send_live(&changes),
			None => self.schedule_reload(),
		}
		// A load in flight may predate this edit.
		if self.loaded_generation < self.generation {
			self.schedule_reload();
		}
	}

	fn send_live(&self, changes: &live::LiveChanges) {
		let Some(a) = &self.audio else { return };
		for &(track, db) in &changes.volumes {
			a.live_volume(track, db);
		}
		for &(track, pan) in &changes.pans {
			a.live_pan(track, pan);
		}
		for &(send, db) in &changes.sends {
			a.live_send(send, db);
		}
		for (device, param, value) in &changes.params {
			a.live_param(*device, param, *value);
		}
		if changes.automation {
			let p = self.project();
			a.automation(p.automation.clone(), p.tempo.clone());
		}
	}

	/// Plays `points` for the lane `address` while a gesture runs (not in the project
	/// yet); repeated identical previews are not resent.
	pub fn preview_lane(
		&mut self,
		address: &ParamAddress,
		points: Vec<cadenza_model::AutomationPoint>,
	) {
		let key = (address.clone(), points);
		if self.lane_preview.as_ref() == Some(&key) {
			return;
		}
		let mut lanes = self.project().automation.clone();
		lanes.retain(|l| &l.address != address);
		if !key.1.is_empty() {
			lanes.push(cadenza_model::AutomationLane::new(
				address.clone(),
				key.1.clone(),
			));
		}
		if let Some(a) = &self.audio {
			a.automation(lanes, self.project().tempo.clone());
		}
		self.lane_preview = Some(key);
	}

	/// Ends a lane preview: the player goes back to the project's lanes (a committed
	/// edit has already sent them).
	pub fn end_lane_preview(&mut self) {
		if self.lane_preview.take().is_some()
			&& let Some(a) = &self.audio
		{
			let p = self.project();
			a.automation(p.automation.clone(), p.tempo.clone());
		}
	}

	/// The plugin catalog (loaded on first use).
	pub fn catalog(&mut self) -> Arc<PluginCatalog> {
		self.session.plugins()
	}

	/// What the lane `address` controls (range, unit, name).
	pub fn lane_meta(&mut self, address: &ParamAddress) -> Option<automation::ParamMeta> {
		let catalog = self.catalog();
		automation::meta(self.project(), Some(&catalog), address)
	}

	/// Every automatable parameter of a channel, for the lane picker.
	pub fn lane_choices(&mut self, owner: Option<TrackId>) -> Vec<automation::Choice> {
		let catalog = self.catalog();
		automation::choices(self.project(), Some(&catalog), owner)
	}

	/// Static (non-automated) value of `address`.
	pub fn static_value(&mut self, address: &ParamAddress) -> Option<f32> {
		let catalog = self.catalog();
		automation::static_value(self.project(), Some(&catalog), address)
	}

	/// "Automate this": shows the lane for `address` under its channel in the arrange
	/// view, creating it (one point at the start, at the current value) if needed.
	pub fn automate(&mut self, address: ParamAddress) {
		if !self.is_automated(&address) {
			let Some(value) = self.static_value(&address) else {
				return;
			};
			let cmd = Command::SetAutomation {
				address: address.clone(),
				points: vec![cadenza_model::AutomationPoint::new(Tick::ZERO, value)],
			};
			if !self.apply(cmd) {
				return;
			}
		}
		match address.track {
			Some(t) => self.selection.select_track(t),
			None => self.selection.select_master(),
		}
		self.view.automation.show(address);
		if !self.view.panels.visible(PanelKind::Arrange) {
			self.view.panels.set(PanelKind::Arrange, true);
		}
	}

	/// Toggles the automation lanes of the selected track (or the master): the A key.
	pub fn toggle_lanes(&mut self) {
		let owner = if self.selection.master {
			None
		} else if let Some(t) = self.selection.track {
			Some(t)
		} else {
			self.notices
				.info("Select a track to show its automation lanes");
			return;
		};
		let project = self.project().clone();
		self.view.automation.toggle(&project, owner);
	}

	/// Deletes the selected automation points (one undo step). Returns false when none
	/// are selected.
	pub fn delete_selected_points(&mut self) -> bool {
		let selected = std::mem::take(&mut self.view.automation.selected);
		if selected.is_empty() {
			return false;
		}
		let mut addresses: Vec<ParamAddress> = Vec::new();
		for (a, _) in &selected {
			if !addresses.contains(a) {
				addresses.push(a.clone());
			}
		}
		let mut commands: Vec<Command> = addresses
			.iter()
			.filter_map(|a| {
				let ticks: Vec<Tick> = selected
					.iter()
					.filter(|(x, _)| x == a)
					.map(|(_, t)| *t)
					.collect();
				automation::delete_points(self.project(), a, &ticks)
			})
			.collect();
		match commands.len() {
			0 => {}
			1 => {
				self.apply(commands.remove(0));
			}
			_ => {
				self.apply(Command::Batch {
					label: "Delete automation points".into(),
					commands,
				});
			}
		}
		true
	}

	/// The BPM field: sets the tempo of the segment the playhead is in.
	pub fn set_tempo_at_playhead(&mut self, bpm: f64) {
		let segment = automation::tempo_segment(self.project(), self.position());
		if (segment.bpm - bpm).abs() > 1e-9 {
			self.apply(Command::SetTempo {
				tick: segment.tick,
				bpm,
			});
		}
	}

	pub fn undo(&mut self) {
		let before = self.project().clone();
		match self.session.document_mut().undo() {
			Ok(label) => {
				self.notices.info(format!("Undo: {label}"));
				self.after_edit(Some(&before));
			}
			Err(e) => self.notices.info(e.to_string()),
		}
	}

	pub fn redo(&mut self) {
		let before = self.project().clone();
		match self.session.document_mut().redo() {
			Ok(label) => {
				self.notices.info(format!("Redo: {label}"));
				self.after_edit(Some(&before));
			}
			Err(e) => self.notices.info(e.to_string()),
		}
	}

	pub fn undo_label(&self) -> Option<&str> {
		self.document().history_labels().last().copied()
	}

	pub fn redo_label(&self) -> Option<&str> {
		self.document().redo_labels().first().copied()
	}

	/// Shows a dragged fader value and plays it live, without an undo step yet.
	pub fn preview_volume(&mut self, track: Option<TrackId>, db: f32) {
		self.previews.volume.insert(track, db);
		if let Some(a) = &self.audio {
			a.live_volume(track, db);
		}
	}

	pub fn preview_pan(&mut self, track: Option<TrackId>, pan: f32) {
		self.previews.pan.insert(track, pan);
		if let Some(a) = &self.audio {
			a.live_pan(track, pan);
		}
	}

	/// Ends a fader gesture with one undoable command. Like every mix/parameter edit it
	/// reaches the player live (see [`live`]): no recompilation cutting sounding notes.
	pub fn commit_volume(&mut self, track: Option<TrackId>, db: f32) {
		self.previews.volume.remove(&track);
		if self.project().channel(track).map(|c| c.volume_db) == Some(db) {
			return;
		}
		self.apply(Command::SetChannel {
			track,
			patch: ChannelPatch {
				volume_db: Some(db),
				..ChannelPatch::default()
			},
		});
	}

	pub fn commit_pan(&mut self, track: Option<TrackId>, pan: f32) {
		self.previews.pan.remove(&track);
		if self.project().channel(track).map(|c| c.pan) == Some(pan) {
			return;
		}
		self.apply(Command::SetChannel {
			track,
			patch: ChannelPatch {
				pan: Some(pan),
				..ChannelPatch::default()
			},
		});
	}

	/// Volume shown for a strip: the dragged value while a gesture is running, else the
	/// automated value at the playhead, else the channel's value.
	pub fn volume_db(&self, track: Option<TrackId>) -> f32 {
		self.previews
			.volume
			.get(&track)
			.copied()
			.or_else(|| self.automated(&volume_address(track)))
			.or_else(|| self.project().channel(track).map(|c| c.volume_db))
			.unwrap_or(0.0)
	}

	pub fn pan(&self, track: Option<TrackId>) -> f32 {
		self.previews
			.pan
			.get(&track)
			.copied()
			.or_else(|| self.automated(&pan_address(track)))
			.or_else(|| self.project().channel(track).map(|c| c.pan))
			.unwrap_or(0.0)
	}

	/// Value of the automation lane for `address` at the playhead, if it has one.
	pub fn automated(&self, address: &ParamAddress) -> Option<f32> {
		let p = self.project();
		p.automation_lane(address)?
			.value_at(self.position(), &p.tempo)
	}

	/// Whether `address` has an automation lane (the control follows it).
	pub fn is_automated(&self, address: &ParamAddress) -> bool {
		self.project()
			.automation_lane(address)
			.is_some_and(|l| !l.points.is_empty())
	}

	/// Shown level of a send (preview, automation, model).
	pub fn send_level(&self, track: TrackId, send: DeviceId) -> f32 {
		self.previews
			.sends
			.get(&send)
			.copied()
			.or_else(|| self.automated(&ParamAddress::send(track, send)))
			.or_else(|| {
				self.project()
					.track(track)?
					.channel
					.sends
					.iter()
					.find(|s| s.id == send)
					.map(|s| s.level_db)
			})
			.unwrap_or(0.0)
	}

	/// Shown value of a device parameter (preview, automation, explicit value), or
	/// `None` when it uses the device default.
	pub fn param_value(
		&self,
		track: Option<TrackId>,
		device: DeviceId,
		param: &str,
	) -> Option<f32> {
		if let Some(v) = self.previews.params.get(&(device, param.to_owned())) {
			return Some(*v);
		}
		self.automated(&ParamAddress::device(track, device, param))
			.or_else(|| {
				self.project()
					.device(track, device)?
					.params
					.get(param)
					.copied()
			})
	}

	/// A send level being dragged: heard live, committed with [`AppState::commit_send`].
	pub fn preview_send(&mut self, send: DeviceId, db: f32) {
		self.previews.sends.insert(send, db);
		if let Some(a) = &self.audio {
			a.live_send(send, db);
		}
	}

	pub fn commit_send(&mut self, track: TrackId, send: DeviceId, db: f32) {
		self.previews.sends.remove(&send);
		self.set_param(ParamAddress::send(track, send), db);
	}

	/// A device parameter being dragged: heard live (built-ins and plugins, sandboxed
	/// too), committed with [`AppState::commit_param`].
	pub fn preview_param(&mut self, device: DeviceId, param: &str, value: f32) {
		self.previews
			.params
			.insert((device, param.to_owned()), value);
		if let Some(a) = &self.audio {
			a.live_param(device, param, value);
		}
	}

	pub fn commit_param(
		&mut self,
		track: Option<TrackId>,
		device: DeviceId,
		param: &str,
		value: f32,
	) {
		self.previews.params.remove(&(device, param.to_owned()));
		self.set_param(ParamAddress::device(track, device, param), value);
	}

	/// One `SetParam` unless the value is already set.
	fn set_param(&mut self, address: ParamAddress, value: f32) {
		let p = self.project();
		let current = match &address.target {
			ParamTarget::Volume => p.channel(address.track).map(|c| c.volume_db),
			ParamTarget::Pan => p.channel(address.track).map(|c| c.pan),
			ParamTarget::Send { send } => address
				.track
				.and_then(|t| p.track(t))
				.and_then(|t| t.channel.sends.iter().find(|s| s.id == *send))
				.map(|s| s.level_db),
			ParamTarget::Device { device, param } => p
				.device(address.track, *device)
				.and_then(|d| d.params.get(param).copied()),
		};
		if current != Some(value) {
			self.apply(Command::SetParam { address, value });
		}
	}

	// --- Devices -------------------------------------------------------------------

	/// Every device the browser offers (built-ins and the cached plugin catalog).
	pub fn browser_items(&mut self) -> &[BrowserItem] {
		if self.browser_items.is_none() {
			let catalog = self.session.plugins();
			self.browser_items = Some(browser::items(Some(&catalog)));
		}
		self.browser_items.as_deref().unwrap_or_default()
	}

	/// Puts a browser device on `track` (`None`: the selected track, or the master for
	/// effects): an instrument replaces the instrument of an instrument track (or creates
	/// a new instrument track), an effect is appended to the chain.
	pub fn use_device(&mut self, item: &BrowserItem, track: Option<TrackId>) -> bool {
		let Some(device) = self.build_device(item) else {
			return false;
		};
		let target = track.or(self.selection.track);
		let project = self.project();
		let device_ref;
		let cmd = match item.role {
			DeviceRole::Instrument => match target.and_then(|t| project.track(t)) {
				Some(t) if t.kind == TrackKind::Instrument => {
					device_ref = DeviceRef {
						track: Some(t.id),
						device: device.id,
					};
					Command::SetInstrument {
						track: t.id,
						device: Some(device),
					}
				}
				_ => {
					let mut t = edits::new_track(project, TrackKind::Instrument);
					t.name = item.name.clone();
					device_ref = DeviceRef {
						track: Some(t.id),
						device: device.id,
					};
					t.instrument = Some(device);
					edits::add_track(project, t, target)
				}
			},
			DeviceRole::Effect => {
				let track = if self.selection.master && track.is_none() {
					None
				} else {
					target
				};
				device_ref = DeviceRef {
					track,
					device: device.id,
				};
				Command::InsertDevice {
					track,
					device,
					index: None,
				}
			}
		};
		let ok = self.apply(cmd);
		if ok {
			self.selection.select_device(device_ref);
		}
		ok
	}

	/// Loads a built-in factory preset into a device (one undo step).
	pub fn load_factory_preset(&mut self, device: DeviceRef, preset: &cadenza_dsp::FactoryPreset) {
		let params = preset
			.params
			.iter()
			.map(|(k, v)| ((*k).to_owned(), *v))
			.collect();
		self.apply(Command::SetDeviceState {
			track: device.track,
			device: device.device,
			state: None,
			params: Some(params),
		});
	}

	/// The device at `r`, if it still exists (instrument or insert).
	pub fn device(&self, r: DeviceRef) -> Option<&cadenza_model::Device> {
		match r.track {
			Some(t) => self.project().track(t)?.device(r.device),
			None => self
				.project()
				.master
				.inserts
				.iter()
				.find(|d| d.id == r.device),
		}
	}

	pub fn remove_device(&mut self, track: Option<TrackId>, device: DeviceId) {
		let is_instrument = track
			.and_then(|t| self.project().track(t))
			.and_then(|t| t.instrument.as_ref())
			.is_some_and(|d| d.id == device);
		let cmd = match (is_instrument, track) {
			(true, Some(track)) => Command::SetInstrument {
				track,
				device: None,
			},
			_ => Command::RemoveDevice { track, device },
		};
		self.apply(cmd);
	}

	// --- Transport -----------------------------------------------------------------

	pub fn is_playing(&self) -> bool {
		self.clock.as_ref().is_some_and(PlayerClock::is_playing)
	}

	pub fn position(&self) -> Tick {
		self.clock
			.as_ref()
			.map_or(self.position, PlayerClock::position)
	}

	pub fn play(&mut self) {
		// A pending edit must reach the player before it starts.
		if self.reload_since.is_some() {
			self.send_reload();
		}
		match &self.audio {
			Some(a) if self.clock.is_some() => a.play(),
			_ => self
				.notices
				.warn("No audio output: playback is unavailable"),
		}
	}

	pub fn stop(&mut self) {
		if let Some(a) = &self.audio {
			a.stop();
		}
	}

	/// Song end (last clip end), at least one bar.
	pub fn song_end(&self) -> Tick {
		let bar = self.project().time_signature.ticks_per_bar();
		Tick(self.project().end_tick().0.max(bar.0))
	}

	pub fn toggle_play(&mut self) {
		if self.is_playing() {
			self.stop();
		} else {
			self.play();
		}
	}

	pub fn seek(&mut self, tick: Tick) {
		let tick = Tick(tick.0.max(0));
		self.position = tick;
		if let Some(a) = &self.audio {
			a.seek(tick);
		}
	}

	// --- Files ---------------------------------------------------------------------

	/// Runs `pending` now, or asks first if there are unsaved changes.
	pub fn request(&mut self, pending: Pending) {
		if self.dirty {
			self.confirm = Some(pending);
		} else {
			self.proceed(pending);
		}
	}

	/// Continues after the unsaved-changes prompt (or when there was nothing to save).
	pub fn proceed(&mut self, pending: Pending) {
		self.confirm = None;
		match pending {
			Pending::New => self.new_project(),
			Pending::Open(Some(path)) => {
				self.open(&path);
			}
			Pending::Open(None) => self.file_dialog(FileAction::Open),
			Pending::Quit => self.quit_confirmed = true,
		}
	}

	/// "Save" in the unsaved-changes prompt: saves, then continues.
	pub fn save_then(&mut self, pending: Pending) {
		self.confirm = None;
		if self.path().is_some() {
			if self.save() {
				self.proceed(pending);
			}
		} else {
			self.after_save = Some(pending);
			self.file_dialog(FileAction::SaveAs);
		}
	}

	pub fn new_project(&mut self) {
		self.stop();
		self.session
			.reset(Document::new(Project::new("Untitled")), None);
		self.reset_view();
		self.notices.info("New project");
	}

	pub fn open(&mut self, path: &Path) -> bool {
		match Project::load_json(path) {
			Ok(project) => {
				self.stop();
				self.session
					.reset(Document::new(project), Some(path.to_path_buf()));
				self.reset_view();
				self.notices.info(format!("Opened {}", path.display()));
				true
			}
			Err(e) => {
				self.notices
					.error(format!("Cannot open {}: {e}", path.display()));
				false
			}
		}
	}

	fn reset_view(&mut self) {
		self.dirty = false;
		self.selection = Selection::default();
		self.previews = Previews::default();
		self.view.timeline.scroll_beats = 0.0;
		self.view.scroll_y = 0.0;
		self.release_previews();
		self.view.piano.clip = None;
		self.view.piano.selected.clear();
		self.view.piano.framed = None;
		self.view.automation = automation::AutomationView::default();
		self.load_warnings.clear();
		self.seek(Tick::ZERO);
		self.schedule_reload();
	}

	/// Saves to the current path (asks for one if the project was never saved). Plugin
	/// states are captured first, like the MCP `save_project` tool.
	pub fn save(&mut self) -> bool {
		if self.path().is_none() {
			self.file_dialog(FileAction::SaveAs);
			return false;
		}
		self.save_to(None)
	}

	pub fn save_as(&mut self, path: PathBuf) -> bool {
		let path = if path.extension().is_none() {
			path.with_extension("cadenza.json")
		} else {
			path
		};
		self.save_to(Some(path))
	}

	fn save_to(&mut self, path: Option<PathBuf>) -> bool {
		let args = cadenza_mcp::tools::project::SaveProjectArgs {
			path: path.map(|p| p.display().to_string()),
			capture_plugin_states: None,
		};
		let saved = self.edit_session(|s| cadenza_mcp::tools::project::save_project(s, args));
		match saved {
			Some(out) => {
				self.dirty = false;
				for w in out.warnings {
					self.notices.warn(w);
				}
				self.notices.info(format!("Saved {}", out.path));
				true
			}
			None => false,
		}
	}

	/// Imports an audio file at the playhead: onto the selected audio track, or a new one.
	pub fn import_audio(&mut self, path: &Path) {
		let track = self
			.selection
			.track
			.filter(|t| self.project().track(*t).map(|t| t.kind) == Some(TrackKind::Audio));
		self.import_audio_at(path, track, None);
	}

	/// Imports an audio file onto `track` (an audio track; `None` = a new one) at `at`
	/// (`None` = the playhead).
	pub fn import_audio_at(&mut self, path: &Path, track: Option<TrackId>, at: Option<Tick>) {
		let args = cadenza_mcp::tools::audio::ImportAudioArgs {
			path: path.display().to_string(),
			track_id: track,
			at_beats: Some(at.unwrap_or_else(|| self.position()).beats()),
			name: None,
			gain_db: None,
		};
		if let Some(out) = self.edit_session(|s| cadenza_mcp::tools::audio::import_audio(s, args)) {
			self.selection.click_clip(out.track_id, out.clip_id, false);
			self.notices.info(format!(
				"Imported {} ({:.1} s)",
				path.display(),
				out.file.duration_seconds
			));
		}
	}

	/// Imports a standard MIDI file as new instrument tracks.
	pub fn import_midi(&mut self, path: &Path) {
		self.import_midi_at(path, None, Some(Tick::ZERO));
	}

	/// Imports a MIDI file into instrument track `track` as one clip, or (`None`) as new
	/// instrument tracks, starting at `at` (`None` = the playhead). One undo step.
	pub fn import_midi_at(&mut self, path: &Path, track: Option<TrackId>, at: Option<Tick>) {
		let at = at.unwrap_or_else(|| self.position());
		if track.is_some() || at == Tick::ZERO {
			let args = cadenza_mcp::tools::midi::ImportMidiArgs {
				path: path.display().to_string(),
				split_by_channel: false,
				track_id: track,
				at_beats: Some(at.beats()),
			};
			if let Some(out) = self.edit_session(|s| cadenza_mcp::tools::midi::import_midi(s, args))
			{
				for w in out.warnings {
					self.notices.warn(w);
				}
				if let Some(c) = out.clip {
					self.selection.click_clip(c.track_id, c.clip_id, false);
				}
				self.notices.info(format!(
					"Imported {} ({} tracks)",
					path.display(),
					out.created_tracks.len().max(1)
				));
			}
			return;
		}
		// New tracks at a position: the shared tool always starts them at 0, so build the
		// import batch here and shift its clips (still one command, one undo step).
		let result = std::fs::read(path)
			.map_err(|e| e.to_string())
			.and_then(|bytes| {
				cadenza_midi_io::import_smf(
					&bytes,
					&cadenza_midi_io::ImportOptions {
						split_by_channel: false,
						target: cadenza_midi_io::ImportTarget::NewTracks,
					},
				)
				.map_err(|e| e.to_string())
			});
		let mut result = match result {
			Ok(r) => r,
			Err(e) => {
				self.notices
					.error(format!("Cannot import {}: {e}", path.display()));
				return;
			}
		};
		let mut tracks = 0;
		if let Command::Batch { commands, .. } = &mut result.command {
			for cmd in commands {
				if let Command::AddTrack { track, .. } = cmd {
					tracks += 1;
					for c in &mut track.clips {
						c.start = c.start + at;
					}
				}
			}
		}
		if self.apply(result.command) {
			for w in result.warnings {
				self.notices.warn(w);
			}
			self.notices
				.info(format!("Imported {} ({tracks} tracks)", path.display()));
		}
	}

	// --- Drag and drop -------------------------------------------------------------

	/// What dropping `payload` on `target` would do (for the drop hint).
	pub fn drop_action(&self, payload: &Payload, target: DropTarget, copy: bool) -> DropAction {
		dnd::resolve(self.project(), payload, target, copy)
	}

	/// Performs a drop. Returns false when nothing happened (rejected or failed).
	pub fn drop_payload(&mut self, payload: &Payload, target: DropTarget, copy: bool) -> bool {
		match self.drop_action(payload, target, copy) {
			DropAction::SetInstrument { track, item } => self.use_device(&item, Some(track)),
			DropAction::InsertEffect { track, item, index } => {
				self.insert_effect(&item, track, index)
			}
			DropAction::NewTrack { item, after } => self.new_track_with(&item, after),
			DropAction::LoadPreset { device, preset } => {
				self.load_factory_preset(device, preset);
				self.selection.select_device(device);
				true
			}
			DropAction::DeviceWithPreset {
				track,
				device,
				preset,
			} => self.add_device_with_preset(track, &device, preset),
			DropAction::MoveInsert {
				from,
				device,
				to,
				slot,
				copy,
			} => match edits::drop_insert(self.project(), from, device, to, slot, copy) {
				Some(cmd) => self.apply(cmd),
				None => false,
			},
			DropAction::ImportAudio { path, track, at } => {
				self.import_audio_at(&path, track, at);
				true
			}
			DropAction::ImportMidi { path, track, at } => {
				self.import_midi_at(&path, track, at);
				true
			}
			DropAction::OpenProject(path) => {
				self.request(Pending::Open(Some(path)));
				true
			}
			DropAction::Reject(why) => {
				self.notices.info(why);
				false
			}
		}
	}

	fn build_device(&mut self, item: &BrowserItem) -> Option<cadenza_model::Device> {
		let catalog = (item.source == browser::Source::Plugin).then(|| self.session.plugins());
		let spec = cadenza_mcp::convert::DeviceSpec {
			device: item.id.clone(),
			params: Default::default(),
		};
		match cadenza_mcp::convert::build_device(&spec, item.role, catalog.as_deref()) {
			Ok(d) => Some(d),
			Err(e) => {
				self.notices.error(e.to_string());
				None
			}
		}
	}

	/// Inserts an effect at `index` (None = end) of a channel (`None` = master).
	pub fn insert_effect(
		&mut self,
		item: &BrowserItem,
		track: Option<TrackId>,
		index: Option<usize>,
	) -> bool {
		let Some(device) = self.build_device(item) else {
			return false;
		};
		let r = DeviceRef {
			track,
			device: device.id,
		};
		let ok = self.apply(Command::InsertDevice {
			track,
			device,
			index,
		});
		if ok {
			self.selection.select_device(r);
		}
		ok
	}

	/// A new track below `after` (or at the end) holding `item`: an instrument track for
	/// an instrument, a bus for an effect.
	pub fn new_track_with(&mut self, item: &BrowserItem, after: Option<TrackId>) -> bool {
		let Some(device) = self.build_device(item) else {
			return false;
		};
		let project = self.project();
		let r;
		let track = match item.role {
			DeviceRole::Instrument => {
				let mut t = edits::new_track(project, TrackKind::Instrument);
				t.name = item.name.clone();
				r = DeviceRef {
					track: Some(t.id),
					device: device.id,
				};
				t.instrument = Some(device);
				t
			}
			DeviceRole::Effect => {
				let mut t = edits::new_track(project, TrackKind::Bus);
				t.name = format!("{} Bus", item.name);
				r = DeviceRef {
					track: Some(t.id),
					device: device.id,
				};
				t.channel.inserts.push(device);
				t
			}
		};
		let after = after.or_else(|| project.tracks.last().map(|t| t.id));
		let cmd = edits::add_track(project, track, after);
		let ok = self.apply(cmd);
		if ok {
			self.selection.select_device(r);
		}
		ok
	}

	/// Adds built-in `device` with `preset` loaded to `track` (instrument or insert).
	fn add_device_with_preset(
		&mut self,
		track: TrackId,
		device: &str,
		preset: &cadenza_dsp::FactoryPreset,
	) -> bool {
		let Some(info) = cadenza_dsp::registry().iter().find(|i| i.id == device) else {
			return false;
		};
		let mut d = cadenza_model::Device::builtin(info.id, info.name);
		d.params = preset
			.params
			.iter()
			.map(|(k, v)| ((*k).to_owned(), *v))
			.collect();
		let r = DeviceRef {
			track: Some(track),
			device: d.id,
		};
		let cmd = match info.role {
			DeviceRole::Instrument => Command::SetInstrument {
				track,
				device: Some(d),
			},
			DeviceRole::Effect => Command::InsertDevice {
				track: Some(track),
				device: d,
				index: None,
			},
		};
		let ok = self.apply(cmd);
		if ok {
			self.selection.select_device(r);
		}
		ok
	}

	// --- Piano roll and note preview ----------------------------------------------

	/// Opens `clip` (a MIDI clip) in the piano roll and shows the panel.
	pub fn open_piano_roll(&mut self, clip: ClipId) -> bool {
		let Some((track, c)) = self.project().find_clip(clip) else {
			return false;
		};
		if !c.is_midi() {
			return false;
		}
		let track = track.id;
		if self.view.piano.clip != Some(clip) {
			self.release_previews();
			self.view.piano.selected.clear();
			self.view.piano.framed = None;
		}
		self.view.piano.clip = Some(clip);
		self.selection.click_clip(track, clip, false);
		self.view.panels.open_piano_roll();
		true
	}

	pub fn close_piano_roll(&mut self) {
		self.release_previews();
		self.view.panels.set(PanelKind::PianoRoll, false);
	}

	/// Track of the clip in the piano roll.
	pub fn piano_track(&self) -> Option<TrackId> {
		let clip = self.view.piano.clip?;
		self.project().find_clip(clip).map(|(t, _)| t.id)
	}

	/// Starts sounding `pitch` on `track`'s instrument (also while stopped). Notes held
	/// on another track are released first.
	pub fn preview_note_on(&mut self, track: TrackId, pitch: u8, velocity: f32) {
		if self.previewing.iter().any(|(t, _)| *t != track) {
			self.release_previews();
		}
		if self.previewing.contains(&(track, pitch)) {
			return;
		}
		self.previewing.push((track, pitch));
		if let Some(a) = &self.audio {
			a.preview_on(track, pitch, velocity);
		}
	}

	pub fn preview_note_off(&mut self, track: TrackId, pitch: u8) {
		let before = self.previewing.len();
		self.previewing.retain(|n| *n != (track, pitch));
		if self.previewing.len() != before
			&& let Some(a) = &self.audio
		{
			a.preview_off(track, pitch);
		}
	}

	/// Releases every previewed note.
	pub fn release_previews(&mut self) {
		for (track, pitch) in std::mem::take(&mut self.previewing) {
			if let Some(a) = &self.audio {
				a.preview_off(track, pitch);
			}
		}
	}

	// --- Preferences ---------------------------------------------------------------

	/// The per-user layout/zoom preferences to persist.
	pub fn prefs(&self) -> Prefs {
		let v = &self.view;
		let mut panels = v.panels.clone();
		// The mixer only made room for the piano roll (which starts closed next time).
		panels.mixer |= panels.mixer_yielded;
		Prefs {
			panels,
			px_per_beat: v.timeline.px_per_beat,
			track_height: v.track_height,
			snap: v.snap,
			follow: v.follow,
			piano_px_per_beat: v.piano.timeline.px_per_beat,
			piano_row_height: v.piano.row_height,
			piano_velocity_lane: v.piano.velocity_lane,
			piano_ghosts: v.piano.ghosts,
			piano_scale: v
				.piano
				.scale
				.map(|s| format!("{} {}", s.root.name(false), scale_kind_name(s.kind)))
				.unwrap_or_default(),
			bookmarks: v.bookmarks.clone(),
		}
	}

	/// Restores saved preferences (values are clamped to valid ranges).
	pub fn apply_prefs(&mut self, p: Prefs) {
		let v = &mut self.view;
		v.panels = PanelLayout {
			maximized: None,
			focus: None,
			mixer_yielded: false,
			..p.panels
		};
		// The piano roll has nothing to show until a clip is opened.
		v.panels.piano_roll = false;
		v.timeline.px_per_beat = p
			.px_per_beat
			.clamp(timeline::MIN_PX_PER_BEAT, timeline::MAX_PX_PER_BEAT);
		v.track_height = p.track_height.clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
		v.snap = p.snap;
		v.follow = p.follow;
		v.piano.timeline.px_per_beat = p
			.piano_px_per_beat
			.clamp(timeline::MIN_PX_PER_BEAT, timeline::MAX_PX_PER_BEAT);
		v.piano.row_height = p
			.piano_row_height
			.clamp(piano::MIN_ROW_HEIGHT, piano::MAX_ROW_HEIGHT);
		v.piano.velocity_lane = p.piano_velocity_lane;
		v.piano.ghosts = p.piano_ghosts;
		v.piano.scale = cadenza_theory::Scale::parse(&p.piano_scale).ok();
		v.bookmarks = p.bookmarks;
		if self.files.dir.is_none() {
			self.files.dir = v.bookmarks.first().cloned();
		}
	}

	/// Renders the song to `path` (format from the extension) on a worker thread.
	pub fn start_export(&mut self, path: PathBuf) {
		if self.jobs.export.is_some() {
			self.notices.warn("An export is already running");
			return;
		}
		let export = match cadenza_engine::export_options_for_path(&path, None, None) {
			Ok(e) => e,
			Err(e) => {
				self.notices.error(e.to_string());
				return;
			}
		};
		let project = self.project().clone();
		let opts = RenderOptions {
			plugins: self.session.plugins_for_project(),
			hosting: self.session.plugin_hosting().clone(),
			..RenderOptions::default()
		};
		self.notices.info(format!("Exporting {}…", path.display()));
		self.jobs.export = Some(Job::spawn("export", self.waker.clone(), move || {
			cadenza_engine::render_to_file(&project, &opts, &path, &export)
				.map(|r| (path, r))
				.map_err(|e| e.to_string())
		}));
	}

	/// Rescans plugins in the background (the scanner loads each new plugin in a helper
	/// process).
	pub fn rescan_plugins(&mut self) {
		if self.jobs.scan.is_some() {
			return;
		}
		let cache = self.session.plugin_cache_path().to_path_buf();
		self.notices.info("Scanning plugins…");
		self.jobs.scan = Some(Job::spawn("scan", self.waker.clone(), move || {
			let scanner = cadenza_plugin_host::scanner_path().map_err(|e| e.to_string())?;
			let mut catalog = PluginCatalog::load_or_default(cache);
			let report = catalog
				.scan(&cadenza_plugin_host::default_search_paths(), &scanner)
				.map_err(|e| e.to_string())?;
			catalog.save().map_err(|e| e.to_string())?;
			Ok((catalog, report))
		}));
	}

	/// Opens a native file dialog for `action` (result handled in [`AppState::tick`]).
	pub fn file_dialog(&mut self, action: FileAction) {
		if self.jobs.dialog.is_some() {
			return;
		}
		let dir = self.path().and_then(Path::parent).map(Path::to_path_buf);
		let stem = self
			.path()
			.and_then(|p| p.file_name())
			.and_then(|f| f.to_str())
			.and_then(|f| f.split('.').next())
			.map(str::to_owned)
			.unwrap_or_else(|| self.project().name.clone());
		let name = match action {
			FileAction::Export => format!("{stem}.wav"),
			_ => format!("{stem}.cadenza.json"),
		};
		self.jobs.dialog = Some((
			action,
			crate::jobs::file_dialog(action, dir, name, self.waker.clone()),
		));
	}

	fn finish_dialog(&mut self, action: FileAction, path: Option<PathBuf>) {
		let Some(path) = path else {
			self.after_save = None;
			return;
		};
		match action {
			FileAction::Open => {
				self.open(&path);
			}
			FileAction::SaveAs => {
				if self.save_as(path)
					&& let Some(next) = self.after_save.take()
				{
					self.proceed(next);
				}
			}
			FileAction::ImportAudio => self.import_audio(&path),
			FileAction::ImportMidi => self.import_midi(&path),
			FileAction::Export => self.start_export(path),
		}
	}

	// --- Frame update --------------------------------------------------------------

	fn schedule_reload(&mut self) {
		self.generation += 1;
		self.reload_since.get_or_insert_with(Instant::now);
	}

	/// Whether an edit has not reached the player yet.
	pub fn reload_pending(&self) -> bool {
		self.reload_since.is_some()
	}

	fn send_reload(&mut self) {
		self.reload_since = None;
		let Some(audio) = &self.audio else {
			self.loaded_generation = self.generation;
			return;
		};
		if self.clock.is_none() {
			// Not ready yet: retried once the device is open.
			self.reload_since = Some(Instant::now());
			return;
		}
		let plugins = self.session.plugins_for_project();
		audio.load(
			self.session.document().project().clone(),
			plugins,
			self.session.plugin_hosting().clone(),
			self.generation,
		);
	}

	/// Per-frame housekeeping: player events, debounced reloads, background jobs and
	/// meter ballistics. Returns true while something animates (repaint again soon).
	pub fn tick(&mut self, now: Instant) -> bool {
		let dt = self
			.last_tick
			.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32())
			.min(0.25);
		self.last_tick = Some(now);
		self.poll_audio();
		if let Some(since) = self.reload_since
			&& now.saturating_duration_since(since) >= RELOAD_DEBOUNCE
		{
			self.send_reload();
		}
		self.poll_jobs();
		self.peaks.poll();
		let ids: Vec<TrackId> = self.project().tracks.iter().map(|t| t.id).collect();
		self.meters.update(self.bank.as_deref(), ids, dt);
		self.is_playing()
			|| self.meters.any_active()
			|| self.reload_since.is_some()
			|| self.jobs.scan.is_some()
			|| self.jobs.export.is_some()
	}

	fn poll_audio(&mut self) {
		let Some(audio) = &self.audio else { return };
		let mut events = Vec::new();
		while let Some(e) = audio.poll() {
			events.push(e);
		}
		for event in events {
			match event {
				AudioEvent::Ready { clock } => {
					self.audio_status = AudioStatus::Ready {
						sample_rate: clock.sample_rate(),
					};
					self.clock = Some(clock);
					if let Some(a) = &self.audio {
						a.seek(self.position);
					}
					self.schedule_reload();
				}
				AudioEvent::Unavailable(e) => {
					self.notices.warn(format!("No audio output: {e}"));
					self.audio_status = AudioStatus::Unavailable(e);
				}
				AudioEvent::Loaded {
					generation,
					warnings,
					meters,
				} => {
					self.loaded_generation = self.loaded_generation.max(generation);
					self.bank = meters;
					if warnings != self.load_warnings {
						for w in &warnings {
							self.notices.warn(w.clone());
						}
						self.load_warnings = warnings;
					}
				}
				AudioEvent::LoadFailed { generation, error } => {
					self.loaded_generation = self.loaded_generation.max(generation);
					self.notices.error(format!("Playback engine: {error}"));
				}
				AudioEvent::ReloadNeeded => self.schedule_reload(),
				AudioEvent::PluginCrashes(crashes) => {
					for c in crashes {
						self.notices.error(c.message);
					}
				}
				AudioEvent::Error(e) => self.notices.error(format!("Audio: {e}")),
			}
		}
	}

	fn poll_jobs(&mut self) {
		if let Some(r) = self.jobs.dialog.as_ref().and_then(|(_, j)| j.poll()) {
			let (action, _) = self.jobs.dialog.take().expect("dialog job");
			self.finish_dialog(action, r.flatten());
		}
		if let Some(r) = self.jobs.export.as_ref().and_then(Job::poll) {
			let secs = self
				.jobs
				.export
				.take()
				.map_or(0.0, |j| j.started.elapsed().as_secs_f32());
			match r {
				Some(Ok((path, report))) => {
					for w in report.warnings {
						self.notices.warn(w);
					}
					self.notices.info(format!(
						"Exported {} in {secs:.1} s — peak {:.1} dBFS, {:.1} LUFS",
						path.display(),
						report.master.true_peak_db,
						report.master.lufs_integrated
					));
				}
				Some(Err(e)) => self.notices.error(format!("Export failed: {e}")),
				None => self
					.notices
					.error("Export failed: the render thread stopped"),
			}
		}
		if let Some(r) = self.jobs.scan.as_ref().and_then(Job::poll) {
			self.jobs.scan = None;
			match r {
				Some(Ok((catalog, report))) => {
					self.session.set_plugins(Arc::new(catalog));
					self.browser_items = None;
					self.notices.info(format!(
						"Plugin scan: {} files, {} added, {} updated, {} removed, {} blocked",
						report.files_found,
						report.added.len(),
						report.updated.len(),
						report.removed.len(),
						report.blocked.len()
					));
				}
				Some(Err(e)) => self.notices.error(format!("Plugin scan failed: {e}")),
				None => self
					.notices
					.error("Plugin scan failed: the scan thread stopped"),
			}
		}
	}
}

/// Name of a scale kind as `cadenza_theory::Scale::parse` reads it.
pub fn scale_kind_name(kind: cadenza_theory::ScaleKind) -> &'static str {
	use cadenza_theory::ScaleKind as K;
	match kind {
		K::Major => "major",
		K::NaturalMinor => "minor",
		K::HarmonicMinor => "harmonic minor",
		K::MelodicMinor => "melodic minor",
		K::Dorian => "dorian",
		K::Phrygian => "phrygian",
		K::Lydian => "lydian",
		K::Mixolydian => "mixolydian",
		K::Locrian => "locrian",
		K::MajorPentatonic => "major pentatonic",
		K::MinorPentatonic => "minor pentatonic",
		K::Blues => "blues",
		K::WholeTone => "whole tone",
		K::Chromatic => "chromatic",
	}
}

#[cfg(test)]
mod tests;
