//! Application state and logic, independent of any window so it can be unit-tested.
//!
//! [`AppState`] holds the editing [`Session`] (the same type the MCP server and the AI
//! agent work on: document + undo history, file path, plugin catalog, hosting) as a
//! [`SharedSession`]: the in-app AI producer edits the very same document from its own
//! thread. Views read a snapshot ([`DocView`]: project + history labels) refreshed
//! whenever the session's change key moves, so drawing never takes the lock. Every edit,
//! whatever its origin — [`AppState::apply`], [`AppState::edit_session`] for the shared
//! MCP tool functions such as audio/MIDI import, or the agent's tools — ends in
//! [`AppState::sync_session`]: the project is marked dirty, the selection is pruned and the
//! player follows live or through a debounced reload.

pub mod ai;
pub mod audio_edit;
pub mod automation;
pub mod browser;
pub mod dnd;
pub mod editors;
pub mod edits;
pub mod export;
pub mod files;
pub mod layout;
pub mod live;
pub mod meters;
pub mod notices;
pub mod piano;
pub mod project_io;
pub mod record;
pub mod rows;
pub mod safety;
pub mod samples;
pub mod selection;
pub mod timeline;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cadenza_core::{ClipId, DeviceId, ParamAddress, ParamTarget, Tick, TrackId};
use cadenza_dsp::DeviceRole;
use cadenza_engine::{MeterBank, PlayerClock};
use cadenza_mcp::{ChangeKey, McpError, Session, SharedSession};
use cadenza_model::{ChannelPatch, Command, Document, Project, TrackKind};
use cadenza_plugin_host::{PluginCatalog, ScanReport};

use crate::audio::{AudioEngine, AudioEvent, Waker};
use crate::jobs::{FileAction, Job, PeakCache};
use browser::BrowserItem;
use dnd::{DropAction, DropTarget, Payload};
use editors::PluginEditors;
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
	/// The audio editor (shares the bottom editor dock with the piano roll).
	pub audio: audio_edit::AudioEditorView,
	/// The editor dock shows the audio editor instead of the piano roll.
	pub audio_editor: bool,
	/// The sampler / drum sampler editor (shares the bottom editor dock).
	pub instrument: samples::InstrumentView,
	/// The editor dock shows the instrument editor (over the piano roll / audio editor).
	pub instrument_editor: bool,
	/// Pattern / Song mode and the arrange loop region.
	pub transport: transport::TransportView,
	/// The start screen (recent projects, new, open, demo) is shown.
	pub start_screen: bool,
	/// Tracks whose take lanes are expanded.
	pub takes: Vec<TrackId>,
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
			audio: audio_edit::AudioEditorView::default(),
			audio_editor: false,
			instrument: samples::InstrumentView::default(),
			instrument_editor: false,
			transport: transport::TransportView::default(),
			start_screen: false,
			takes: Vec::new(),
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
	/// Collect project files (after the Save As it needs).
	Collect,
}

/// Background work in flight.
#[derive(Default)]
pub struct Jobs {
	pub scan: Option<Job<Result<(PluginCatalog, ScanReport), String>>>,
	/// An audio editor operation rendering a processed copy.
	pub process: Option<Job<Result<audio_edit::Rendered, String>>>,
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

/// What the views read of the document: the project and the history labels, copied
/// from the shared session after each change (so drawing never waits for its lock).
#[derive(Debug, Clone, PartialEq)]
pub struct DocView {
	project: Project,
	undo: Vec<String>,
	redo: Vec<String>,
}

impl DocView {
	fn of(document: &Document) -> Self {
		Self {
			project: document.project().clone(),
			undo: document
				.history_labels()
				.into_iter()
				.map(str::to_owned)
				.collect(),
			redo: document
				.redo_labels()
				.into_iter()
				.map(str::to_owned)
				.collect(),
		}
	}

	pub fn project(&self) -> &Project {
		&self.project
	}

	/// Labels of undoable steps, oldest first.
	pub fn history_labels(&self) -> Vec<&str> {
		self.undo.iter().map(String::as_str).collect()
	}

	/// Labels of redoable steps, next redo first.
	pub fn redo_labels(&self) -> Vec<&str> {
		self.redo.iter().map(String::as_str).collect()
	}

	pub fn can_undo(&self) -> bool {
		!self.undo.is_empty()
	}

	pub fn can_redo(&self) -> bool {
		!self.redo.is_empty()
	}
}

/// Autosave, session marker and recovery settings (see [`safety`]).
pub struct SafetyState {
	/// Off in headless states (tests opt in with [`AppState::enable_safety`]).
	pub enabled: bool,
	/// Autosave interval in minutes (0 = off).
	pub minutes: u32,
	/// Where unsaved projects autosave and the demo is written.
	pub state_dir: PathBuf,
	/// The marker this app wrote for the open project.
	lock: Option<PathBuf>,
	autosaver: safety::Autosaver,
	/// An autosave failure was reported (only once per session).
	warned: bool,
}

impl Default for SafetyState {
	fn default() -> Self {
		Self {
			enabled: false,
			minutes: safety::DEFAULT_AUTOSAVE_MINUTES,
			state_dir: safety::state_dir(),
			lock: None,
			autosaver: safety::Autosaver::default(),
			warned: false,
		}
	}
}

pub struct AppState {
	/// The document, history, path and plugin catalog, shared with the AI agent. Lock it
	/// only briefly ([`AppState::session`]); views read [`AppState::document`].
	pub session: SharedSession,
	/// Snapshot of the session's document as of `session_key`.
	doc: DocView,
	path: Option<PathBuf>,
	session_key: ChangeKey,
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
	/// Plugin editor windows and the edits made in them.
	pub editors: PluginEditors,
	/// Arm states, record button, metronome/count-in, audio settings, takes view.
	pub record: record::RecordState,
	/// The AI producer chat (see [`ai`] for how edits are locked while it works).
	pub ai: ai::AiChat,
	/// Plugin editor values and finished background edits that arrived while the AI was
	/// editing: applied once it is done.
	ai_queue: Vec<Command>,
	/// What to continue with once a "Save As" dialog finished.
	after_save: Option<Pending>,
	/// The export dialog and the running export.
	pub export: export::ExportDialog,
	/// File → Open Recent (most recent first; persisted in the preferences).
	pub recent: Vec<PathBuf>,
	pub safety: SafetyState,
	/// Unsaved work offered for recovery.
	pub recovery: Option<safety::Recovery>,
	/// The "Locate missing files" dialog.
	pub relink: Option<project_io::RelinkDialog>,
	/// After "Save As" into a folder that lacks the project's files: offer to collect.
	pub collect_prompt: bool,
	/// An archive being opened, waiting for its destination folder.
	pending_archive: Option<PathBuf>,
	pub files_jobs: project_io::FileJobs,
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
	/// Play was pressed (and not stopped since): pattern mode auditions only then.
	play_intent: bool,
	/// The user placed the playhead since the last stop: pattern mode starts there
	/// (inside the pattern) instead of at the pattern start.
	cued: bool,
	/// Loop and audition last sent to the player.
	play_config: Option<transport::PlayConfig>,
}

impl AppState {
	/// State with a live audio engine (the app).
	pub fn new(waker: Option<Waker>) -> Self {
		let mut s = Self::headless_with(waker.clone());
		s.ai = ai::AiChat::new(waker.clone());
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
		let doc = DocView::of(session.document());
		let session_key = session.change_key();
		Self {
			session: session.into_shared(),
			doc,
			path: None,
			session_key,
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
			editors: PluginEditors::default(),
			record: record::RecordState::default(),
			// Tests and headless renders never touch the OS keyring.
			ai: ai::AiChat::with_parts(
				ai::worker::default_factory(),
				Arc::new(ai::secrets::MemorySecretStore::default()),
				std::env::temp_dir().join(format!("cadenza-chat-{}", std::process::id())),
				std::env::current_dir().unwrap_or_default(),
				waker.clone(),
			),
			ai_queue: Vec::new(),
			after_save: None,
			export: export::ExportDialog::default(),
			recent: Vec::new(),
			safety: SafetyState::default(),
			recovery: None,
			relink: None,
			collect_prompt: false,
			pending_archive: None,
			files_jobs: project_io::FileJobs::default(),
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
			play_intent: false,
			cued: false,
			play_config: None,
		}
	}

	pub fn project(&self) -> &Project {
		&self.doc.project
	}

	/// The document as of the last sync (project + history labels).
	pub fn document(&self) -> &DocView {
		&self.doc
	}

	pub fn path(&self) -> Option<&Path> {
		self.path.as_deref()
	}

	/// Locks the shared session. Keep the guard for a short, bounded piece of work: the AI
	/// agent's tools wait for it.
	pub fn session(&self) -> std::sync::MutexGuard<'_, Session> {
		self.session
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
	}

	/// Brings the snapshot up to date with the shared session and runs the edit
	/// bookkeeping (dirty flag, selection pruning, live player updates or a reload) when
	/// something changed — whoever changed it. `wait` = false never blocks (per-frame
	/// polling while the agent may hold the lock). Returns whether it changed.
	pub fn sync_session(&mut self, wait: bool) -> bool {
		let (view, path, key, clean, epoch_changed) = {
			let guard = if wait {
				self.session
					.lock()
					.unwrap_or_else(|poisoned| poisoned.into_inner())
			} else {
				match self.session.try_lock() {
					Ok(g) => g,
					Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
					Err(std::sync::TryLockError::WouldBlock) => return false,
				}
			};
			let key = guard.change_key();
			if key == self.session_key {
				// Saving changes the path and the clean state, not the document.
				if self.path.as_deref() != guard.path() {
					self.path = guard.path().map(Path::to_path_buf);
				}
				if guard.is_clean() {
					self.dirty = false;
				}
				return false;
			}
			(
				DocView::of(guard.document()),
				guard.path().map(Path::to_path_buf),
				key,
				guard.is_clean(),
				key.0 != self.session_key.0,
			)
		};
		let before = std::mem::replace(&mut self.doc, view);
		self.session_key = key;
		self.path = path;
		// A different document (new/open) has nothing to diff against: reload.
		self.after_edit((!epoch_changed).then_some(&before.project));
		if clean {
			self.dirty = false;
		}
		true
	}

	/// Replaces the document (new/open project, tests).
	pub fn load_document(&mut self, document: Document, path: Option<PathBuf>) {
		self.session().reset(document, path);
		self.sync_session(true);
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

	/// Executes one edit. Returns false (with an error notice) if the model rejected it,
	/// or while the AI is editing the project (see [`ai`]).
	pub fn apply(&mut self, cmd: Command) -> bool {
		if self.ai_locked() {
			return false;
		}
		self.apply_inner(cmd)
	}

	/// Like [`AppState::apply`], but while the AI edits the command waits until it is done
	/// (plugin editor values, finished background operations: work the user already did).
	pub fn apply_or_queue(&mut self, cmd: Command) -> bool {
		if self.ai.busy() {
			self.ai_queue.push(cmd);
			return true;
		}
		self.apply_inner(cmd)
	}

	/// Whether the AI is editing (user edits refused, with a notice saying so).
	fn ai_locked(&mut self) -> bool {
		if self.ai.busy() {
			self.notices.info(
				"The AI is editing the project — wait for it or press Stop in the chat (F10)",
			);
			return true;
		}
		false
	}

	// --- AI producer ---------------------------------------------------------------

	/// What the AI is told about the user's view with each request.
	pub fn ai_context(&self) -> cadenza_agent::AppContext {
		let p = self.project();
		let beats = |t: Tick| t.beats();
		let describe_clip = |id: cadenza_core::ClipId| {
			p.find_clip(id).map(|(track, c)| {
				let notes = match &c.content {
					cadenza_model::ClipContent::Midi { notes } => {
						format!(", {} notes", notes.len())
					}
					cadenza_model::ClipContent::Audio { .. } => ", audio".into(),
				};
				format!(
					"\"{}\" (clip {}, track \"{}\", beats {:.2}-{:.2}{notes})",
					c.name,
					c.id,
					track.name,
					beats(c.start),
					beats(c.start + c.length)
				)
			})
		};
		let selected_tracks = self
			.selection
			.track
			.and_then(|t| p.track(t))
			.map(|t| {
				let instrument = t
					.instrument
					.as_ref()
					.map(|d| format!(", instrument {}", d.name))
					.unwrap_or_default();
				format!("\"{}\" (track {}, {:?}{instrument})", t.name, t.id, t.kind)
			})
			.into_iter()
			.chain(
				self.selection
					.master
					.then(|| "the master channel".to_owned()),
			)
			.collect();
		let files_root = self
			.path()
			.and_then(Path::parent)
			.map(Path::to_path_buf)
			.unwrap_or_else(|| self.ai.session_dir().to_path_buf());
		let loop_beats = (self.view.transport.loop_on)
			.then_some(self.view.transport.loop_range)
			.flatten()
			.map(|(a, b)| (beats(a), beats(b)));
		cadenza_agent::AppContext {
			project_path: self.path().map(|p| p.display().to_string()),
			files_root: files_root.display().to_string(),
			selected_tracks,
			selected_clips: self
				.selection
				.clips
				.iter()
				.filter_map(|c| describe_clip(*c))
				.collect(),
			playhead_beats: beats(self.position()),
			loop_beats,
			open_clip: self.view.piano.clip.and_then(describe_clip),
			transport_mode: Some(self.view.transport.mode.name().to_lowercase()),
			recording: self.recording_context(),
		}
	}

	/// Sends `request` to the AI producer (with the app context).
	pub fn send_ai(&mut self, request: &str) -> bool {
		let context = self.ai_context();
		let session = self.session.clone();
		self.ai.send(request, context, &session)
	}

	/// Quick requests for the chat, from the selection.
	pub fn ai_quick_actions(&self) -> Vec<(String, String)> {
		ai::quick_actions(
			self.project(),
			self.selection.track,
			self.selection.clips.iter().next().copied(),
		)
	}

	/// Chat events of the frame; a finished request brings its edits in and releases the
	/// queued commands.
	fn poll_ai(&mut self) {
		for finished in self.ai.poll() {
			self.sync_session(true);
			if finished.edited {
				self.notices
					.info(format!("{} — one undo step (Ctrl+Z)", finished.undo_label));
			}
		}
		if !self.ai.busy() && !self.ai_queue.is_empty() {
			for cmd in std::mem::take(&mut self.ai_queue) {
				self.apply_inner(cmd);
			}
		}
	}

	fn apply_inner(&mut self, cmd: Command) -> bool {
		let result = self.session().document_mut().execute(cmd);
		match result {
			Ok(()) => {
				self.sync_session(true);
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
		if self.ai_locked() {
			return None;
		}
		let result = f(&mut self.session());
		self.sync_session(true);
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
		self.selection.prune(&self.doc.project);
		self.view.piano.prune(&self.doc.project);
		self.view.automation.prune(&self.doc.project);
		self.view.audio.prune(&self.doc.project);
		self.view.instrument.prune(&self.doc.project);
		self.prune_record();
		if self.view.instrument.device.is_none() {
			self.view.instrument_editor = false;
		}
		let project = &self.doc.project;
		self.meters
			.retain(|id| project.tracks.iter().any(|t| t.id == id));
		self.editors.prune(project);
		let editors = &self.editors;
		let changes = before.and_then(|b| {
			live::diff(b, self.project(), &|device, state| {
				editors.live_state(device, state)
			})
		});
		match changes {
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
		for &track in &changes.clips {
			let p = self.project();
			if let Some(t) = p.track(track) {
				a.track_clips(track, t.clips.clone(), p.tempo.clone());
			}
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
		self.session().plugins()
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
		if self.ai_locked() {
			return;
		}
		let result = self.session().document_mut().undo();
		match result {
			Ok(label) => {
				self.notices.info(format!("Undo: {label}"));
				self.sync_session(true);
			}
			Err(e) => self.notices.info(e.to_string()),
		}
	}

	pub fn redo(&mut self) {
		if self.ai_locked() {
			return;
		}
		let result = self.session().document_mut().redo();
		match result {
			Ok(label) => {
				self.notices.info(format!("Redo: {label}"));
				self.sync_session(true);
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
		if let Some(v) = self.editors.live_value(device, param) {
			return Some(v);
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
			let catalog = self.session().plugins();
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
			if item.role == DeviceRole::Instrument {
				self.instrument_hint(&item.name);
			}
		}
		ok
	}

	/// After an instrument lands on a track (which is now selected): how to write notes.
	fn instrument_hint(&mut self, name: &str) {
		self.notices.info(format!(
			"{name} added — press F7 or double-click the track to open the piano roll"
		));
	}

	/// Loads a built-in factory preset into a device (one undo step); sampler presets
	/// bring their (factory) samples.
	pub fn load_factory_preset(&mut self, device: DeviceRef, preset: &cadenza_dsp::FactoryPreset) {
		let params = preset
			.params
			.iter()
			.map(|(k, v)| ((*k).to_owned(), *v))
			.collect();
		let set = Command::SetDeviceState {
			track: device.track,
			device: device.device,
			state: None,
			params: Some(params),
		};
		let cmd = if preset.zones.is_empty() {
			set
		} else {
			match cadenza_engine::factory::preset_zones(preset) {
				Ok(zones) => Command::Batch {
					label: format!("Load preset '{}'", preset.name),
					commands: vec![
						set,
						Command::SetSampleZones {
							track: device.track,
							device: device.device,
							zones,
						},
					],
				},
				Err(e) => {
					self.notices.error(e.to_string());
					return;
				}
			}
		};
		self.apply(cmd);
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

	/// Whether `device` is a third-party plugin (CLAP/VST3), which may have an editor.
	pub fn is_plugin(&self, device: DeviceId) -> bool {
		editors::locate(self.project(), device)
			.and_then(|track| self.project().device(track, device))
			.is_some_and(|d| !matches!(d.kind, cadenza_model::DeviceKind::Builtin { .. }))
	}

	pub fn plugin_editor_open(&self, device: DeviceId) -> bool {
		self.editors.is_open(device)
	}

	/// Opens the plugin's editor window, or closes it when open.
	pub fn toggle_plugin_editor(&mut self, device: DeviceId) {
		if self.editors.is_open(device) {
			self.close_plugin_editor(device);
		} else {
			self.open_plugin_editor(device);
		}
	}

	/// Opens (or raises) the editor window of plugin device `device`, titled
	/// "<device> — <track>". Edits made there come back as commands (see
	/// [`editors`]).
	pub fn open_plugin_editor(&mut self, device: DeviceId) {
		let project = self.project();
		let Some(d) = editors::locate(project, device).and_then(|t| project.device(t, device))
		else {
			return;
		};
		if matches!(d.kind, cadenza_model::DeviceKind::Builtin { .. }) {
			self.notices.info(format!(
				"{} is a built-in device: edit it in the inspector (F8)",
				d.name
			));
			return;
		}
		if d.bypass {
			self.notices.warn(format!(
				"{} is bypassed: enable it to open its editor",
				d.name
			));
			return;
		}
		if !cadenza_engine::EDITORS_SUPPORTED {
			self.notices
				.warn("Plugin editor windows are not available on this OS yet; use the inspector");
			return;
		}
		let Some(title) = editors::title(project, device) else {
			return;
		};
		match &self.audio_status {
			AudioStatus::Unavailable(e) => {
				self.notices.error(format!(
					"Plugin editors need the audio engine, which is unavailable: {e}"
				));
				return;
			}
			AudioStatus::Off => {
				self.notices
					.error("Plugin editors need the audio engine (not running)");
				return;
			}
			_ => {}
		}
		if self.reload_pending() || self.loaded_generation < self.generation || self.clock.is_none()
		{
			// The player does not have this device yet: open once it does.
			self.editors.queue(device, title);
			return;
		}
		self.editors.opened(device);
		if let Some(a) = &self.audio {
			a.open_editor(device, title);
		}
	}

	/// Closes the plugin's editor window, keeping what was changed there.
	pub fn close_plugin_editor(&mut self, device: DeviceId) {
		self.editors.closed(device);
		if let Some(a) = &self.audio {
			a.close_editor(device);
			a.capture_states(self.project().clone(), vec![device]);
		}
	}

	/// What the user did in plugin editors (from the player): gestures become commands.
	pub fn editor_updates(&mut self, updates: Vec<cadenza_engine::EditorUpdate>, now: Instant) {
		let actions = self.editors.handle(&self.doc.project, updates, now);
		self.editor_actions(actions);
	}

	/// Applies commands produced by plugin editors and asks for state captures.
	fn editor_actions(&mut self, actions: editors::Actions) {
		for cmd in actions.commands {
			self.apply_or_queue(cmd);
		}
		if !actions.capture.is_empty()
			&& let Some(a) = &self.audio
		{
			a.capture_states(self.project().clone(), actions.capture);
		}
	}

	/// Stores live plugin states (no reload: the instances already hold them).
	pub fn store_editor_states(&mut self, states: Vec<cadenza_engine::EditorState>) {
		let commands = self.editors.store_states(&self.doc.project, states);
		for cmd in commands {
			self.apply_or_queue(cmd);
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

	/// Plays in the current mode: the song (looping the loop region when it is on), or
	/// in pattern mode the clip open in the piano roll, looped and heard alone.
	pub fn play(&mut self) {
		// Record mode (R): Play records.
		if self.record.mode && !self.is_recording() {
			self.record();
			return;
		}
		// A pending edit must reach the player before it starts.
		if self.reload_since.is_some() {
			self.send_reload();
		}
		self.play_intent = true;
		let (config, fell_back) = self.transport_config();
		if fell_back {
			self.notices.info(
				"Pattern mode plays the clip open in the piano roll — open one with F7. \
				 Playing the song instead.",
			);
		}
		// Start inside the loop: pattern mode at the pattern start (FL Studio) unless the
		// playhead was placed inside it, song mode when past the loop end.
		if let Some((start, end)) = config.loop_range {
			let pos = self.position();
			let outside = if config.pattern.is_some() {
				!self.cued || pos < start || pos >= end
			} else {
				pos >= end
			};
			if outside {
				self.locate(start);
			}
		}
		self.send_config(config);
		match &self.audio {
			Some(a) if self.clock.is_some() => a.play(),
			_ => self
				.notices
				.warn("No audio output: playback is unavailable"),
		}
	}

	/// Stops; pattern mode goes back to the pattern start (FL Studio).
	pub fn stop(&mut self) {
		self.stop_recording();
		self.play_intent = false;
		self.cued = false;
		if let Some(a) = &self.audio {
			a.stop();
		}
		let (config, _) = self.transport_config();
		if let (Some(_), Some((start, _))) = (config.pattern, config.loop_range) {
			self.locate(start);
		}
		self.send_config(config);
	}

	/// Play was pressed and not stopped since (also without an audio device).
	pub fn play_intent(&self) -> bool {
		self.play_intent
	}

	/// What the player plays for the current mode (second value: pattern mode fell back
	/// to the song because no clip is open in the piano roll).
	pub fn transport_config(&self) -> (transport::PlayConfig, bool) {
		transport::config(
			self.project(),
			&self.view.transport,
			self.view.piano.clip.filter(|_| !self.view.audio_editor),
			self.play_intent,
		)
	}

	/// Loop and audition last sent to the player.
	pub fn play_config(&self) -> Option<&transport::PlayConfig> {
		self.play_config.as_ref()
	}

	/// Sends loop and audition to the player when they changed.
	fn send_config(&mut self, config: transport::PlayConfig) {
		if self.play_config.as_ref() == Some(&config) {
			return;
		}
		if let Some(a) = &self.audio {
			let old = self.play_config.as_ref();
			if old.map(|c| c.loop_range) != Some(config.loop_range) {
				a.set_loop(config.loop_range);
			}
			if old.map(|c| &c.audition) != Some(&config.audition) {
				a.audition(config.audition.clone());
			}
		}
		self.play_config = Some(config);
	}

	/// Keeps the player in step with the mode, the loop region and the pattern clip (it
	/// may grow, or another clip may be opened while playing).
	fn sync_transport(&mut self) {
		let (config, _) = self.transport_config();
		if self.play_config.as_ref() == Some(&config) {
			return;
		}
		let new_pattern = config.pattern.is_some()
			&& self.play_config.as_ref().map(|c| c.pattern) != Some(config.pattern);
		if self.play_intent
			&& new_pattern
			&& let Some((start, end)) = config.loop_range
		{
			let pos = self.position();
			if pos < start || pos >= end {
				self.locate(start);
			}
		}
		self.send_config(config);
	}

	/// Switches Pattern / Song mode; a running transport restarts in the new mode.
	pub fn set_play_mode(&mut self, mode: transport::PlayMode) {
		if self.view.transport.mode == mode {
			return;
		}
		let playing = self.play_intent;
		if playing {
			self.stop();
		}
		self.view.transport.mode = mode;
		if playing {
			self.play();
		} else {
			let (config, _) = self.transport_config();
			self.send_config(config);
		}
	}

	/// The L key: Pattern <-> Song.
	pub fn toggle_play_mode(&mut self) {
		self.set_play_mode(self.view.transport.mode.toggled());
	}

	/// Turns the arrange loop on/off (Ctrl+L); without a region, loops the bar at the
	/// playhead.
	pub fn toggle_loop(&mut self) {
		if self.view.transport.loop_range.is_none() {
			let bar = self.project().time_signature.ticks_per_bar();
			let pos = self.position();
			let start = Tick(pos.0.max(0) / bar.0 * bar.0);
			self.view.transport.loop_range = Some((start, start + bar));
		}
		self.view.transport.loop_on = !self.view.transport.loop_on;
		self.sync_transport();
	}

	/// Sets the arrange loop region (and turns the loop on); `None` clears it.
	pub fn set_loop_range(&mut self, range: Option<(Tick, Tick)>) {
		let t = &mut self.view.transport;
		t.loop_range = range.filter(|(s, e)| e > s);
		t.loop_on = t.loop_range.is_some();
		self.sync_transport();
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

	/// Moves the playhead (a user action: pattern mode then plays from there).
	pub fn seek(&mut self, tick: Tick) {
		self.cued = true;
		self.locate(tick);
	}

	/// Moves the playhead without cueing (transport bookkeeping).
	fn locate(&mut self, tick: Tick) {
		let tick = Tick(tick.0.max(0));
		self.position = tick;
		if let Some(a) = &self.audio {
			a.seek(tick);
		}
	}

	// --- Files ---------------------------------------------------------------------

	/// Runs `pending` now, or asks first if there are unsaved changes.
	pub fn request(&mut self, pending: Pending) {
		if pending == Pending::Quit {
			self.ai.stop();
		} else if self.ai_locked() {
			return;
		}
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
			Pending::Collect => self.collect_files(),
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

	fn reset_view(&mut self) {
		let open: Vec<DeviceId> = self.editors.open_devices().collect();
		if let Some(a) = &self.audio {
			for device in open {
				a.close_editor(device);
			}
		}
		self.editors = PluginEditors::default();
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
		self.view.audio = audio_edit::AudioEditorView::default();
		self.view.audio_editor = false;
		self.view.piano.recent.clear();
		self.view.transport.loop_on = false;
		self.view.transport.loop_range = None;
		self.load_warnings.clear();
		self.locate(Tick::ZERO);
		self.cued = false;
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
		self.capture_editors_now();
		let args = cadenza_mcp::tools::project::SaveProjectArgs {
			path: path.map(|p| p.display().to_string()),
			capture_plugin_states: None,
		};
		let was = self.path().map(Path::to_path_buf);
		let saved = self.edit_session(|s| cadenza_mcp::tools::project::save_project(s, args));
		match saved {
			Some(out) => {
				self.dirty = false;
				self.after_save(was);
				for w in out.warnings {
					self.notices.warn(w);
				}
				self.notices.info(format!("Saved {}", out.path));
				true
			}
			None => false,
		}
	}

	/// Stores what plugin editors changed and the project does not have yet (pending
	/// one-off values, live states of open editors). Blocks briefly: used before saving.
	fn capture_editors_now(&mut self) {
		let actions = self.editors.flush(&self.doc.project, Instant::now(), true);
		let mut devices = actions.capture.clone();
		self.editor_actions(editors::Actions {
			capture: Vec::new(),
			..actions
		});
		devices.extend(self.editors.needs_capture());
		devices.sort_unstable_by_key(|d| d.to_string());
		devices.dedup();
		if devices.is_empty() {
			return;
		}
		let Some(a) = &self.audio else { return };
		let states = a.capture_states_now(
			self.project().clone(),
			devices,
			crate::audio::CAPTURE_TIMEOUT,
		);
		self.store_editor_states(states);
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
			DropAction::LoadSample { device, path, slot } => self.load_sample(device, &path, slot),
			DropAction::NewSampler { path } => self.new_sampler_track(&path),
			DropAction::Reject(why) => {
				self.notices.info(why);
				false
			}
		}
	}

	fn build_device(&mut self, item: &BrowserItem) -> Option<cadenza_model::Device> {
		let catalog = (item.source == browser::Source::Plugin).then(|| self.session().plugins());
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
			if item.role == DeviceRole::Instrument {
				self.instrument_hint(&item.name);
			}
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
		match cadenza_engine::factory::preset_zones(preset) {
			Ok(zones) => d.zones = zones,
			Err(e) => self.notices.error(e.to_string()),
		}
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
		self.view.piano.recent.insert(track, clip);
		self.view.audio_editor = false;
		self.view.instrument_editor = false;
		self.selection.click_clip(track, clip, false);
		self.view.panels.open_piano_roll();
		true
	}

	/// Opens the piano roll for instrument track `track`: its selected clip, else the
	/// clip under the playhead, else the last one opened there, else a new empty one-bar
	/// clip at the playhead (one undo step, named after the track).
	pub fn open_track_in_piano_roll(&mut self, track: TrackId) -> bool {
		let Some(t) = self.project().track(track) else {
			return false;
		};
		if t.kind != TrackKind::Instrument {
			let msg = format!(
				"'{}' is not an instrument track: the piano roll edits MIDI clips",
				t.name
			);
			self.notices.info(msg);
			return false;
		}
		let found = piano::clip_for_track(
			self.project(),
			track,
			&self.selection.clips,
			self.position(),
			self.view.piano.recent.get(&track).copied(),
		);
		match found {
			Some(clip) => self.open_piano_roll(clip),
			None => self.new_clip_in_piano_roll(track, self.position()),
		}
	}

	/// Creates an empty one-bar clip on `track` at the first free bar from `at` and opens
	/// it in the piano roll (one undo step).
	pub fn new_clip_in_piano_roll(&mut self, track: TrackId, at: Tick) -> bool {
		let Some((cmd, clip)) = piano::new_clip(self.project(), track, at) else {
			return false;
		};
		self.apply(cmd) && self.open_piano_roll(clip)
	}

	/// F7: hides a visible editor dock; otherwise opens the piano roll for the selected
	/// instrument track (see [`AppState::open_track_in_piano_roll`]), or the selected clip
	/// of another kind in its editor, or just shows the dock.
	pub fn toggle_piano_roll(&mut self) {
		if self.view.panels.visible(PanelKind::PianoRoll) {
			self.close_piano_roll();
			return;
		}
		let track = self
			.selection
			.track
			.filter(|t| self.project().track(*t).map(|t| t.kind) == Some(TrackKind::Instrument));
		if let Some(track) = track {
			self.open_track_in_piano_roll(track);
			return;
		}
		let selected = self.selection.clips.iter().copied().next();
		let opened = self.view.piano.clip.is_some() || self.view.audio.clip.is_some();
		if let Some(clip) = selected.filter(|_| !opened) {
			self.open_editor(clip);
			return;
		}
		self.view.panels.open_piano_roll();
	}

	/// Opens `clip` in its editor: the piano roll for MIDI, the audio editor for audio.
	pub fn open_editor(&mut self, clip: ClipId) -> bool {
		match self.project().find_clip(clip) {
			Some((_, c)) if c.is_midi() => self.open_piano_roll(clip),
			Some(_) => self.open_audio_editor(clip),
			None => false,
		}
	}

	/// Opens an audio clip in the audio editor (the bottom editor dock).
	pub fn open_audio_editor(&mut self, clip: ClipId) -> bool {
		let Some(r) = audio_edit::region(self.project(), clip) else {
			return false;
		};
		self.release_previews();
		if self.view.audio.clip != Some(clip) {
			self.view.audio.selection = None;
			self.view.audio.framed = None;
		}
		self.view.audio.clip = Some(clip);
		self.view.audio_editor = true;
		self.view.instrument_editor = false;
		self.selection.click_clip(r.track, clip, false);
		self.view.panels.open_piano_roll();
		true
	}

	/// Clip-based edit of the audio editor (one command; errors become notices).
	pub fn audio_edit(&mut self, cmd: Option<Command>) -> bool {
		match cmd {
			Some(cmd) => self.apply(cmd),
			None => false,
		}
	}

	/// Renders `op` over the editor's selection (or the whole clip region) into a new
	/// file next to the project, then swaps it into the clip (one undo step).
	pub fn process_audio(&mut self, op: audio_edit::Process) {
		if !op.available() {
			self.notices.warn(audio_edit::STRETCH_HINT);
			return;
		}
		if self.jobs.process.is_some() {
			self.notices.warn("An audio operation is already running");
			return;
		}
		let Some(clip) = self.view.audio.clip else {
			return;
		};
		let Some(r) = audio_edit::region(self.project(), clip) else {
			return;
		};
		let range = r.clamp(self.view.audio.selection.unwrap_or((r.offset, r.end())));
		if range.1 - range.0 <= 1e-6 {
			self.notices.info("Select a range of the clip first");
			return;
		}
		let out = audio_edit::processed_path(self.path(), &r.file, op);
		let source = r.file.clone();
		self.notices.info(format!("{}…", op.label()));
		self.jobs.process = Some(Job::spawn("audio-process", self.waker.clone(), move || {
			let extra = audio_edit::render_processed(&source, &out, range, op)?;
			Ok(audio_edit::Rendered {
				clip,
				op,
				file: out,
				extra_seconds: extra,
			})
		}));
	}

	// --- Sampler / drum sampler editor ---------------------------------------------

	/// Whether `device` is a built-in sampler or drum sampler (it has the instrument editor).
	pub fn is_sample_device(&self, r: DeviceRef) -> bool {
		self.device(r).and_then(samples::kind).is_some()
	}

	/// Shows `r`'s sampler editor in the bottom editor dock.
	pub fn open_instrument_editor(&mut self, r: DeviceRef) -> bool {
		if !self.is_sample_device(r) {
			return false;
		}
		self.release_previews();
		if self.view.instrument.device != Some(r) {
			self.view.instrument.zone = 0;
		}
		self.view.instrument.device = Some(r);
		self.view.instrument_editor = true;
		self.selection.select_device(r);
		self.view.panels.open_piano_roll();
		true
	}

	/// Toggles the sampler editor of `r` (inspector button, double-clicks).
	pub fn toggle_instrument_editor(&mut self, r: DeviceRef) {
		let shown = self.view.instrument_editor
			&& self.view.instrument.device == Some(r)
			&& self.view.panels.visible(PanelKind::PianoRoll);
		if shown {
			self.close_piano_roll();
		} else {
			self.open_instrument_editor(r);
		}
	}

	/// Leaves the instrument editor for the piano roll / audio editor in the dock.
	pub fn close_instrument_editor(&mut self) {
		self.release_previews();
		self.view.instrument_editor = false;
	}

	/// Loads `path` into a pad, zone or the whole keyboard of a sample device (the shared
	/// `load_sample` tool: root from the name or pitch, pad settings kept; one undo step).
	pub fn load_sample(&mut self, r: DeviceRef, path: &Path, slot: samples::SampleSlot) -> bool {
		let Some(track) = r.track else {
			return false;
		};
		let args = samples::load_args(track, r, path, slot);
		let ok = self
			.edit_session(|s| cadenza_mcp::tools::samples::load_sample(s, args))
			.is_some();
		if ok {
			match slot {
				samples::SampleSlot::Pad(n) => self.view.instrument.pad = n,
				samples::SampleSlot::Zone(i) => self.view.instrument.zone = i,
				samples::SampleSlot::Keyboard => self.view.instrument.zone = 0,
			}
			self.open_instrument_editor(r);
		}
		ok
	}

	/// A new instrument track (below the others) with a sampler playing `path` across the
	/// keyboard at its root key; one undo step. Opens the sampler editor.
	pub fn new_sampler_track(&mut self, path: &Path) -> bool {
		let zone = match cadenza_engine::auto_zone(path) {
			Ok(z) => z,
			Err(e) => {
				self.notices.error(e.to_string());
				return false;
			}
		};
		let name = zone.sample.stem();
		let device = cadenza_model::Device::builtin("sampler", "Sampler").with_zones(vec![zone]);
		let r_device = device.id;
		let track = cadenza_model::Track::new(name, TrackKind::Instrument).with_instrument(device);
		let track_id = track.id;
		if !self.apply(Command::AddTrack { track, index: None }) {
			return false;
		}
		self.selection.select_track(track_id);
		self.open_instrument_editor(DeviceRef {
			track: Some(track_id),
			device: r_device,
		})
	}

	/// Applies a zone/pad edit built by [`samples`] (one command per gesture).
	pub fn apply_sample_edit(&mut self, cmd: Option<Command>) -> bool {
		cmd.is_some_and(|c| self.apply(c))
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
			piano_snap: v.piano.snap,
			piano_scale: v
				.piano
				.scale
				.map(|s| format!("{} {}", s.root.name(false), scale_kind_name(s.kind)))
				.unwrap_or_default(),
			bookmarks: v.bookmarks.clone(),
			ai: self.ai.settings.clone(),
			recent: self.recent.clone(),
			autosave_minutes: self.safety.minutes,
			export: self.export.settings.clone(),
			audio: self.record.audio.clone(),
			record: self.record.settings,
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
		v.piano.snap = p.piano_snap;
		v.piano.scale = cadenza_theory::Scale::parse(&p.piano_scale).ok();
		v.bookmarks = p.bookmarks;
		self.ai.settings = p.ai.sanitized();
		self.recent = p.recent;
		self.recent.truncate(safety::RECENT_MAX);
		self.safety.minutes = p.autosave_minutes.min(60);
		self.export.settings = p.export;
		if !export::FormatChoice::all().contains(&self.export.settings.format) {
			self.export.settings.format = export::FormatChoice::Wav24;
		}
		if self.files.dir.is_none() {
			self.files.dir = v.bookmarks.first().cloned();
		}
		self.record.settings = p.record;
		self.apply_audio_settings(p.audio);
	}

	/// Rescans plugins in the background (the scanner loads each new plugin in a helper
	/// process).
	pub fn rescan_plugins(&mut self) {
		if self.jobs.scan.is_some() {
			return;
		}
		let cache = self.session().plugin_cache_path().to_path_buf();
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
			FileAction::PackArchive => format!("{stem}.cadenza.zip"),
			_ => format!("{stem}.cadenza.json"),
		};
		let dir = match action {
			FileAction::ExportFolder => self.export.folder.clone().or(dir),
			_ => dir,
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
			FileAction::ExportFolder
			| FileAction::UnpackFolder
			| FileAction::RelinkFolder
			| FileAction::PackArchive => self.finish_file_action(action, path),
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
		self.editors.reloaded();
		let (plugins, hosting) = {
			let mut session = self
				.session
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			(
				session.plugins_for_project(),
				session.plugin_hosting().clone(),
			)
		};
		audio.load(self.doc.project.clone(), plugins, hosting, self.generation);
	}

	/// Per-frame housekeeping: player events, debounced reloads, background jobs and
	/// meter ballistics. Returns true while something animates (repaint again soon).
	pub fn tick(&mut self, now: Instant) -> bool {
		let dt = self
			.last_tick
			.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32())
			.min(0.25);
		self.last_tick = Some(now);
		// Edits made by other owners of the session (the AI agent).
		self.sync_session(false);
		self.poll_ai();
		self.poll_audio();
		if let Some(since) = self.reload_since
			&& now.saturating_duration_since(since) >= RELOAD_DEBOUNCE
		{
			self.send_reload();
		}
		let actions = self.editors.flush(&self.doc.project, now, false);
		self.editor_actions(actions);
		self.poll_jobs();
		self.poll_file_jobs();
		self.tick_autosave(now);
		self.sync_transport();
		self.tick_record(dt);
		self.peaks.poll();
		let ids: Vec<TrackId> = self.project().tracks.iter().map(|t| t.id).collect();
		self.meters.update(self.bank.as_deref(), ids, dt);
		self.is_playing()
			|| self.meters.any_active()
			|| self.reload_since.is_some()
			|| self.editors.busy()
			|| self.jobs.scan.is_some()
			|| self.export.running.is_some()
			|| self.files_jobs.busy()
			|| self.jobs.process.is_some()
			|| self.ai.busy()
			|| self.record_animating()
	}

	fn poll_audio(&mut self) {
		let Some(audio) = &self.audio else { return };
		let mut events = Vec::new();
		while let Some(e) = audio.poll() {
			events.push(e);
		}
		for event in events {
			match event {
				AudioEvent::Ready {
					clock,
					input,
					status,
				} => {
					self.record.input_stats = Some(input);
					self.record.status = Some(status);
					self.resend_record_state();
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
					if self.loaded_generation >= self.generation && self.reload_since.is_none() {
						for (device, title) in self.editors.take_queued() {
							if let Some(a) = &self.audio {
								a.open_editor(device, title);
							}
						}
					}
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
				AudioEvent::Editor(updates) => self.editor_updates(updates, Instant::now()),
				AudioEvent::EditorStates(states) => self.store_editor_states(states),
				AudioEvent::EditorFailed { device, error } => {
					self.editors.closed(device);
					self.notices.error(error);
				}
				AudioEvent::Devices { audio, midi } => self.record.devices = Some((audio, midi)),
				AudioEvent::RecordingStarted(m) => {
					self.record_event(record::RecordEvent::Started(m));
				}
				AudioEvent::Recorded(rec) => self.record_event(record::RecordEvent::Done(rec)),
				AudioEvent::RecordFailed(e) => self.record_event(record::RecordEvent::Failed(e)),
				AudioEvent::Error(e) => self.notices.error(format!("Audio: {e}")),
			}
		}
	}

	fn poll_jobs(&mut self) {
		if let Some(r) = self.jobs.dialog.as_ref().and_then(|(_, j)| j.poll()) {
			let (action, _) = self.jobs.dialog.take().expect("dialog job");
			self.finish_dialog(action, r.flatten());
		}
		if let Some(r) = self.jobs.process.as_ref().and_then(Job::poll) {
			self.jobs.process = None;
			match r {
				Some(Ok(done)) => match audio_edit::swap_in(self.project(), &done) {
					Some(cmd) => {
						if self.apply_or_queue(cmd) {
							self.notices.info(format!(
								"{}: wrote {}",
								done.op.label(),
								done.file.display()
							));
						}
					}
					None => self
						.notices
						.warn("The clip was removed before the operation finished"),
				},
				Some(Err(e)) => self.notices.error(format!("Audio operation failed: {e}")),
				None => self
					.notices
					.error("Audio operation failed: the worker thread stopped"),
			}
		}
		if let Some(r) = self.jobs.scan.as_ref().and_then(Job::poll) {
			self.jobs.scan = None;
			match r {
				Some(Ok((catalog, report))) => {
					self.session().set_plugins(Arc::new(catalog));
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
