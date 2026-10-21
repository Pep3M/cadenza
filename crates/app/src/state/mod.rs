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
pub mod ai_changes;
pub mod arrange_tool;
pub mod audio_edit;
pub mod automation;
pub mod autowrite;
pub mod bounce;
pub mod browser;
pub mod chorus_editor;
pub mod collisions;
pub mod compressor_editor;
pub mod delay_editor;
pub mod device;
pub mod device_ab;
pub mod device_display;
pub mod dnd;
pub mod drum_machine_editor;
pub mod drum_sampler_editor;
pub mod editors;
pub mod edits;
pub mod electric_piano_editor;
pub mod eq_editor;
pub mod export;
pub mod files;
pub mod folders;
pub mod gain_editor;
pub mod inspector;
pub mod layout;
pub mod library;
pub mod limiter_editor;
pub mod live;
pub mod live_voices;
pub mod loudness_meter;
pub mod match_eq;
pub mod meter;
pub mod meters;
pub mod midi_learn;
pub mod mix;
pub mod mixer;
pub mod modulation;
pub mod notices;
pub mod piano;
pub mod playlist;
pub mod poly_synth_editor;
pub mod project_io;
pub mod record;
pub mod reverb_editor;
pub mod rows;
pub mod safety;
pub mod sampler_editor;
pub mod samples;
pub mod saturator_editor;
pub mod selection;
pub mod start;
pub mod timeline;
pub mod transport;
pub mod windows;
pub mod xruns;

use crate::i18n::{tr, trf, trn_with};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cadenza_core::{ClipId, DeviceId, ParamAddress, ParamTarget, Tick, TrackId};
use cadenza_dsp::DeviceRole;
use cadenza_engine::{DeviceProbe, MeterBank, PlayerClock};
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

pub const DEFAULT_TRACK_HEIGHT: f32 = 66.0;
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
	/// The device editor's device and the sampler / drum sampler selection (shares the
	/// bottom editor dock).
	pub instrument: samples::InstrumentView,
	/// The editor dock shows the device editor (over the piano roll / audio editor).
	pub device_editor: bool,
	/// Floating windows (device editors, track / clip properties) over the workspace.
	pub windows: windows::Windows,
	/// Pattern / Song mode and the arrange loop region.
	pub transport: transport::TransportView,
	/// The start screen (recent projects, new, open, demo) is shown.
	pub start_screen: bool,
	/// Tracks whose take lanes are expanded.
	pub takes: Vec<TrackId>,
	/// Collapsed folder tracks (their contents hidden in the arrange and the mixer).
	pub collapsed: std::collections::HashSet<TrackId>,
	/// Mixer strip width and visible sections.
	pub mixer: mixer::MixerView,
	/// The inspector's active tab (Track | Clip | Device).
	pub inspector: inspector::InspectorView,
	/// The arrange pointer tool (Select / Draw / Paint / Split / Mute).
	pub arrange_tool: arrange_tool::ArrangeTool,
	/// Track heights set by dragging a header's bottom edge (others use `track_height`).
	pub track_heights: HashMap<TrackId, f32>,
	/// Clips copied with Ctrl+C / Ctrl+X in the arrange.
	pub clip_board: playlist::ClipClipboard,
	/// The clip the Paint tool repeats when none is pressed or selected on the lane.
	pub paint_source: Option<ClipId>,
	/// The shell for the current window size (docked / overlay panels, transport and
	/// status bar folding), set by the view every frame.
	pub responsive: layout::Responsive,
}

impl ViewState {
	/// Height of `track`'s clip row.
	pub fn height_of(&self, track: TrackId) -> f32 {
		self.track_heights
			.get(&track)
			.copied()
			.unwrap_or(self.track_height)
	}

	/// Sets one track's height (clamped); the default height clears the override.
	pub fn set_track_height(&mut self, track: TrackId, height: f32) {
		let h = height.clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
		if (h - self.track_height).abs() < 0.5 {
			self.track_heights.remove(&track);
		} else {
			self.track_heights.insert(track, h);
		}
	}

	/// Alt+wheel: every track grows or shrinks together (resized ones keep their ratio).
	pub fn scale_track_heights(&mut self, height: f32) {
		let new = height.clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
		let ratio = new / self.track_height.max(1.0);
		self.track_height = new;
		for h in self.track_heights.values_mut() {
			*h = (*h * ratio).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
		}
	}
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
			device_editor: false,
			windows: windows::Windows::default(),
			transport: transport::TransportView::default(),
			start_screen: false,
			takes: Vec::new(),
			collapsed: std::collections::HashSet::new(),
			mixer: mixer::MixerView::default(),
			inspector: inspector::InspectorView::default(),
			arrange_tool: arrange_tool::ArrangeTool::default(),
			track_heights: HashMap::new(),
			clip_board: playlist::ClipClipboard::default(),
			paint_source: None,
			responsive: layout::Responsive::default(),
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
	/// A freeze / bounce / consolidation render: the job and its length (seconds).
	pub bounce: Option<(bounce::BounceJob, Job<Result<f64, String>>)>,
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
	/// Frozen tracks edited since they were frozen (computed once per change).
	stale_freezes: Vec<TrackId>,
}

impl DocView {
	fn of(document: &Document) -> Self {
		let project = document.project();
		Self {
			stale_freezes: project
				.tracks
				.iter()
				.filter(|t| t.freeze.is_some() && project.freeze_is_stale(t.id))
				.map(|t| t.id)
				.collect(),
			project: project.clone(),
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

	/// A frozen track whose clips or devices changed after it was frozen.
	pub fn freeze_stale(&self, track: TrackId) -> bool {
		self.stale_freezes.contains(&track)
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
	/// Audio device and plugin dropouts (transport indicator).
	pub xruns: xruns::Xruns,
	/// Live loudness of the master strip (from the player's master tap).
	pub master_loudness: mixer::MasterLoudness,
	/// Mono / dim on the output: monitoring only, not in the project (see
	/// [`AppState::set_monitor_fx`]).
	monitor_fx: cadenza_engine::MonitorFx,
	/// The open editor's device playing a listen mode (compressor key, EQ band solo):
	/// monitoring only, cleared when the editor closes (see [`AppState::set_listen`]).
	listen: Option<(DeviceId, cadenza_engine::Listen)>,
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
	/// The mix assistant (the AI panel's Mix tab).
	pub mix: mix::MixAssistant,
	/// Match EQ of the EQ editors.
	match_eq: match_eq::MatchEqState,
	collisions: collisions::CollisionState,
	/// MIDI learn and the mapped controllers' values (T36).
	pub midi: midi_learn::MidiLearnState,
	/// Recent project summaries and the start screen's inputs.
	pub start: start::StartScreen,
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
	/// The device whose live signal the engine probes, and the probe once the engine
	/// answered (see [`AppState::set_probe`]).
	probe_target: Option<DeviceId>,
	probe: Option<Arc<DeviceProbe>>,
	/// The device the open device editor pointed the probe at (see
	/// [`AppState::sync_editor_probe`]).
	editor_probe: Option<DeviceId>,
	/// Presets of the device editor's device (factory + user), listed when it opens.
	device_presets: Option<(DeviceRef, Vec<device::PresetEntry>)>,
	/// The preset last loaded into (or saved from) each device, for the dirty dot.
	loaded_presets: HashMap<DeviceId, device::LoadedPreset>,
	/// A/B compare states of the devices opened in the editor (view state).
	compares: HashMap<DeviceId, device_ab::Compare>,
	/// The devices each undo / redo step changed (the editor's undo hint).
	touches: device_ab::StepTouches,
	/// An A/B switch is being applied (its own step is not an undo / redo to follow).
	switching: bool,
	/// Built-in params when the running AI request started, and the request.
	ai_snapshot: Option<(ai_changes::Snapshot, String)>,
	/// What the last AI request changed on built-in devices (rings, the editor's card).
	pub ai_changes: Option<ai_changes::AiChanges>,
	/// The device whose AI changes are heard reverted ("Hold to hear before").
	hearing_before: Option<DeviceId>,
	/// Playhead when there is no audio engine, or while stopped before it is ready.
	position: Tick,
	reload_since: Option<Instant>,
	generation: u64,
	loaded_generation: u64,
	waker: Option<Waker>,
	last_tick: Option<Instant>,
	/// Lane points last sent to the player by a running gesture.
	lane_preview: Option<(ParamAddress, Vec<cadenza_model::AutomationPoint>)>,
	/// Automation recording (transport Read / Touch / Latch).
	pub autowrite: autowrite::Writer,
	/// Lanes whose playback is suspended while they are written (last sent to the player).
	write_suspended: Vec<ParamAddress>,
	/// Play was pressed (and not stopped since): pattern mode auditions only then.
	play_intent: bool,
	/// The user placed the playhead since the last stop: pattern mode starts there
	/// (inside the pattern) instead of at the pattern start.
	cued: bool,
	/// Loop and audition last sent to the player.
	play_config: Option<transport::PlayConfig>,
	/// The style library (the session's, fetched once).
	library: Option<Arc<cadenza_library::Library>>,
	/// The browser's Library tab.
	pub library_view: library::LibraryView,
	/// A library pattern sounding through the preview path.
	library_preview: Option<library::PatternPreview>,
	/// The UI language preference (View > Language).
	pub language: crate::i18n::LanguageChoice,
	/// The app sets the process-wide language; headless states (tests) only this
	/// thread's, so parallel tests keep their own.
	language_global: bool,
}

impl AppState {
	/// State with a live audio engine (the app).
	pub fn new(waker: Option<Waker>) -> Self {
		let mut s = Self::headless_with(waker.clone());
		s.ai = ai::AiChat::new(waker.clone());
		s.language_global = true;
		s.set_language(crate::i18n::LanguageChoice::System);
		s.audio = Some(AudioEngine::start(waker));
		s.audio_status = AudioStatus::Starting;
		s.report_library();
		s
	}

	/// State without audio (tests, screenshots in CI). The style library is the embedded
	/// `core` pack only, so tests never read the user's packs.
	pub fn headless() -> Self {
		let s = Self::headless_with(None);
		s.session()
			.set_library(Arc::new(cadenza_library::Library::core()));
		s
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
			xruns: xruns::Xruns::default(),
			master_loudness: mixer::MasterLoudness::default(),
			monitor_fx: cadenza_engine::MonitorFx::default(),
			listen: None,
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
			mix: mix::MixAssistant::default(),
			match_eq: match_eq::MatchEqState::default(),
			collisions: collisions::CollisionState::default(),
			midi: midi_learn::MidiLearnState::default(),
			start: start::StartScreen::default(),
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
			probe_target: None,
			probe: None,
			editor_probe: None,
			device_presets: None,
			loaded_presets: HashMap::new(),
			compares: HashMap::new(),
			touches: device_ab::StepTouches::default(),
			ai_snapshot: None,
			ai_changes: None,
			hearing_before: None,
			switching: false,
			position: Tick::ZERO,
			reload_since: Some(Instant::now()),
			generation: 0,
			loaded_generation: 0,
			waker,
			last_tick: None,
			lane_preview: None,
			autowrite: autowrite::Writer::default(),
			write_suspended: Vec::new(),
			play_intent: false,
			cued: false,
			play_config: None,
			library: None,
			library_view: library::LibraryView::default(),
			library_preview: None,
			language: crate::i18n::LanguageChoice::System,
			language_global: false,
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
		self.follow_history(&before, epoch_changed);
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
			.unwrap_or_else(|| tr("not saved").into());
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
			self.notices.info(tr(
				"The AI is editing the project — wait for it or press Stop in the chat (F10)",
			));
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
					cadenza_model::ClipContent::Midi { notes, .. } => {
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
					.then(|| tr("the master channel").to_owned()),
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
			reference_file: None,
			open_device: self
				.device_editor_target()
				.and_then(|r| self.describe_device(r)),
			reply_language: self
				.ai
				.settings
				.reply_language
				.resolve_name(self.language.resolve()),
		}
	}

	/// Sends `request` to the AI producer (with the app context).
	pub fn send_ai(&mut self, request: &str) -> bool {
		self.send_ai_as(request, request.trim())
	}

	/// [`AppState::send_ai`] remembering `shown` as the request on the AI change card.
	fn send_ai_as(&mut self, request: &str, shown: &str) -> bool {
		let context = self.ai_context();
		let session = self.session.clone();
		if self.ai.busy() {
			return false;
		}
		self.begin_ai_changes(shown);
		let sent = self.ai.send(request, context, &session);
		if !sent {
			self.ai_snapshot = None;
		}
		sent
	}

	/// The device editor's "Ask AI": `text` about the device `r`, named in the request
	/// (the context carries its id as the open device). Opens the chat so the reply shows.
	pub fn ask_ai_about_device(&mut self, r: DeviceRef, text: &str) -> bool {
		let text = text.trim();
		let Some(d) = self.device(r) else {
			return false;
		};
		if text.is_empty() {
			return false;
		}
		let on = match r.track.and_then(|t| self.project().track(t)) {
			Some(t) => format!("'{}'", t.name),
			None => tr("the master").to_owned(),
		};
		let request = format!("{text} ({} on {on})", d.name);
		let sent = self.send_ai_as(&request, text);
		if sent {
			self.view.panels.set(PanelKind::Chat, true);
			self.mix.open = false;
		}
		sent
	}

	/// A device as the AI reads it: "\"EQ\" (device 4c1d..., Parametric EQ, eq) on track
	/// \"Vox\" (track 9a0b...)".
	fn describe_device(&self, r: DeviceRef) -> Option<String> {
		let p = self.project();
		let d = self.device(r)?;
		let kind = d
			.builtin_id()
			.and_then(cadenza_dsp::device_info)
			.map_or_else(|| "plugin".to_owned(), |i| format!("{}, {}", i.name, i.id));
		let on = match r.track.and_then(|t| p.track(t)) {
			Some(t) => format!("track \"{}\" (track {})", t.name, t.id),
			None => tr("the master channel").to_owned(),
		};
		Some(format!("\"{}\" (device {}, {kind}) on {on}", d.name, d.id))
	}

	/// "Explain this device": a read-only request about the device `r` (its current
	/// settings in plain language); opens the chat so the answer streams in.
	pub fn explain_device(&mut self, r: DeviceRef) -> bool {
		let Some(d) = self.device(r) else {
			return false;
		};
		if self.ai.busy() {
			return false;
		}
		let name = d.name.clone();
		let mut context = self.ai_context();
		let described = self.describe_device(r).unwrap_or_else(|| name.clone());
		context.open_device = Some(described.clone());
		let text = cadenza_agent::prompt::explain_request(&described);
		let label = trf("Explain {device}", &[("device", &name)]);
		let session = self.session.clone();
		let sent = self.ai.send_explain(&label, text, context, &session);
		if sent {
			self.view.panels.set(PanelKind::Chat, true);
			self.mix.open = false;
		}
		sent
	}

	// --- AI changes on devices (design 15e) -------------------------------------------

	/// A request starts: the built-ins' params are remembered and the last changes go.
	fn begin_ai_changes(&mut self, request: &str) {
		self.hear_before(None);
		self.ai_changes = None;
		self.ai_snapshot = Some((ai_changes::snapshot(self.project()), request.to_owned()));
	}

	fn finish_ai_changes(&mut self, finished: &ai::worker::Finished) {
		let Some((before, request)) = self.ai_snapshot.take() else {
			return;
		};
		if !finished.edited {
			return;
		}
		let undo = &self.doc.undo;
		if undo.last() != Some(&finished.undo_label) {
			return;
		}
		self.ai_changes = ai_changes::AiChanges::new(
			&finished.undo_label,
			&request,
			undo.len(),
			&before,
			&self.doc.project,
		);
	}

	/// The AI change of `param` on `device`, if the last request moved it.
	pub fn ai_change(&self, device: DeviceId, param: &str) -> Option<&ai_changes::Change> {
		self.ai_changes.as_ref()?.change(device, param)
	}

	/// Every AI change on `device`.
	pub fn ai_changes_of(&self, device: DeviceId) -> &[ai_changes::Change] {
		self.ai_changes.as_ref().map_or(&[], |c| c.of(device))
	}

	/// Whether Undo would take back the AI step (it is still the last step).
	pub fn ai_step_is_last(&self) -> bool {
		self.ai_changes
			.as_ref()
			.is_some_and(|c| c.is_last_step(&self.doc.undo))
	}

	/// "Keep": the changes stay, their rings go.
	pub fn keep_ai_changes(&mut self) {
		self.hear_before(None);
		self.ai_changes = None;
	}

	/// "Undo" on the card: undoes the AI step while it is the last one.
	pub fn undo_ai_changes(&mut self) -> bool {
		if !self.ai_step_is_last() {
			return false;
		}
		self.hear_before(None);
		self.undo();
		self.ai_changes.is_none()
	}

	/// The user touched `param`: that AI change is kept and loses its ring.
	fn touch_ai_change(&mut self, device: DeviceId, param: &str) {
		if let Some(c) = &mut self.ai_changes {
			c.touch(device, param);
			if c.is_empty() {
				self.ai_changes = None;
			}
		}
	}

	/// "Hold to hear before": `Some(device)` plays that device's params from before the AI
	/// request through the live param path (no command), `None` puts the current values
	/// back. The document never changes.
	pub fn hear_before(&mut self, device: Option<DeviceId>) {
		if self.hearing_before == device {
			return;
		}
		if let Some(old) = self.hearing_before.take() {
			self.send_ai_values(old, false);
		}
		if let Some(new) = device
			&& !self.ai_changes_of(new).is_empty()
		{
			self.send_ai_values(new, true);
			self.hearing_before = Some(new);
		}
	}

	/// The device being heard as it was before the AI request.
	pub fn hearing_before(&self) -> Option<DeviceId> {
		self.hearing_before
	}

	fn send_ai_values(&self, device: DeviceId, before: bool) {
		let Some(a) = &self.audio else { return };
		let track = self.device_track(device).flatten();
		for c in self.ai_changes_of(device) {
			let value = if before {
				c.before
			} else {
				self.param_value(track, device, &c.param).unwrap_or(c.after)
			};
			a.live_param(device, &c.param, value);
		}
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
			self.finish_ai_changes(&finished);
			if finished.undo_label == cadenza_agent::ENGINEER_UNDO_LABEL {
				self.mix_pass_finished(&finished);
			}
			if finished.edited {
				self.notices.info(trf(
					"{undo_label} — one undo step (Ctrl+Z)",
					&[("undo_label", &finished.undo_label)],
				));
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
			self.view.device_editor = false;
		}
		self.view.windows.prune(&self.doc.project);
		let devices = device_ab::devices(&self.doc.project);
		self.compares.retain(|id, _| devices.contains_key(id));
		self.sync_editor_probe();
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
		if changes.midi_map {
			a.midi_map(self.project().clone());
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
				.info(tr("Select a track to show its automation lanes"));
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
				self.notices
					.info(trf("Undo: {label}", &[("label", &label)]));
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
				self.notices
					.info(trf("Redo: {label}", &[("label", &label)]));
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
		self.write_value(
			ParamAddress {
				track,
				target: ParamTarget::Volume,
			},
			db,
		);
	}

	pub fn preview_pan(&mut self, track: Option<TrackId>, pan: f32) {
		self.previews.pan.insert(track, pan);
		if let Some(a) = &self.audio {
			a.live_pan(track, pan);
		}
		self.write_value(
			ParamAddress {
				track,
				target: ParamTarget::Pan,
			},
			pan,
		);
	}

	// --- Freeze / bounce / consolidate ---------------------------------------------

	/// Where track renders go: `.cadenza/freeze/` (freeze) or `Audio/` of the project, or
	/// a state folder before the first save.
	fn bounce_dir(&self, kind: bounce::BounceKind) -> PathBuf {
		match (self.path(), kind) {
			(Some(p), bounce::BounceKind::Freeze) => cadenza_model::ProjectLayout::new(p)
				.cache_dir()
				.join("freeze"),
			(Some(p), _) => cadenza_model::ProjectLayout::new(p).audio_dir(),
			(None, _) => self
				.safety
				.state_dir
				.join("renders")
				.join(format!("untitled-{}", std::process::id())),
		}
	}

	/// Renders a track (or clips) on a worker; the result lands as one undo step.
	fn start_bounce(
		&mut self,
		kind: bounce::BounceKind,
		track: TrackId,
		clips: Vec<ClipId>,
		start: Tick,
	) {
		if self.jobs.bounce.is_some() {
			self.notices.warn(tr("A track render is already running"));
			return;
		}
		let Some(t) = self.project().track(track) else {
			return;
		};
		let stem = match kind {
			bounce::BounceKind::Freeze => format!("{} freeze", t.name),
			bounce::BounceKind::InPlace => format!("{} bounce", t.name),
			bounce::BounceKind::Consolidate => format!("{} consolidated", t.name),
		};
		let file = bounce::render_path(&self.bounce_dir(kind), &stem);
		let job = bounce::BounceJob {
			kind,
			track,
			clips: clips.clone(),
			start,
			file: file.clone(),
			new_track: TrackId::new(),
			new_clip: ClipId::new(),
			source: self.project().freeze_source(track),
		};
		let mut project = self.project().clone();
		// Render what the track computes, not a previous freeze.
		if let Some(t) = project.tracks.iter_mut().find(|t| t.id == track) {
			t.freeze = None;
		}
		let (plugins, hosting) = {
			let mut session = self.session();
			(
				session.plugins_for_project(),
				session.plugin_hosting().clone(),
			)
		};
		let opts = cadenza_engine::RenderOptions {
			sample_rate: match self.audio_status {
				AudioStatus::Ready { sample_rate } => sample_rate,
				_ => cadenza_engine::RenderOptions::default().sample_rate,
			},
			plugins,
			hosting,
			..cadenza_engine::RenderOptions::default()
		};
		let scope = match kind {
			bounce::BounceKind::Consolidate => cadenza_engine::BounceScope::Clips(clips),
			_ => cadenza_engine::BounceScope::Track,
		};
		self.notices.info(match kind {
			bounce::BounceKind::Freeze => tr("Freezing…"),
			bounce::BounceKind::InPlace => tr("Bouncing…"),
			bounce::BounceKind::Consolidate => tr("Consolidating…"),
		});
		let work = Job::spawn("bounce", self.waker.clone(), move || {
			cadenza_engine::render_track(&project, &opts, track, &scope, start, &file)
				.map(|r| r.duration_seconds)
				.map_err(|e| e.to_string())
		});
		self.jobs.bounce = Some((job, work));
	}

	/// Freezes a track: renders it (devices, clips, inserts) and plays the render instead,
	/// unloading its devices. Unfreeze with [`AppState::unfreeze_track`].
	pub fn freeze_track(&mut self, track: TrackId) {
		if self.project().track(track).is_some_and(bounce::can_freeze) {
			self.start_bounce(bounce::BounceKind::Freeze, track, Vec::new(), Tick::ZERO);
		}
	}

	pub fn unfreeze_track(&mut self, track: TrackId) {
		if self
			.project()
			.track(track)
			.is_some_and(|t| t.freeze.is_some())
		{
			self.apply(Command::SetTrackFreeze {
				track,
				freeze: None,
			});
		}
	}

	/// Renders a track to a new audio track below it (from the bar of its first clip) and
	/// mutes the original: one undo step when done.
	pub fn bounce_in_place(&mut self, track: TrackId) {
		let Some(t) = self.project().track(track) else {
			return;
		};
		if !bounce::can_freeze(t) {
			return;
		}
		let first = t.clips.iter().filter(|c| c.plays()).map(|c| c.start).min();
		let Some(first) = first else {
			self.notices.info(tr("The track has no clips to bounce"));
			return;
		};
		let start = self.project().meter().bar_floor(first);
		self.start_bounce(bounce::BounceKind::InPlace, track, Vec::new(), start);
	}

	/// Merges the selected clips of one track into one clip (MIDI at once, audio
	/// rendered): one undo step.
	pub fn consolidate_selection(&mut self) {
		let clips: Vec<ClipId> = self.selection.clips.iter().copied().collect();
		let (track, start, _) = match bounce::consolidation(self.project(), &clips) {
			Ok(v) => v,
			Err(e) => {
				self.notices.info(e);
				return;
			}
		};
		let midi = clips.iter().all(|c| {
			self.project()
				.find_clip(*c)
				.is_some_and(|(_, c)| c.is_midi())
		});
		if midi {
			let new = ClipId::new();
			if let Some(cmd) = bounce::consolidate_midi(self.project(), &clips, new)
				&& self.apply(cmd)
			{
				self.selection.click_clip(track, new, false);
			}
			return;
		}
		self.start_bounce(bounce::BounceKind::Consolidate, track, clips, start);
	}

	fn poll_bounce(&mut self) {
		let Some(r) = self.jobs.bounce.as_ref().and_then(|(_, j)| j.poll()) else {
			return;
		};
		let (job, _) = self.jobs.bounce.take().expect("bounce job");
		match r {
			Some(Ok(seconds)) => match bounce::finish(self.project(), &job, seconds) {
				Ok(cmd) => {
					if self.apply_or_queue(cmd) {
						self.notices.info(match job.kind {
							bounce::BounceKind::Freeze => {
								tr("Frozen: the track plays its render (Unfreeze to edit)")
									.to_owned()
							}
							bounce::BounceKind::InPlace => trf(
								"Bounced to a new track: {job}",
								&[("job", &job.file.display())],
							),
							bounce::BounceKind::Consolidate => tr("Clips consolidated").to_owned(),
						});
					}
				}
				Err(e) => self.notices.warn(e),
			},
			Some(Err(e)) => self
				.notices
				.error(trf("Track render failed: {e}", &[("e", &e)])),
			None => self
				.notices
				.error(tr("Track render failed: the worker thread stopped")),
		}
	}

	/// A track render is running.
	pub fn bounce_busy(&self) -> bool {
		self.jobs.bounce.is_some()
	}

	// --- Folders --------------------------------------------------------------------

	/// Collapses / expands a folder track.
	pub fn toggle_folder(&mut self, folder: TrackId) {
		if !self.view.collapsed.remove(&folder) {
			self.view.collapsed.insert(folder);
		}
	}

	/// Moves a track (a folder with its contents) to display index `to`, filing it into
	/// the folder it lands in (one undo step).
	pub fn move_track_to(&mut self, track: TrackId, to: usize) {
		if let Some(cmd) = folders::move_track(self.project(), &self.view.collapsed, track, to) {
			self.apply(cmd);
		}
	}

	/// Puts `tracks` into a new folder named "Folder N" (one undo step) and selects it.
	pub fn group_into_folder(&mut self, tracks: &[TrackId]) {
		let folder = edits::new_track(self.project(), TrackKind::Folder);
		let id = folder.id;
		if let Some(cmd) = folders::group_into_folder(self.project(), tracks, folder)
			&& self.apply(cmd)
		{
			self.selection.select_track(id);
		}
	}

	/// Takes a track one level out of its folder.
	pub fn take_out_of_folder(&mut self, track: TrackId) {
		if let Some(cmd) = folders::take_out(self.project(), track) {
			self.apply(cmd);
		}
	}

	// --- Automation write ----------------------------------------------------------

	/// Sets the transport's automation mode; leaving a write mode ends the pass (one undo
	/// step for what was written).
	pub fn set_write_mode(&mut self, mode: autowrite::WriteMode) {
		if self.autowrite.mode == mode {
			return;
		}
		self.autowrite.mode = mode;
		if mode == autowrite::WriteMode::Off {
			self.finish_write_pass();
		}
	}

	/// A control moved: while playing in a write mode the value is written into its lane.
	/// Returns whether it was.
	fn write_value(&mut self, address: ParamAddress, value: f32) -> bool {
		if !self.autowrite.armed(self.is_playing()) {
			return false;
		}
		let at = self.position();
		self.autowrite.touch(&address, value, at);
		self.sync_write_suspension();
		true
	}

	/// A control was let go: true when it was being written (nothing else to commit).
	fn release_write(&mut self, address: &ParamAddress) -> bool {
		if !self.autowrite.owns(address) {
			return false;
		}
		let at = self.position();
		self.autowrite.release(address, at);
		self.sync_write_suspension();
		true
	}

	/// Plays the project's lanes minus the ones being written.
	fn sync_write_suspension(&mut self) {
		let suspended = self.autowrite.suspended();
		if suspended == self.write_suspended {
			return;
		}
		self.write_suspended = suspended;
		if let Some(a) = &self.audio {
			let mut lanes = self.project().automation.clone();
			lanes.retain(|l| !self.write_suspended.contains(&l.address));
			a.automation(lanes, self.project().tempo.clone());
		}
	}

	/// Each frame: latched controls follow the playhead; when playback stops the pass is
	/// committed.
	pub(crate) fn tick_autowrite(&mut self, playing: bool, position: Tick) {
		if playing {
			if self.autowrite.has_pass() {
				self.autowrite.advance(position);
			}
		} else if self.autowrite.has_pass() {
			self.finish_write_pass();
		}
	}

	fn finish_write_pass(&mut self) {
		let cmd = self.autowrite.finish(&self.doc.project);
		self.write_suspended.clear();
		let applied = cmd.is_some_and(|c| self.apply_or_queue(c));
		if !applied && let Some(a) = &self.audio {
			let p = self.project();
			a.automation(p.automation.clone(), p.tempo.clone());
		}
	}

	/// Ends a fader gesture with one undoable command. Like every mix/parameter edit it
	/// reaches the player live (see [`live`]): no recompilation cutting sounding notes.
	pub fn commit_volume(&mut self, track: Option<TrackId>, db: f32) {
		self.previews.volume.remove(&track);
		if self.release_write(&ParamAddress {
			track,
			target: ParamTarget::Volume,
		}) {
			return;
		}
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
		if self.release_write(&ParamAddress {
			track,
			target: ParamTarget::Pan,
		}) {
			return;
		}
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
		let track = self
			.project()
			.tracks
			.iter()
			.find(|t| t.channel.sends.iter().any(|s| s.id == send))
			.map(|t| t.id);
		if let Some(track) = track {
			self.write_value(ParamAddress::send(track, send), db);
		}
	}

	pub fn commit_send(&mut self, track: TrackId, send: DeviceId, db: f32) {
		self.previews.sends.remove(&send);
		let address = ParamAddress::send(track, send);
		if self.release_write(&address) {
			return;
		}
		self.set_param(address, db);
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
		if let Some(track) = self.device_track(device) {
			self.write_value(ParamAddress::device(track, device, param), value);
		}
	}

	/// Shows a value a mapped MIDI controller set (the player already plays it): like a
	/// gesture's preview, so automation Touch/Latch writes it.
	pub(crate) fn mirror_value(&mut self, address: &ParamAddress, value: f32) {
		match &address.target {
			ParamTarget::Volume => {
				self.previews.volume.insert(address.track, value);
			}
			ParamTarget::Pan => {
				self.previews.pan.insert(address.track, value);
			}
			ParamTarget::Send { send } => {
				self.previews.sends.insert(*send, value);
			}
			ParamTarget::Device { device, param } => {
				self.previews.params.insert((*device, param.clone()), value);
			}
		}
		self.write_value(address.clone(), value);
	}

	/// Ends a gesture on any address with one command (see the `commit_*` methods).
	pub(crate) fn commit_address(&mut self, address: &ParamAddress, value: f32) {
		match &address.target {
			ParamTarget::Volume => self.commit_volume(address.track, value),
			ParamTarget::Pan => self.commit_pan(address.track, value),
			ParamTarget::Send { send } => match address.track {
				Some(track) => self.commit_send(track, *send, value),
				None => {
					self.previews.sends.remove(send);
				}
			},
			ParamTarget::Device { device, param } => {
				self.commit_param(address.track, *device, param, value);
			}
		}
	}

	/// The channel holding `device` (`Some(None)` = the master).
	fn device_track(&self, device: DeviceId) -> Option<Option<TrackId>> {
		let p = self.project();
		let on = |c: &cadenza_model::Channel| c.inserts.iter().any(|d| d.id == device);
		if on(&p.master) {
			return Some(None);
		}
		p.tracks
			.iter()
			.find(|t| on(&t.channel) || t.instrument.as_ref().is_some_and(|d| d.id == device))
			.map(|t| Some(t.id))
	}

	pub fn commit_param(
		&mut self,
		track: Option<TrackId>,
		device: DeviceId,
		param: &str,
		value: f32,
	) {
		self.previews.params.remove(&(device, param.to_owned()));
		self.touch_ai_change(device, param);
		let address = ParamAddress::device(track, device, param);
		if self.release_write(&address) {
			return;
		}
		self.set_param(address, value);
	}

	/// Forgets a parameter preview without committing it (a gesture that ended where it
	/// started).
	pub fn drop_param_preview(&mut self, device: DeviceId, param: &str) {
		self.previews.params.remove(&(device, param.to_owned()));
	}

	/// Several device parameters let go together (a node dragged in freq and gain): ONE
	/// undo step named `label` holding the values that changed. Returns whether it applied.
	pub fn commit_params(
		&mut self,
		track: Option<TrackId>,
		device: DeviceId,
		values: &[(&str, f32)],
		label: &str,
	) -> bool {
		let mut commands = Vec::new();
		for &(param, value) in values {
			self.previews.params.remove(&(device, param.to_owned()));
			self.touch_ai_change(device, param);
			let address = ParamAddress::device(track, device, param);
			if self.release_write(&address) {
				continue;
			}
			let current = self
				.project()
				.device(track, device)
				.and_then(|d| d.params.get(param).copied());
			if current != Some(value) {
				commands.push(Command::SetParam { address, value });
			}
		}
		if commands.is_empty() {
			return false;
		}
		self.apply(Command::Batch {
			label: label.to_owned(),
			commands,
		})
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
		self.notices.info(trf(
			"{name} added — press F7 or double-click the track to open the piano roll",
			&[("name", &name)],
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
			self.notices.info(trf(
				"{name} is a built-in device: edit it in the inspector (F8)",
				&[("name", &d.name)],
			));
			return;
		}
		if d.bypass {
			self.notices.warn(trf(
				"{name} is bypassed: enable it to open its editor",
				&[("name", &d.name)],
			));
			return;
		}
		if !cadenza_engine::EDITORS_SUPPORTED {
			self.notices.warn(tr(
				"Plugin editor windows are not supported on this platform yet; use the inspector",
			));
			return;
		}
		let Some(title) = editors::title(project, device) else {
			return;
		};
		match &self.audio_status {
			AudioStatus::Unavailable(e) => {
				self.notices.error(trf(
					"Plugin editors need the audio engine, which is unavailable: {e}",
					&[("e", &e)],
				));
				return;
			}
			AudioStatus::Off => {
				self.notices
					.error(tr("Plugin editors need the audio engine (not running)"));
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
		// Automation write: editor knob moves during playback go into their lanes
		// (gesture begin/value/end = touch, write, release).
		if self.autowrite.armed(self.is_playing()) {
			for u in &updates {
				let Some(track) = self.device_track(u.device) else {
					continue;
				};
				match &u.event {
					cadenza_engine::EditorEvent::Value { param, value } => {
						let address = ParamAddress::device(track, u.device, param.clone());
						self.write_value(address, *value as f32);
					}
					cadenza_engine::EditorEvent::GestureEnd { param } => {
						let address = ParamAddress::device(track, u.device, param.clone());
						self.release_write(&address);
					}
					_ => {}
				}
			}
		}
		let actions = self.editors.handle(&self.doc.project, updates, now);
		self.editor_actions(actions);
	}

	/// Applies commands produced by plugin editors and asks for state captures. Values of
	/// parameters being written into automation are not committed as static values.
	fn editor_actions(&mut self, mut actions: editors::Actions) {
		if self.autowrite.has_pass() {
			let w = &self.autowrite;
			let keep =
				|c: &Command| !matches!(c, Command::SetParam { address, .. } if w.owns(address));
			actions.commands.retain(&keep);
			for c in &mut actions.commands {
				if let Command::Batch { commands, .. } = c {
					commands.retain(&keep);
				}
			}
			actions
				.commands
				.retain(|c| !matches!(c, Command::Batch { commands, .. } if commands.is_empty()));
		}
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
		// Record mode (R): Play records the armed tracks (nothing armed: plays).
		if self.record.mode && !self.is_recording() && self.record_plan().is_ok() {
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
			self.notices.info(tr(
				"Pattern mode plays the clip open in the piano roll — open one with F7. \
				 Playing the song instead.",
			));
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
				.warn(tr("No audio output: playback is unavailable")),
		}
	}

	/// Stops; pattern mode goes back to the pattern start (FL Studio).
	pub fn stop(&mut self) {
		self.end_recording(false);
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
			let bar = self
				.project()
				.meter()
				.bar_at(Tick(self.position().0.max(0)));
			self.view.transport.loop_range = Some((bar.start, bar.end()));
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
		let bar = self.project().meter().bar_start(1);
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
				self.notices
					.info(trf("Saved {path}", &[("path", &out.path)]));
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
			self.notices.info(trf(
				"Imported {path} ({seconds} s)",
				&[
					("path", &path.display()),
					(
						"seconds",
						&crate::i18n::decimal(format!("{:.1}", out.file.duration_seconds)),
					),
				],
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
				self.notices.info(trn_with(
					"Imported {path} ({n} track)",
					"Imported {path} ({n} tracks)",
					out.created_tracks.len().max(1),
					&[("path", &path.display())],
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
				self.notices.error(trf(
					"Cannot import {path}: {e}",
					&[("path", &path.display()), ("e", &e)],
				));
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
			self.notices.info(trn_with(
				"Imported {path} ({n} track)",
				"Imported {path} ({n} tracks)",
				tracks,
				&[("path", &path.display())],
			));
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
			DropAction::MapKit { device, folder } => self.map_kit_folder(device, &folder),
			DropAction::NewSampler { path } => self.new_sampler_track(&path),
			DropAction::AddPattern {
				pattern, track, at, ..
			} => self.add_library_pattern(&pattern, track, at),
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
		self.view.device_editor = false;
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
			let msg = trf(
				"'{name}' is not an instrument track: the piano roll edits MIDI clips",
				&[("name", &t.name)],
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

	/// F7: hides a visible editor dock (unless an instrument track other than the one
	/// being edited is selected: that track's clip is shown instead); otherwise opens the piano roll for the selected
	/// instrument track (see [`AppState::open_track_in_piano_roll`]), or the selected clip
	/// of another kind in its editor, or just shows the dock.
	pub fn toggle_piano_roll(&mut self) {
		let track = self
			.selection
			.track
			.filter(|t| self.project().track(*t).map(|t| t.kind) == Some(TrackKind::Instrument));
		if self.view.panels.visible(PanelKind::PianoRoll) {
			// Another instrument track was selected since: show its clip instead of closing.
			let editing = (!self.view.audio_editor && !self.view.device_editor)
				.then_some(self.view.piano.clip)
				.flatten()
				.and_then(|c| self.project().find_clip(c).map(|(t, _)| t.id));
			match track {
				Some(t) if editing != Some(t) => {
					self.open_track_in_piano_roll(t);
				}
				_ => self.close_piano_roll(),
			}
			return;
		}
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
		self.view.device_editor = false;
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
			self.notices
				.warn(crate::i18n::tr_str(audio_edit::STRETCH_HINT));
			return;
		}
		if self.jobs.process.is_some() {
			self.notices
				.warn(tr("An audio operation is already running"));
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
			self.notices.info(tr("Select a range of the clip first"));
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

	// --- Device editors ---------------------------------------------------------------

	/// Whether `device` is a built-in sampler or drum sampler (pads / zones body).
	pub fn is_sample_device(&self, r: DeviceRef) -> bool {
		self.device(r).and_then(samples::kind).is_some()
	}

	/// Whether `r` is a built-in device (it opens in the device editor; plugins have
	/// their own windows).
	pub fn has_device_editor(&self, r: DeviceRef) -> bool {
		self.device(r)
			.and_then(|d| d.builtin_id())
			.is_some_and(|id| cadenza_dsp::device_info(id).is_some())
	}

	/// Shows built-in device `r` in its floating window (opened, or brought to the front
	/// when it is already open); a device shown in the bottom dock stays there.
	pub fn open_device_editor(&mut self, r: DeviceRef) -> bool {
		if !self.has_device_editor(r) {
			return false;
		}
		if !self.docked_device_shown(r) {
			self.open_window(windows::WindowItem::Device(r));
		}
		self.selection.select_device(r);
		self.sync_editor_probe();
		true
	}

	/// Shows built-in device `r` in the bottom editor dock (one device at a time; its
	/// floating window closes).
	pub fn open_device_in_dock(&mut self, r: DeviceRef) -> bool {
		if !self.has_device_editor(r) {
			return false;
		}
		self.release_previews();
		if self.view.instrument.device != Some(r) {
			self.view.instrument.zone = 0;
		}
		self.view.windows.close(windows::WindowItem::Device(r));
		self.view.instrument.device = Some(r);
		self.view.device_editor = true;
		self.view.panels.open_piano_roll();
		self.selection.select_device(r);
		self.sync_editor_probe();
		true
	}

	/// Whether `r` is the device of the visible editor dock.
	fn docked_device_shown(&self, r: DeviceRef) -> bool {
		self.view.instrument.device == Some(r)
			&& self.view.device_editor
			&& self.view.panels.visible(PanelKind::PianoRoll)
	}

	/// Toggles the editor of `r` (double-clicks, the inspector's Edit): a focused
	/// window closes, an unfocused one comes to the front, the dock showing it closes.
	pub fn toggle_device_editor(&mut self, r: DeviceRef) {
		let item = windows::WindowItem::Device(r);
		if self.view.windows.is_open(item) {
			if self.view.windows.focus == Some(item) {
				self.close_window(item);
			} else {
				self.view.windows.focus(item);
				self.sync_editor_probe();
			}
		} else if self.docked_device_shown(r) {
			self.close_piano_roll();
		} else {
			self.open_device_editor(r);
		}
	}

	/// Leaves the device editor of the dock (it goes back to the piano roll / audio
	/// editor) and closes the focused device window.
	pub fn close_device_editor(&mut self) {
		self.release_previews();
		self.view.device_editor = false;
		if let Some(r) = self.view.windows.focused_device() {
			self.view.windows.close(windows::WindowItem::Device(r));
		}
		self.sync_editor_probe();
	}

	/// Pops the dock's device out into its floating window; the dock shows the piano
	/// roll / audio editor again.
	pub fn pop_out_device_editor(&mut self) {
		let Some(r) = self.view.instrument.device else {
			return;
		};
		self.view.device_editor = false;
		self.open_window(windows::WindowItem::Device(r));
		self.sync_editor_probe();
	}

	/// Puts device window `r` into the bottom dock.
	pub fn dock_device_window(&mut self, r: DeviceRef) {
		self.open_device_in_dock(r);
	}

	/// Opens (or brings to the front) the floating window of `item`.
	pub fn open_window(&mut self, item: windows::WindowItem) -> bool {
		if !item.alive(self.project()) {
			return false;
		}
		let kind = item.kind_key(self.project());
		let size = match item {
			windows::WindowItem::Device(_) => windows::DEVICE_SIZE,
			windows::WindowItem::Track(_) => windows::TRACK_SIZE,
			windows::WindowItem::Clip(_) => windows::CLIP_SIZE,
		};
		self.view.windows.open(item, &kind, size);
		self.sync_editor_probe();
		true
	}

	/// Closes the floating window of `item`.
	pub fn close_window(&mut self, item: windows::WindowItem) {
		if item.device().is_some() {
			self.release_previews();
		}
		self.view.windows.close(item);
		self.sync_editor_probe();
	}

	/// The floating properties window of a track (`None` = the master channel).
	pub fn open_track_window(&mut self, track: Option<TrackId>) -> bool {
		match track {
			Some(t) => self.selection.select_track(t),
			None => self.selection.select_master(),
		}
		self.open_window(windows::WindowItem::Track(track))
	}

	/// The floating properties window of a clip.
	pub fn open_clip_window(&mut self, clip: ClipId) -> bool {
		if let Some((t, _)) = self.project().find_clip(clip) {
			let t = t.id;
			self.selection.click_clip(t, clip, false);
		}
		self.open_window(windows::WindowItem::Clip(clip))
	}

	/// Whether `r` is shown in a device editor (a window or the visible dock).
	pub fn device_editor_shown(&self, r: DeviceRef) -> bool {
		self.view.windows.is_open(windows::WindowItem::Device(r)) || self.docked_device_shown(r)
	}

	/// The device editor in front: the focused device window, else the visible dock's
	/// device, else the topmost device window (device alive).
	pub fn device_editor_target(&self) -> Option<DeviceRef> {
		let dock = self
			.view
			.instrument
			.device
			.filter(|r| self.docked_device_shown(*r));
		let r = self
			.view
			.windows
			.focused_device()
			.or(dock)
			.or_else(|| self.view.windows.top_device())?;
		self.device(r).is_some().then_some(r)
	}

	// --- A/B compare and the undo hint --------------------------------------------

	fn device_params(&self, r: DeviceRef) -> device_ab::Params {
		self.device(r).map(|d| d.params.clone()).unwrap_or_default()
	}

	/// The A/B state of `r`, both slots set to its current params the first time.
	pub fn compare(&mut self, r: DeviceRef) -> &device_ab::Compare {
		let current = self.device_params(r);
		self.compares
			.entry(r.device)
			.or_insert_with(|| device_ab::Compare::new(&current))
	}

	/// The slot `r` is on (A until the editor switched it).
	pub fn compare_slot(&self, r: DeviceRef) -> device_ab::Slot {
		self.compares
			.get(&r.device)
			.map_or(device_ab::Slot::A, |c| c.active())
	}

	/// Switches `r` to `slot`: its params become that slot's in ONE undo step
	/// ("Compare B"); nothing to undo when both slots are equal.
	pub fn switch_compare(&mut self, r: DeviceRef, slot: device_ab::Slot) -> bool {
		let current = self.device_params(r);
		let Some(state) = self.device(r).map(|d| d.state.clone()) else {
			return false;
		};
		let Some(apply) = self.compare(r).switch_to(slot, &current) else {
			return false;
		};
		if let Some(params) = apply {
			self.switching = true;
			let ok = self.apply(Command::Batch {
				label: slot.label(),
				commands: vec![Command::SetDeviceState {
					track: r.track,
					device: r.device,
					state,
					params: Some(params),
				}],
			});
			self.switching = false;
			if !ok {
				return false;
			}
		}
		if let Some(c) = self.compares.get_mut(&r.device) {
			c.flip(&current);
		}
		true
	}

	/// The other slot of `r` (the A/B key).
	pub fn toggle_compare(&mut self, r: DeviceRef) -> bool {
		let slot = self.compare_slot(r).other();
		self.switch_compare(r, slot)
	}

	/// "A→B": the slot `r` is on is copied onto the other one (view state, no undo step).
	pub fn copy_compare(&mut self, r: DeviceRef) {
		let current = self.device_params(r);
		self.compare(r);
		if let Some(c) = self.compares.get_mut(&r.device) {
			c.copy(&current);
		}
		let label = self.compares.get(&r.device).map(|c| c.copy_label());
		if let Some(label) = label {
			self.notices
				.info(trf("Copied {label}", &[("label", &label)]));
		}
	}

	/// The label of the step undo would revert, when that step touched `r`.
	pub fn device_undo_label(&self, r: DeviceRef) -> Option<&str> {
		self.touches
			.last_touches(r.device)
			.then(|| self.undo_label())
			.flatten()
	}

	/// Follows a document change from `before`: which devices the new / undone step
	/// touched, and A/B switches undone or redone (the slot flips back with them).
	fn follow_history(&mut self, before: &DocView, epoch_changed: bool) {
		if epoch_changed {
			self.touches.clear();
			self.compares.clear();
			self.ai_changes = None;
			self.hearing_before = None;
			return;
		}
		let sizes = |d: &DocView| (d.undo.len(), d.redo.len());
		let (b, a) = (sizes(before), sizes(&self.doc));
		let touched = device_ab::touched_devices(&before.project, &self.doc.project);
		let step = device_ab::step(b, a);
		self.follow_ai_changes(before, step);
		let label = match step {
			device_ab::Step::Undo => before.undo.last(),
			device_ab::Step::Redo => self.doc.undo.last(),
			_ => None,
		};
		if !self.switching && label.is_some_and(|l| device_ab::is_compare_label(l)) {
			let old = device_ab::devices(&before.project);
			for id in &touched {
				if let (Some(c), Some(d)) = (self.compares.get_mut(id), old.get(id)) {
					c.flip(&d.params);
				}
			}
		}
		self.touches.update(b, a, touched);
	}

	/// AI changes after a document change: undoing the AI step clears them, a later user
	/// step drops the params it moved.
	fn follow_ai_changes(&mut self, before: &DocView, step: device_ab::Step) {
		let Some(c) = &mut self.ai_changes else {
			return;
		};
		if c.undone(&self.doc.undo) {
			self.ai_changes = None;
			self.hearing_before = None;
			return;
		}
		if step == device_ab::Step::New && !self.ai.busy() {
			c.touch_changed(&before.project, &self.doc.project);
			if c.is_empty() {
				self.ai_changes = None;
				self.hearing_before = None;
			}
		}
	}

	/// Points the probe at the device editor's device (dock or popped-out window), and
	/// away from it once the editor closes, switches or the dock hides. With no editor
	/// open, falls back to the built-in device the inspector is showing (its compact
	/// forms draw from the same probe), so their analyzers and meters stay live instead of
	/// idle; only one consumer may drain the probe's frames a given frame, which is why the
	/// editor always wins the device when both show one. A probe set by someone else is
	/// left alone while neither needs it.
	/// The inspector's active tab, after following a selection change.
	pub fn inspector_tab(&mut self) -> inspector::InspectorTab {
		self.view.inspector.follow(self.selection.focus())
	}

	/// What the inspector's tabs edit for the current selection.
	pub fn inspector_targets(&self) -> inspector::Targets {
		inspector::Targets::of(&self.selection, self.project())
	}

	/// The device the inspector's Device tab shows (when that tab is active).
	pub fn inspector_device(&mut self) -> Option<DeviceRef> {
		if self.inspector_tab() != inspector::InspectorTab::Device {
			return None;
		}
		self.inspector_targets().device
	}

	/// Points the one probe at the device editor in front ([`Self::device_editor_target`]:
	/// the focused window first), else the inspector's Device tab.
	pub fn sync_editor_probe(&mut self) {
		let want = self
			.device_editor_target()
			.or_else(|| {
				if !self.view.panels.visible(PanelKind::Inspector) {
					return None;
				}
				self.inspector_device()
					.filter(|r| self.has_device_editor(*r))
			})
			.map(|r| r.device);
		if want != self.editor_probe {
			self.editor_probe = want;
			self.set_probe(want);
		}
		if self.listen.is_some_and(|(d, _)| Some(d) != want) {
			self.set_listen_target(None);
		}
	}

	/// Makes `device` (the device of the open editor) play a listen mode instead of its
	/// output, or back to normal with `Listen::Off`; one device at a time. Monitoring
	/// only: never in the document or renders, kept across reloads, cleared when the
	/// editor closes or shows another device.
	pub fn set_listen(&mut self, device: DeviceId, mode: cadenza_engine::Listen) {
		if mode == cadenza_engine::Listen::Off {
			if self.listen.is_some_and(|(d, _)| d == device) {
				self.set_listen_target(None);
			}
		} else {
			self.set_listen_target(Some((device, mode)));
		}
	}

	fn set_listen_target(&mut self, target: Option<(DeviceId, cadenza_engine::Listen)>) {
		if self.listen == target {
			return;
		}
		self.listen = target;
		if let Some(a) = &self.audio {
			a.listen(target);
		}
	}

	/// The listen mode `device` plays (`Listen::Off` when it does not listen).
	pub fn listen_mode(&self, device: DeviceId) -> cadenza_engine::Listen {
		match self.listen {
			Some((d, mode)) if d == device => mode,
			_ => cadenza_engine::Listen::Off,
		}
	}

	/// Factory and user presets of `r` (listed once per device through the MCP
	/// `list_presets` tool; refreshed after a save).
	pub fn device_presets(&mut self, r: DeviceRef) -> &[device::PresetEntry] {
		if self.device_presets.as_ref().map(|(d, _)| *d) != Some(r) {
			let list = self.list_device_presets(r);
			self.device_presets = Some((r, list));
		}
		self.device_presets
			.as_ref()
			.map_or(&[], |(_, list)| list.as_slice())
	}

	fn list_device_presets(&mut self, r: DeviceRef) -> Vec<device::PresetEntry> {
		let args = cadenza_mcp::tools::presets::ListPresetsArgs {
			track_id: r.track,
			device_id: r.device,
		};
		let listed = cadenza_mcp::tools::presets::list_presets(&mut self.session(), args);
		match listed {
			Ok(out) => out
				.presets
				.into_iter()
				.map(|p| device::PresetEntry {
					name: p.name,
					user: p.source == cadenza_mcp::tools::presets::PresetSource::User,
					description: p.description,
				})
				.collect(),
			Err(e) => {
				self.notices.error(e.to_string());
				Vec::new()
			}
		}
	}

	/// Loads preset `entry` into `r` through the MCP `load_preset` tool (one undo step).
	pub fn load_device_preset(&mut self, r: DeviceRef, entry: &device::PresetEntry) -> bool {
		use cadenza_mcp::tools::presets::{LoadPresetArgs, PresetSource, load_preset};
		let args = LoadPresetArgs {
			track_id: r.track,
			device_id: r.device,
			name: entry.name.clone(),
			source: Some(if entry.user {
				PresetSource::User
			} else {
				PresetSource::Factory
			}),
		};
		let Some(out) = self.edit_session(|s| load_preset(s, args)) else {
			return false;
		};
		self.loaded_presets.insert(
			r.device,
			device::LoadedPreset {
				name: out.name,
				user: entry.user,
				params: out.params,
			},
		);
		true
	}

	/// Saves `r`'s current settings as user preset `name` through the MCP `save_preset`
	/// tool (`overwrite` replaces one with that name).
	pub fn save_device_preset(&mut self, r: DeviceRef, name: &str, overwrite: bool) -> bool {
		use cadenza_mcp::tools::presets::{SavePresetArgs, save_preset};
		let args = SavePresetArgs {
			track_id: r.track,
			device_id: r.device,
			name: name.trim().to_owned(),
			overwrite,
		};
		if self.edit_session(|s| save_preset(s, args)).is_none() {
			return false;
		}
		let params = self.device(r).map(|d| d.params.clone()).unwrap_or_default();
		self.loaded_presets.insert(
			r.device,
			device::LoadedPreset {
				name: name.trim().to_owned(),
				user: true,
				params,
			},
		);
		self.device_presets = None;
		self.notices
			.info(trf("Saved preset '{name}'", &[("name", &name.trim())]));
		true
	}

	/// The preset `r` is on and whether its params moved away from it: the one loaded
	/// or saved here, else a factory preset with exactly these params.
	pub fn preset_status(&self, r: DeviceRef) -> device::PresetStatus {
		let Some(d) = self.device(r) else {
			return device::PresetStatus::default();
		};
		let Some(info) = d.builtin_id().and_then(cadenza_dsp::device_info) else {
			return device::PresetStatus::default();
		};
		device::preset_status(
			info,
			&d.params,
			self.loaded_presets.get(&r.device),
			cadenza_dsp::factory_presets(info.id),
		)
	}

	/// Loads `path` into a pad, zone or the whole keyboard of a sample device (the shared
	/// `load_sample` tool: root from the name or pitch, pad settings kept; one undo step).
	pub fn load_sample(&mut self, r: DeviceRef, path: &Path, slot: samples::SampleSlot) -> bool {
		if let samples::SampleSlot::Area { key, vel } = slot {
			let ok = self.add_zone_at(r, path, key, vel);
			if ok {
				self.open_device_editor(r);
			}
			return ok;
		}
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
				samples::SampleSlot::Keyboard | samples::SampleSlot::Area { .. } => {
					self.view.instrument.zone = 0
				}
			}
			if self.device(r).and_then(samples::kind) == Some(samples::SampleKind::Sampler) {
				self.view.instrument.last_sample = Some(path.to_path_buf());
			}
			self.open_device_editor(r);
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
		self.view.instrument.last_sample = Some(path.to_path_buf());
		self.open_device_editor(DeviceRef {
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

	/// Follows `device` (any built-in in the project: instrument, insert, master insert)
	/// with the engine's probe, or stops (`None`). Device editors drain
	/// [`AppState::probe`] once per frame for analyzers, meters and scopes.
	pub fn set_probe(&mut self, device: Option<DeviceId>) {
		if self.probe_target == device {
			return;
		}
		self.probe_target = device;
		if device.is_none() {
			self.probe = None;
		}
		if let Some(a) = &self.audio {
			a.probe(device);
		}
	}

	pub fn probe_target(&self) -> Option<DeviceId> {
		self.probe_target
	}

	/// Mono / dim on what the output device plays (after the meters, never in renders
	/// or the project). Kept across loads and device reopens.
	pub fn set_monitor_fx(&mut self, fx: cadenza_engine::MonitorFx) {
		self.monitor_fx = fx;
		if let Some(a) = &self.audio {
			a.monitor_fx(fx);
		}
	}

	pub fn monitor_fx(&self) -> cadenza_engine::MonitorFx {
		self.monitor_fx
	}

	/// Meters (and the master tap) of a player the app does not own, such as a test's
	/// virtual device; the app's own engine replaces them on its next load.
	pub fn attach_meters(
		&mut self,
		bank: Option<Arc<MeterBank>>,
		master: Option<Arc<DeviceProbe>>,
	) {
		self.bank = bank;
		match master {
			Some(tap) => self.master_loudness.attach(tap),
			None => self.master_loudness.detach(),
		}
	}

	/// Sidechain targets of `source`'s output menu (devices on other strips that take a
	/// key; see [`mixer::sidechain_targets`]).
	pub fn sidechain_targets(&mut self, source: TrackId) -> Vec<mixer::SidechainTarget> {
		let catalog = self.catalog();
		mixer::sidechain_targets(self.project(), source, |d| mixer::accepts_key(d, &catalog))
	}

	/// The probe ring of the running player while a device is probed (`None` without an
	/// audio engine or device, or until the engine answered).
	pub fn probe(&self) -> Option<&Arc<DeviceProbe>> {
		self.probe_target.and(self.probe.as_ref())
	}

	/// Attaches a player's probe ring directly (headless tests driving a virtual device;
	/// the app gets it from the audio thread).
	pub fn attach_probe(&mut self, probe: Option<Arc<DeviceProbe>>) {
		self.probe = probe;
	}

	/// The current output of `device`'s modulators (engine order), while it is the probed
	/// device and the player runs it.
	pub fn modulation_values(&self, device: DeviceId) -> Option<Vec<f32>> {
		match self.probe() {
			Some(p) if self.probe_target == Some(device) && p.is_active() => {
				let (values, n) = p.modulation();
				Some(values[..n].to_vec())
			}
			_ => None,
		}
	}

	/// What `device` plays now (held keys, voices), from its readings: empty unless it is
	/// the probed device and the player runs it.
	pub fn live_voices(&self, device: DeviceId) -> live_voices::LiveVoices {
		match self.probe() {
			Some(p) if self.probe_target == Some(device) && p.is_active() => {
				let (values, n) = p.readings();
				live_voices::LiveVoices::decode(&values[..n])
			}
			_ => live_voices::LiveVoices::default(),
		}
	}

	/// Keys to light on `r`'s editors: previews on its track (at once) plus the keys its
	/// processor holds (playback, live MIDI input). Lowest first, no repeats.
	pub fn lit_keys(&self, r: DeviceRef, live: &live_voices::LiveVoices) -> Vec<u8> {
		let mut keys: Vec<u8> = r
			.track
			.map(|t| {
				self.previewing
					.iter()
					.filter(|(tr, _)| *tr == t)
					.map(|(_, n)| *n)
					.collect()
			})
			.unwrap_or_default();
		keys.extend(live.held_keys());
		keys.sort_unstable();
		keys.dedup();
		keys
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
		// A pending structural edit (a new or swapped instrument) must reach the player
		// first, or the note would sound through the previous instrument.
		if self.reload_since.is_some() {
			self.send_reload();
		}
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

	// --- Style library ---------------------------------------------------------------

	/// The style library (the embedded `core` pack plus the user packs).
	pub fn library(&mut self) -> Arc<cadenza_library::Library> {
		if let Some(l) = &self.library {
			return Arc::clone(l);
		}
		let l = self.session().library();
		self.library = Some(Arc::clone(&l));
		l
	}

	/// Loads the library at startup and warns about packs with problems (never fatal).
	pub fn report_library(&mut self) {
		let library = self.library();
		for r in library.reports() {
			if r.issues.is_empty() {
				continue;
			}
			let what = r.pack.as_deref().unwrap_or(&r.location);
			let text = if r.loaded {
				trn_with(
					"Library pack '{pack}': {n} item skipped or broken. See Browser > Library > \
					 Credits.",
					"Library pack '{pack}': {n} items skipped or broken. See Browser > Library > \
					 Credits.",
					r.issues.len(),
					&[("pack", &what)],
				)
			} else {
				trf(
					"Library pack at {location} was not loaded: {first}",
					&[
						("location", &r.location),
						(
							"first",
							&r.issues.first().map_or("", |i| i.message.as_str()),
						),
					],
				)
			};
			self.notices.warn(text);
		}
	}

	/// Auditions a library pattern once, at the song tempo, on a fitting instrument track
	/// (the selected one when it fits). Stops a running preview of the same pattern.
	pub fn preview_pattern(&mut self, id: &str) -> bool {
		if self
			.library_preview
			.as_ref()
			.is_some_and(|p| p.pattern == id)
		{
			self.stop_pattern_preview();
			return false;
		}
		self.stop_pattern_preview();
		if self.is_playing() {
			self.notices
				.info(tr("Stop playback to preview library patterns"));
			return false;
		}
		let library = self.library();
		let Some(pattern) = library.pattern(id) else {
			return false;
		};
		let Some(track) = library::preview_track(self.project(), self.selection.track, pattern)
		else {
			self.notices.info(tr(
				"Add an instrument track to preview patterns (or drag one below the tracks)",
			));
			return false;
		};
		let at = self.position();
		match library::PatternPreview::new(self.project(), &library, id, track, at, Instant::now())
		{
			Ok(p) => {
				self.library_preview = Some(p);
				true
			}
			Err(e) => {
				self.notices.error(e);
				false
			}
		}
	}

	/// The pattern being previewed.
	pub fn pattern_preview(&self) -> Option<&str> {
		self.library_preview.as_ref().map(|p| p.pattern.as_str())
	}

	/// How far the previewed pattern has played, 0..=1.
	pub fn pattern_preview_progress(&self) -> Option<f32> {
		self.library_preview
			.as_ref()
			.map(|p| p.progress(Instant::now()))
	}

	pub fn stop_pattern_preview(&mut self) {
		if self.library_preview.take().is_some() {
			self.release_previews();
		}
	}

	/// Sends the preview's due notes; true while it runs.
	fn tick_pattern_preview(&mut self, now: Instant) -> bool {
		let Some(preview) = &mut self.library_preview else {
			return false;
		};
		let track = preview.track;
		let due = preview.due(now);
		let finished = preview.finished();
		for (pitch, velocity) in due {
			match velocity {
				Some(v) => self.preview_note_on(track, pitch, v),
				None => self.preview_note_off(track, pitch),
			}
		}
		if finished || self.is_playing() {
			self.stop_pattern_preview();
		}
		!finished
	}

	/// Writes a library pattern on `track` (`None`: a new track) from the bar of `at`
	/// (`None`: the playhead): one undo step, selected afterwards.
	pub fn add_library_pattern(
		&mut self,
		id: &str,
		track: Option<TrackId>,
		at: Option<Tick>,
	) -> bool {
		let library = self.library();
		let at = at.unwrap_or(self.position());
		match library::add_pattern(self.project(), &library, id, track, at) {
			Ok(drop) => {
				if !self.apply(drop.command) {
					return false;
				}
				self.selection.click_clip(drop.track, drop.clip, false);
				for w in drop.warnings {
					self.notices.warn(w);
				}
				true
			}
			Err(e) => {
				self.notices.error(e);
				false
			}
		}
	}

	/// Lays out a library form from bar 1 (markers, and with a style guide its tempo and
	/// the song's style): one undo step.
	pub fn apply_library_form(&mut self, form: &str, style: Option<&str>) -> bool {
		let library = self.library();
		match library::apply_form(self.project(), &library, form, style) {
			Ok(cmd) => self.apply(cmd),
			Err(e) => {
				self.notices.error(e);
				false
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
			mixer: v.mixer,
			arrange_tool: v.arrange_tool,
			track_heights: v.track_heights.clone(),
			windows: v.windows.saved.clone(),
			language: self.language,
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
		v.mixer = p.mixer;
		v.arrange_tool = p.arrange_tool;
		v.track_heights = p
			.track_heights
			.into_iter()
			.map(|(t, h)| (t, h.clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT)))
			.collect();
		v.windows.saved = p.windows;
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
		self.record.settings = p.record.migrated();
		self.apply_audio_settings(p.audio);
		self.set_language(p.language);
	}

	/// Switches the UI language; every label follows from the next frame on. Headless
	/// states keep the system choice out of tests: "System default" leaves this thread
	/// on the process language (English unless the app set it).
	pub fn set_language(&mut self, choice: crate::i18n::LanguageChoice) {
		use crate::i18n::{self, LanguageChoice};
		self.language = choice;
		if self.language_global {
			i18n::set_global(choice.resolve());
		} else {
			i18n::set_thread(match choice {
				LanguageChoice::System => None,
				LanguageChoice::Fixed(l) => Some(l),
			});
		}
	}

	/// Rescans plugins in the background (the scanner loads each new plugin in a helper
	/// process).
	pub fn rescan_plugins(&mut self) {
		if self.jobs.scan.is_some() {
			return;
		}
		let cache = self.session().plugin_cache_path().to_path_buf();
		self.notices.info(tr("Scanning plugins…"));
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
			FileAction::MixReference => self.mix.reference = Some(path),
			FileAction::MatchReference => self.finish_match_file(path),
			FileAction::ImportMidi => self.import_midi(&path),
			FileAction::PadSample | FileAction::PadLayer | FileAction::KitFolder => {
				self.finish_sample_dialog(action, &path);
			}
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
		self.sync_editor_probe();
		if let Some(since) = self.reload_since
			&& now.saturating_duration_since(since) >= RELOAD_DEBOUNCE
		{
			self.send_reload();
		}
		let actions = self.editors.flush(&self.doc.project, now, false);
		self.editor_actions(actions);
		self.poll_jobs();
		self.poll_mix();
		self.poll_match_eq();
		self.poll_collisions();
		self.poll_file_jobs();
		self.tick_autosave(now);
		self.sync_transport();
		self.tick_midi_learn(now);
		let (playing, position) = (self.is_playing(), self.position());
		self.tick_autowrite(playing, position);
		self.tick_record(dt);
		let previewing = self.tick_pattern_preview(now);
		self.peaks.poll();
		let ids: Vec<TrackId> = self.project().tracks.iter().map(|t| t.id).collect();
		self.meters.update(self.bank.as_deref(), ids, dt);
		let playing = self.is_playing();
		self.master_loudness.update(playing, dt);
		self.is_playing()
			|| previewing
			|| self.meters.any_active()
			|| self.reload_since.is_some()
			|| self.editors.busy()
			|| self.jobs.scan.is_some()
			|| self.export.running.is_some()
			|| self.files_jobs.busy()
			|| self.jobs.process.is_some()
			|| self.jobs.bounce.is_some()
			|| self.ai.busy()
			|| self.mix.busy()
			|| self.record_animating()
			|| self.midi.learning.is_some()
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
					master,
					midi_map,
					status,
				} => {
					self.master_loudness.attach(master);
					self.attach_midi_map(Some(midi_map));
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
					self.probe = None;
					self.master_loudness.detach();
					self.notices.warn(trf("No audio output: {e}", &[("e", &e)]));
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
					self.notices
						.error(trf("Playback engine: {error}", &[("error", &error)]));
				}
				AudioEvent::ReloadNeeded => self.schedule_reload(),
				AudioEvent::AudioThread(scheduling) => {
					if let Some(status) = &mut self.record.status {
						status.audio_thread = Some(scheduling);
					}
				}
				AudioEvent::Probe(probe) => {
					self.probe = probe.filter(|_| self.probe_target.is_some());
				}
				AudioEvent::Xruns { device, plugins } => {
					self.xruns.update(device, plugins, Instant::now());
				}
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
				AudioEvent::Error(e) => self.notices.error(trf("Audio: {e}", &[("e", &e)])),
			}
		}
	}

	fn poll_jobs(&mut self) {
		self.poll_bounce();
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
							self.notices.info(trf(
								"{label}: wrote {done}",
								&[("label", &done.op.label()), ("done", &done.file.display())],
							));
						}
					}
					None => self
						.notices
						.warn(tr("The clip was removed before the operation finished")),
				},
				Some(Err(e)) => self
					.notices
					.error(trf("Audio operation failed: {e}", &[("e", &e)])),
				None => self
					.notices
					.error(tr("Audio operation failed: the worker thread stopped")),
			}
		}
		if let Some(r) = self.jobs.scan.as_ref().and_then(Job::poll) {
			self.jobs.scan = None;
			match r {
				Some(Ok((catalog, report))) => {
					self.session().set_plugins(Arc::new(catalog));
					self.browser_items = None;
					self.notices.info(trf(
						"Plugin scan: {files} files, {added} added, {updated} updated, {removed} \
						 removed, {blocked} blocked",
						&[
							("files", &report.files_found),
							("added", &report.added.len()),
							("updated", &report.updated.len()),
							("removed", &report.removed.len()),
							("blocked", &report.blocked.len()),
						],
					));
				}
				Some(Err(e)) => self
					.notices
					.error(trf("Plugin scan failed: {e}", &[("e", &e)])),
				None => self
					.notices
					.error(tr("Plugin scan failed: the scan thread stopped")),
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
