//! Application state and logic, independent of any window so it can be unit-tested.
//!
//! [`AppState`] owns the editing [`Session`] (the same type the MCP server and the AI
//! agent work on: document + undo history, file path, plugin catalog, hosting). Every
//! edit, whatever its origin, goes through [`AppState::apply`] (or
//! [`AppState::edit_session`] for the shared MCP tool functions such as audio/MIDI
//! import): the command is validated and recorded by the `Document`, the project is
//! marked dirty, the selection is pruned and a debounced player reload is scheduled.
//! Views only read state and call these methods.

pub mod browser;
pub mod edits;
pub mod meters;
pub mod notices;
pub mod selection;
pub mod timeline;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cadenza_core::{DeviceId, Tick, TrackId};
use cadenza_dsp::DeviceRole;
use cadenza_engine::{MeterBank, MixReport, PlayerClock, RenderOptions};
use cadenza_mcp::{McpError, Session};
use cadenza_model::{ChannelPatch, Command, Document, Project, TrackKind};
use cadenza_plugin_host::{PluginCatalog, ScanReport};

use crate::audio::{AudioEngine, AudioEvent, Waker};
use crate::jobs::{FileAction, Job, PeakCache};
use browser::BrowserItem;
use meters::Meters;
use notices::Notices;
use selection::{DeviceRef, Selection};
use timeline::Timeline;

pub use cadenza_mcp::tools::audio::AUDIO_EXTENSIONS;

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
	pub show_browser: bool,
	pub show_mixer: bool,
	pub show_inspector: bool,
	pub snap: bool,
	pub follow_playhead: bool,
}

impl Default for ViewState {
	fn default() -> Self {
		Self {
			timeline: Timeline::default(),
			track_height: DEFAULT_TRACK_HEIGHT,
			scroll_y: 0.0,
			show_browser: true,
			show_mixer: true,
			show_inspector: true,
			snap: true,
			follow_playhead: true,
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

/// Fader values being dragged: heard live and drawn, committed as one command on release.
#[derive(Debug, Default, Clone)]
pub struct Previews {
	pub volume: HashMap<Option<TrackId>, f32>,
	pub pan: HashMap<Option<TrackId>, f32>,
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
		self.apply_inner(cmd, true)
	}

	fn apply_inner(&mut self, cmd: Command, reload: bool) -> bool {
		match self.session.document_mut().execute(cmd) {
			Ok(()) => {
				self.after_edit(reload);
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
		let before = self.document().history_labels().len();
		let result = f(&mut self.session);
		if self.document().history_labels().len() != before {
			self.after_edit(true);
		}
		match result {
			Ok(v) => Some(v),
			Err(e) => {
				self.notices.error(e.to_string());
				None
			}
		}
	}

	fn after_edit(&mut self, reload: bool) {
		self.dirty = true;
		self.selection.prune(self.session.document().project());
		let project = self.session.document().project();
		self.meters
			.retain(|id| project.tracks.iter().any(|t| t.id == id));
		if reload || self.loaded_generation < self.generation {
			self.schedule_reload();
		}
	}

	pub fn undo(&mut self) {
		match self.session.document_mut().undo() {
			Ok(label) => {
				self.notices.info(format!("Undo: {label}"));
				self.after_edit(true);
			}
			Err(e) => self.notices.info(e.to_string()),
		}
	}

	pub fn redo(&mut self) {
		match self.session.document_mut().redo() {
			Ok(label) => {
				self.notices.info(format!("Redo: {label}"));
				self.after_edit(true);
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

	/// Ends a fader gesture with one undoable command. The player already plays the
	/// value, so no recompilation (which would cut sounding notes) is needed.
	pub fn commit_volume(&mut self, track: Option<TrackId>, db: f32) {
		self.previews.volume.remove(&track);
		if self.project().channel(track).map(|c| c.volume_db) == Some(db) {
			return;
		}
		if let Some(a) = &self.audio {
			a.live_volume(track, db);
		}
		self.apply_inner(
			Command::SetChannel {
				track,
				patch: ChannelPatch {
					volume_db: Some(db),
					..ChannelPatch::default()
				},
			},
			false,
		);
	}

	pub fn commit_pan(&mut self, track: Option<TrackId>, pan: f32) {
		self.previews.pan.remove(&track);
		if self.project().channel(track).map(|c| c.pan) == Some(pan) {
			return;
		}
		if let Some(a) = &self.audio {
			a.live_pan(track, pan);
		}
		self.apply_inner(
			Command::SetChannel {
				track,
				patch: ChannelPatch {
					pan: Some(pan),
					..ChannelPatch::default()
				},
			},
			false,
		);
	}

	/// Volume shown for a strip (the dragged value while a gesture is running).
	pub fn volume_db(&self, track: Option<TrackId>) -> f32 {
		self.previews
			.volume
			.get(&track)
			.copied()
			.or_else(|| self.project().channel(track).map(|c| c.volume_db))
			.unwrap_or(0.0)
	}

	pub fn pan(&self, track: Option<TrackId>) -> f32 {
		self.previews
			.pan
			.get(&track)
			.copied()
			.or_else(|| self.project().channel(track).map(|c| c.pan))
			.unwrap_or(0.0)
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
		let catalog = (item.source == browser::Source::Plugin).then(|| self.session.plugins());
		let spec = cadenza_mcp::convert::DeviceSpec {
			device: item.id.clone(),
			params: Default::default(),
		};
		let device = match cadenza_mcp::convert::build_device(&spec, item.role, catalog.as_deref())
		{
			Ok(d) => d,
			Err(e) => {
				self.notices.error(e.to_string());
				return false;
			}
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
		let args = cadenza_mcp::tools::audio::ImportAudioArgs {
			path: path.display().to_string(),
			track_id: track,
			at_beats: Some(self.position().beats()),
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
		let args = cadenza_mcp::tools::midi::ImportMidiArgs {
			path: path.display().to_string(),
			split_by_channel: false,
			track_id: None,
			at_beats: None,
		};
		if let Some(out) = self.edit_session(|s| cadenza_mcp::tools::midi::import_midi(s, args)) {
			for w in out.warnings {
				self.notices.warn(w);
			}
			self.notices.info(format!(
				"Imported {} ({} tracks)",
				path.display(),
				out.created_tracks.len()
			));
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

#[cfg(test)]
mod tests;
