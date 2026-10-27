//! Recording in the app: arm states, the record button and record mode, metronome,
//! count-in and punch settings, audio device settings, takes, and turning a finished
//! recording into ONE undoable "Record" batch.
//!
//! Arm states are session state (not in the project, like Studio One's record enables):
//! they live here and are mirrored to the player, which keeps them across reloads. A
//! track's audio input and monitor mode are project data (`Track::input`, edited with
//! `UpdateTrack`). Recording itself runs in the engine (`cadenza_engine::record`); this
//! module decides what to record (`AppState::record_plan`), starts and stops it through
//! the audio thread and applies `cadenza_engine::record_commands` when the files are
//! done. While a MIDI loop recording runs, finished passes are merged into the player's
//! copy of the track (FL Studio overdub: you hear what you just played on the next pass)
//! without touching the document until the stop.

use crate::i18n::{tr, trf};
use std::path::PathBuf;
use std::sync::Arc;

use cadenza_core::{ClipId, PPQ, TakeGroupId, Tick, TrackId};
use cadenza_engine::{
	AudioConfig, AudioDevices, InputStats, MetronomeSettings, MidiItem, RecordOptions, RecordSetup,
	RecordTrack, Recording, RecordingMonitor, record_commands,
};
use cadenza_model::{Command, Document, MonitorMode, TrackInput, TrackKind, TrackPatch};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::audio::DeviceStatus;

/// Count-in choices (bars).
pub const COUNT_IN_CHOICES: [u32; 4] = [0, 1, 2, 4];

/// Snap for recorded MIDI note starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputQuantize {
	#[default]
	Off,
	Quarter,
	Eighth,
	Sixteenth,
	ThirtySecond,
}

impl InputQuantize {
	pub const ALL: [InputQuantize; 5] = [
		Self::Off,
		Self::Quarter,
		Self::Eighth,
		Self::Sixteenth,
		Self::ThirtySecond,
	];

	pub fn grid(self) -> Option<Tick> {
		match self {
			Self::Off => None,
			Self::Quarter => Some(Tick(PPQ)),
			Self::Eighth => Some(Tick(PPQ / 2)),
			Self::Sixteenth => Some(Tick(PPQ / 4)),
			Self::ThirtySecond => Some(Tick(PPQ / 8)),
		}
	}

	pub fn name(self) -> &'static str {
		match self {
			Self::Off => tr("Off"),
			Self::Quarter => "1/4",
			Self::Eighth => "1/8",
			Self::Sixteenth => "1/16",
			Self::ThirtySecond => "1/32",
		}
	}
}

/// Recording preferences (persisted).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSettings {
	pub metronome: MetronomeSettings,
	/// Bars of count-in before recording (0 = none).
	pub count_in_bars: u32,
	/// Record only inside the arrange loop region (punch-in/out).
	pub punch: bool,
	/// MIDI: add to the clip under the recording start (else a new clip).
	pub midi_merge: bool,
	/// MIDI: keep the notes already there (else replace the recorded range).
	pub midi_overdub: bool,
	/// MIDI loop recording: each pass becomes a take (else passes merge, FL-style).
	pub midi_takes: bool,
	pub quantize: InputQuantize,
	/// Settings revision (missing in older prefs = 0, see [`RecordSettings::migrated`]).
	#[serde(default)]
	pub revision: u32,
}

/// Current [`RecordSettings::revision`].
const RECORD_SETTINGS_REVISION: u32 = 2;

impl RecordSettings {
	/// Prefs saved before revision 2 defaulted to a count-in and a click while recording
	/// that the user never asked for: both now follow the metronome toggle (off unless it
	/// is on) and no count-in.
	pub fn migrated(mut self) -> Self {
		if self.revision < RECORD_SETTINGS_REVISION {
			self.count_in_bars = 0;
			self.metronome.on_record = self.metronome.on_play;
			self.revision = RECORD_SETTINGS_REVISION;
		}
		self
	}
}

impl Default for RecordSettings {
	fn default() -> Self {
		Self {
			metronome: MetronomeSettings::default(),
			count_in_bars: 0,
			punch: false,
			midi_merge: true,
			midi_overdub: true,
			midi_takes: false,
			quantize: InputQuantize::Off,
			revision: RECORD_SETTINGS_REVISION,
		}
	}
}

/// Audio device settings (persisted).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioPrefs {
	pub config: AudioConfig,
	/// MIDI input port name (`None`: no MIDI input).
	pub midi_input: Option<String>,
	/// Added to the measured recording latency (positive moves recordings earlier).
	pub latency_offset_ms: f32,
}

/// Where a recording is.
#[derive(Debug, Clone, Default)]
pub enum Phase {
	#[default]
	Idle,
	/// Asked the audio thread to start.
	Starting,
	Recording(Arc<RecordingMonitor>),
	/// Asked to stop; the files are being finished.
	Stopping,
}

/// What a click on the record button (or Ctrl+R) does, given whether record mode is
/// on and whether a recording/count-in is already running. Pure so the decision is
/// unit-tested without an `AppState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordButtonAction {
	/// Starts a recording.
	Start,
	/// Stops the recording (or count-in) in progress.
	Stop,
	/// Nothing is recording and record mode is on: turn it off. This is the fix for
	/// the button staying lit forever — mode on, Play never pressed again — with no
	/// recording to show for it.
	ModeOff,
}

pub(crate) fn record_button_action(mode: bool, recording: bool) -> RecordButtonAction {
	if recording {
		RecordButtonAction::Stop
	} else if mode {
		RecordButtonAction::ModeOff
	} else {
		RecordButtonAction::Start
	}
}

/// The audio settings dialog: a draft of the preferences until "Apply".
#[derive(Debug, Clone, PartialEq)]
pub struct AudioDialog {
	pub draft: AudioPrefs,
}

/// What a press of Record does (see [`AppState::record_plan`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordPlan {
	pub setup: RecordSetup,
	/// Where playback starts (before the punch-in: pre-roll).
	pub play_from: Tick,
	/// Pattern mode: the MIDI clip recorded into.
	pub pattern: Option<ClipId>,
}

#[derive(Default)]
pub struct RecordState {
	/// Armed tracks as the player has them: a mirror of `Project::armed` (saved with the
	/// song, changed through `Command::SetArmedTracks`), synced after every edit.
	pub armed: Vec<TrackId>,
	/// Record mode (R): Play records.
	pub mode: bool,
	pub settings: RecordSettings,
	pub audio: AudioPrefs,
	pub phase: Phase,
	pattern: Option<ClipId>,
	/// MIDI passes already merged into the player while recording.
	passes_sent: usize,
	/// Audio devices and MIDI ports (from the audio thread, for the settings).
	pub devices: Option<(AudioDevices, Vec<String>)>,
	pub status: Option<DeviceStatus>,
	pub input_stats: Option<Arc<InputStats>>,
	/// Shown input level per channel (peak with fall-off, linear).
	pub input_levels: Vec<f32>,
	pub dialog: Option<AudioDialog>,
	pub test_tone: bool,
	/// The recording in progress as of this frame (one snapshot per frame).
	pub live: Option<cadenza_engine::LiveRecording>,
	/// Instrument tracks that record the voice from `hum_input` and turn it into MIDI
	/// when the recording ends (hum-to-MIDI; session state like arming).
	pub hum: std::collections::BTreeSet<TrackId>,
	/// Input the hummed tracks record (`None`: input 1).
	pub hum_input: Option<TrackInput>,
}

/// Input level fall-off, per second (linear full scale).
const LEVEL_FALL: f32 = 1.5;

impl AppState {
	/// The count-in before a recording while it runs (from the player's clock).
	pub fn count_in(&self) -> Option<cadenza_engine::CountInStatus> {
		self.clock.as_ref().and_then(|c| c.count_in())
	}

	pub fn is_armed(&self, track: TrackId) -> bool {
		self.project().armed.contains(&track)
	}

	/// Audio and instrument tracks can record.
	/// Whether an instrument track records hummed audio as MIDI.
	pub fn is_hum(&self, track: TrackId) -> bool {
		self.record.hum.contains(&track)
	}

	/// Hum to MIDI on / off for an instrument track (arming it when turned on).
	pub fn toggle_hum(&mut self, track: TrackId) {
		if self
			.project()
			.track(track)
			.is_none_or(|t| t.kind != TrackKind::Instrument)
		{
			return;
		}
		if !self.record.hum.remove(&track) {
			self.record.hum.insert(track);
			if !self.is_armed(track) {
				self.toggle_arm(track);
			}
			self.notices.info(tr(
				"Hum to MIDI: record your voice on this track; it becomes notes when you stop",
			));
		}
	}

	pub fn can_arm(&self, track: TrackId) -> bool {
		self.project()
			.track(track)
			.is_some_and(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
	}

	/// Arms or disarms `track` (its ● button; one undo step, saved with the song). An
	/// audio track without an input gets input 1 in the same step; instrument tracks play
	/// the MIDI input while armed.
	pub fn toggle_arm(&mut self, track: TrackId) {
		if self.is_recording() {
			self.notices
				.info(tr("Stop recording before arming or disarming tracks"));
			return;
		}
		let Some(t) = self.project().track(track) else {
			return;
		};
		if !matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) {
			self.notices
				.info(tr("Only audio and instrument tracks record"));
			return;
		}
		let mut armed = self.project().armed.clone();
		let mut commands = Vec::new();
		let label = if armed.contains(&track) {
			armed.retain(|a| *a != track);
			format!("Disarm '{}'", t.name)
		} else {
			if t.kind == TrackKind::Audio && t.input.is_none() {
				commands.push(Command::UpdateTrack {
					track,
					patch: TrackPatch {
						input: Some(Some(TrackInput::mono(0))),
						..TrackPatch::default()
					},
				});
			}
			armed.push(track);
			format!("Arm '{}'", t.name)
		};
		commands.push(Command::SetArmedTracks { armed });
		// The player follows in `sync_arms` (after this edit, and after undo/redo).
		self.apply(Command::Batch { label, commands });
	}

	/// Live MIDI plays through the last armed instrument track.
	fn update_midi_monitor(&self) {
		let p = self.project();
		let monitor = self
			.record
			.armed
			.iter()
			.rev()
			.copied()
			.find(|t| p.track(*t).is_some_and(|t| t.kind == TrackKind::Instrument));
		if let Some(a) = &self.audio {
			a.monitor(monitor);
		}
	}

	/// Sets (or with `None` removes) an audio track's input (one undo step).
	pub fn set_track_input(&mut self, track: TrackId, input: Option<TrackInput>) -> bool {
		if self.project().track(track).map(|t| t.input) == Some(input) {
			return true;
		}
		self.apply(Command::UpdateTrack {
			track,
			patch: TrackPatch {
				input: Some(input),
				..TrackPatch::default()
			},
		})
	}

	/// Off -> Auto -> On -> Off for an audio track's input monitoring.
	pub fn cycle_monitor(&mut self, track: TrackId) {
		let Some(input) = self.project().track(track).and_then(|t| t.input) else {
			self.notices
				.info(tr("Choose an input for the track first (its In button)"));
			return;
		};
		let monitor = match input.monitor {
			MonitorMode::Off => MonitorMode::Auto,
			MonitorMode::Auto => MonitorMode::On,
			MonitorMode::On => MonitorMode::Off,
		};
		self.set_track_input(track, Some(TrackInput { monitor, ..input }));
	}

	/// The monitor button: on <-> off (Auto counts as on and goes off).
	pub fn toggle_monitor(&mut self, track: TrackId) {
		let Some(input) = self.project().track(track).and_then(|t| t.input) else {
			self.notices
				.info(tr("Choose an input for the track first (its In button)"));
			return;
		};
		let mode = if input.monitor == MonitorMode::Off {
			MonitorMode::On
		} else {
			MonitorMode::Off
		};
		self.set_monitor(track, mode);
	}

	/// Sets the input monitor mode of an audio track with an input (one undo step).
	pub fn set_monitor(&mut self, track: TrackId, monitor: MonitorMode) {
		if let Some(input) = self.project().track(track).and_then(|t| t.input)
			&& input.monitor != monitor
		{
			self.set_track_input(track, Some(TrackInput { monitor, ..input }));
		}
	}

	/// A recording is starting, running or being finished.
	pub fn is_recording(&self) -> bool {
		!matches!(self.record.phase, Phase::Idle)
	}

	/// The recording in progress (for drawing it live).
	pub fn recording_monitor(&self) -> Option<&Arc<RecordingMonitor>> {
		match &self.record.phase {
			Phase::Recording(m) => Some(m),
			_ => None,
		}
	}

	/// R: record mode on/off (Play then records, like FL Studio).
	pub fn toggle_record_mode(&mut self) {
		self.record.mode = !self.record.mode;
	}

	/// Ctrl+R and the record button: start recording, stop a running one, or (record
	/// mode on, nothing recording yet) turn record mode off. That third case is what a
	/// user sees as the button "staying lit" — mode on, nothing actually happening — so
	/// a click there clears the mode instead of starting an unwanted recording.
	pub fn toggle_record(&mut self) {
		match record_button_action(self.record.mode, self.is_recording()) {
			RecordButtonAction::Stop => self.stop_recording(),
			RecordButtonAction::ModeOff => self.record.mode = false,
			RecordButtonAction::Start => self.record(),
		}
	}

	/// The folder recordings go to: the project's `Audio/`, or a state folder for an
	/// unsaved project (collected into the project when it is saved).
	pub fn record_folder(&self) -> PathBuf {
		match self.path() {
			Some(p) => cadenza_model::ProjectLayout::new(p).audio_dir(),
			None => self
				.safety
				.state_dir
				.join("recordings")
				.join(format!("untitled-{}", std::process::id())),
		}
	}

	/// What Record would capture now: the armed tracks, from where, with which punch
	/// range, and where playback starts. Pattern mode records into the pattern loop.
	pub fn record_plan(&self) -> Result<RecordPlan, String> {
		let p = self.project();
		let tracks: Vec<RecordTrack> = self
			.record
			.armed
			.iter()
			.filter_map(|id| p.track(*id))
			.filter_map(|t| match t.kind {
				TrackKind::Audio => Some(RecordTrack {
					track: t.id,
					name: t.name.clone(),
					input: Some(t.input.unwrap_or(TrackInput::mono(0))),
				}),
				TrackKind::Instrument => Some(RecordTrack {
					track: t.id,
					name: t.name.clone(),
					input: self
						.record
						.hum
						.contains(&t.id)
						.then(|| self.record.hum_input.unwrap_or(TrackInput::mono(0))),
				}),
				TrackKind::Bus | TrackKind::Folder => None,
			})
			.collect();
		if tracks.is_empty() {
			return Err(tr(
				"Arm a track to record (the ● button on an audio or instrument track header)",
			)
			.into());
		}
		let (config, _) = self.transport_config();
		let pos = self.position();
		let inside = |(a, b): (Tick, Tick)| pos >= a && pos < b;
		let punch = self
			.view
			.transport
			.loop_range
			.filter(|_| self.record.settings.punch);
		let (start, play_from, punch_out, pattern) = match (config.pattern, config.loop_range) {
			(Some(clip), Some(range)) => {
				let from = if self.cued && inside(range) {
					pos
				} else {
					range.0
				};
				(range.0, from, None, Some(clip))
			}
			(None, looped) => match (punch, looped) {
				// Punch + loop: loop recording inside the region (a take per pass).
				(Some(r), Some(_)) => (r.0, if inside(r) { pos } else { r.0 }, None, None),
				// Punch alone: pre-roll from the playhead, capture only the region.
				(Some(r), None) => (r.0, if pos >= r.1 { r.0 } else { pos }, Some(r.1), None),
				(None, Some(r)) => {
					let from = if inside(r) { pos } else { r.0 };
					(from, from, None, None)
				}
				(None, None) => (pos, pos, None, None),
			},
			(Some(_), None) => (pos, pos, None, None),
		};
		let mut setup = RecordSetup::new(self.record_folder(), start, tracks);
		setup.punch_out = punch_out;
		setup.count_in_bars = self.record.settings.count_in_bars;
		Ok(RecordPlan {
			setup,
			play_from,
			pattern,
		})
	}

	/// Starts recording the armed tracks (with the count-in), playing from the plan's
	/// position.
	pub fn record(&mut self) {
		if self.is_recording() || self.ai_locked() {
			return;
		}
		// Pattern mode with nothing armed records into the pattern's track.
		if self.project().armed.is_empty()
			&& let (config, _) = self.transport_config()
			&& let Some((track, _)) = config.pattern.and_then(|c| self.project().find_clip(c))
		{
			let track = track.id;
			self.toggle_arm(track);
		}
		let plan = match self.record_plan() {
			Ok(plan) => plan,
			Err(e) => {
				self.notices.info(e);
				return;
			}
		};
		if self.audio.is_none() || self.clock.is_none() {
			self.notices.warn(tr(
				"No audio device: recording is unavailable (see the audio settings)",
			));
			return;
		}
		if self.reload_since.is_some() {
			self.send_reload();
		}
		let mut setup = plan.setup;
		if self.is_playing() {
			// Punch in on the fly: capture from here, no count-in, no restart.
			setup.count_in_bars = 0;
			self.send_record_settings();
		} else {
			self.play_intent = true;
			let (config, _) = self.transport_config();
			self.locate(plan.play_from);
			self.send_config(config);
			self.send_record_settings();
		}
		if let Some(a) = &self.audio {
			a.start_recording(setup);
		}
		self.record.pattern = plan.pattern;
		self.record.passes_sent = 0;
		self.record.phase = Phase::Starting;
	}

	/// Stops capturing (the record button / Ctrl+R): the transport keeps playing. The
	/// clips arrive as one undo step when the files are done.
	pub fn stop_recording(&mut self) {
		self.end_recording(true);
	}

	/// Stops capturing; `keep_playing: false` also stops the transport (Stop).
	pub(crate) fn end_recording(&mut self, keep_playing: bool) {
		if !matches!(self.record.phase, Phase::Starting | Phase::Recording(_)) {
			return;
		}
		let Some(a) = &self.audio else {
			// No audio thread to ever answer: `Phase::Stopping` would be stuck forever
			// (the button lit with nothing recording). There is nothing to keep.
			self.record.phase = Phase::Idle;
			return;
		};
		a.stop_recording(keep_playing);
		self.record.phase = Phase::Stopping;
	}

	/// The audio device is gone (its thread exited): any pending start/stop will never
	/// get an answer, so a recording in progress or being stopped must not leave the
	/// record button lit forever.
	pub(crate) fn abort_stuck_recording(&mut self) {
		if self.is_recording() {
			self.record.phase = Phase::Idle;
			self.play_intent = false;
			self.notices
				.warn(tr("Recording stopped: the audio device is gone"));
		}
	}

	/// Adds what was recorded to the project as ONE undo step ("Record").
	pub fn finish_recording(&mut self, rec: &Recording) -> bool {
		self.record.phase = Phase::Idle;
		for w in &rec.warnings {
			self.notices.warn(w.clone());
		}
		let s = self.record.settings;
		let options = RecordOptions {
			merge: s.midi_merge,
			overdub: s.midi_overdub,
			midi_takes: s.midi_takes,
			quantize: s.quantize.grid(),
			pattern_clip: self.record.pattern.take(),
		};
		let cache = self
			.analysis_cache
			.clone()
			.unwrap_or_else(cadenza_mcp::tools::notes::default_cache_dir);
		let (rec, hummed, warnings) =
			super::to_midi::hum_commands(self.project(), rec, &self.record.hum, &cache);
		for w in warnings {
			self.notices.warn(w);
		}
		let cmd = match (
			record_commands(self.project(), &rec, &options),
			hummed.is_empty(),
		) {
			(
				Some(Command::Batch {
					label,
					mut commands,
				}),
				_,
			) => {
				commands.extend(hummed);
				Some(Command::Batch { label, commands })
			}
			(Some(cmd), true) => Some(cmd),
			(Some(cmd), false) => Some(Command::Batch {
				label: "Record".into(),
				commands: std::iter::once(cmd).chain(hummed).collect(),
			}),
			(None, false) => Some(Command::Batch {
				label: "Record".into(),
				commands: hummed,
			}),
			(None, true) => None,
		};
		let Some(cmd) = cmd else {
			self.notices.info(tr("Nothing was recorded"));
			// The player may still play merged passes: give it the project back.
			self.schedule_reload();
			return false;
		};
		let ok = self.apply_inner(cmd);
		if ok {
			let takes = rec.passes.len().max(1);
			self.notices.info(if takes > 1 {
				trf(
					"Recorded {takes} takes — one undo step (Ctrl+Z)",
					&[("takes", &takes)],
				)
			} else {
				tr("Recorded — one undo step (Ctrl+Z)").to_owned()
			});
		}
		// Merged MIDI passes were only in the player.
		self.schedule_reload();
		ok
	}

	/// Recording events from the audio thread.
	pub(crate) fn record_event(&mut self, event: RecordEvent) {
		match event {
			RecordEvent::Started(monitor) => {
				if matches!(self.record.phase, Phase::Starting) {
					self.record.phase = Phase::Recording(monitor);
				}
			}
			RecordEvent::Failed(e) => {
				self.record.phase = Phase::Idle;
				self.play_intent = false;
				self.notices.error(trf("Recording: {e}", &[("e", &e)]));
			}
			RecordEvent::Done(rec) => {
				self.finish_recording(&rec);
			}
		}
	}

	/// Sends the metronome and latency settings to the player.
	pub fn send_record_settings(&self) {
		let Some(a) = &self.audio else {
			return;
		};
		a.metronome(self.record.settings.metronome);
		let rate = self.clock.as_ref().map_or(48_000, |c| c.sample_rate());
		let frames = (f64::from(self.record.audio.latency_offset_ms) * f64::from(rate) / 1000.0)
			.round() as i64;
		a.latency_offset(frames);
	}

	pub fn set_record_settings(&mut self, settings: RecordSettings) {
		if self.record.settings != settings {
			self.record.settings = settings;
			self.send_record_settings();
		}
	}

	/// Opens the audio settings dialog (and asks for the device lists).
	pub fn open_audio_settings(&mut self) {
		self.record.dialog = Some(AudioDialog {
			draft: self.record.audio.clone(),
		});
		if let Some(a) = &self.audio {
			a.list_devices();
		}
	}

	/// Applies (and persists) audio settings: new devices reopen the player.
	pub fn apply_audio_settings(&mut self, prefs: AudioPrefs) {
		if self.is_recording() {
			self.notices
				.info(tr("Stop recording before changing the audio settings"));
			return;
		}
		let old = std::mem::replace(&mut self.record.audio, prefs);
		if let Some(a) = &self.audio {
			if old.config != self.record.audio.config {
				a.configure(self.record.audio.config.clone());
			}
			if old.midi_input != self.record.audio.midi_input {
				a.midi_input(self.record.audio.midi_input.clone());
			}
		}
		self.send_record_settings();
	}

	pub fn set_test_tone(&mut self, on: bool) {
		self.record.test_tone = on;
		if let Some(a) = &self.audio {
			a.test_tone(on);
		}
	}

	/// Shown level (linear) of `input`'s channels.
	pub fn input_level(&self, input: &TrackInput) -> f32 {
		input
			.channels()
			.filter_map(|c| self.record.input_levels.get(usize::from(c)).copied())
			.fold(0.0, f32::max)
	}

	/// Input channels of the open audio input (2 when unknown: the common case).
	pub fn input_channels(&self) -> u16 {
		self.record
			.status
			.as_ref()
			.and_then(|s| s.input.as_ref())
			.map_or(2, |i| i.channels.max(1))
	}

	/// Per frame: input level ballistics, and MIDI loop passes merged into the player.
	pub(crate) fn tick_record(&mut self, dt: f32) {
		self.record.live = self.recording_monitor().map(|m| m.snapshot());
		if let Some(stats) = &self.record.input_stats {
			let n =
				usize::from(self.input_channels()).min(cadenza_engine::input::MAX_INPUT_CHANNELS);
			self.record.input_levels.resize(n, 0.0);
			for (c, level) in self.record.input_levels.iter_mut().enumerate() {
				let peak = stats.take_peak(c);
				*level = peak.max(*level - LEVEL_FALL * dt).max(0.0);
			}
		}
		self.preview_midi_passes();
	}

	/// Whether input levels are moving (repaint).
	pub(crate) fn record_animating(&self) -> bool {
		self.is_recording()
			|| self.record.test_tone
			|| self.record.input_levels.iter().any(|l| *l > 0.0)
	}

	/// FL-style overdub: once a MIDI loop pass is complete, the player plays it on the
	/// next passes (the document changes only when recording stops).
	fn preview_midi_passes(&mut self) {
		let Phase::Recording(monitor) = &self.record.phase else {
			return;
		};
		let live = monitor.snapshot();
		let wraps = live
			.midi
			.iter()
			.filter(|m| matches!(m, MidiItem::Wrap { .. }))
			.count();
		if wraps <= self.record.passes_sent || self.record.settings.midi_takes {
			return;
		}
		self.record.passes_sent = wraps;
		let last_wrap = live
			.midi
			.iter()
			.rposition(|m| matches!(m, MidiItem::Wrap { .. }))
			.expect("a wrap");
		let MidiItem::Wrap { from, .. } = live.midi[last_wrap] else {
			return;
		};
		let midi_tracks: Vec<TrackId> = monitor
			.tracks()
			.iter()
			.filter(|t| t.input.is_none())
			.map(|t| t.track)
			.collect();
		let rec = Recording {
			sample_rate: monitor.sample_rate(),
			start: monitor.start_sample(),
			punch_out: None,
			stop: from,
			latency: 0,
			midi_latency: 0,
			passes: Vec::new(),
			audio: Vec::new(),
			midi_tracks: midi_tracks.clone(),
			midi: live.midi[..last_wrap].to_vec(),
			dropouts: 0,
			warnings: Vec::new(),
		};
		let s = self.record.settings;
		let options = RecordOptions {
			merge: s.midi_merge,
			overdub: true,
			midi_takes: false,
			quantize: s.quantize.grid(),
			pattern_clip: self.record.pattern,
		};
		let Some(cmd) = record_commands(self.project(), &rec, &options) else {
			return;
		};
		let mut doc = Document::new(self.project().clone());
		if doc.execute(cmd).is_err() {
			return;
		}
		let Some(a) = &self.audio else {
			return;
		};
		for track in midi_tracks {
			if let Some(t) = doc.project().track(track) {
				a.track_clips(track, t.clips.clone(), doc.project().tempo.clone());
			}
		}
	}

	/// Drops take views of tracks that are gone and brings the player's arm states to the
	/// project's (an arm click, undo/redo, an opened song).
	pub(crate) fn prune_record(&mut self) {
		let p = &self.doc.project;
		self.view.takes.retain(|t| p.track(*t).is_some());
		self.record
			.hum
			.retain(|t| p.track(*t).is_some_and(|t| t.kind == TrackKind::Instrument));
		if self.record.armed == p.armed {
			return;
		}
		if let Some(a) = &self.audio {
			for &t in self.record.armed.iter().filter(|t| !p.armed.contains(t)) {
				a.arm(t, false);
			}
			for &t in p.armed.iter().filter(|t| !self.record.armed.contains(t)) {
				a.arm(t, true);
			}
		}
		self.record.armed = p.armed.clone();
		self.update_midi_monitor();
	}

	/// Re-sends arm states and settings to a (re)started player.
	pub(crate) fn resend_record_state(&self) {
		if let Some(a) = &self.audio {
			for &t in &self.record.armed {
				a.arm(t, true);
			}
		}
		self.update_midi_monitor();
		self.send_record_settings();
	}

	/// For the AI: armed tracks, metronome and count-in (`None` when nothing is armed).
	pub fn recording_context(&self) -> Option<String> {
		let p = self.project();
		let armed: Vec<String> = self
			.record
			.armed
			.iter()
			.filter_map(|id| p.track(*id))
			.map(|t| match (t.kind, t.input) {
				(TrackKind::Audio, Some(i)) => format!(
					"{} (audio, {}, monitor {})",
					t.name,
					i.label(),
					i.monitor.name()
				),
				_ if self.is_hum(t.id) => {
					format!("{} (hum to MIDI: voice from In 1 becomes notes)", t.name)
				}
				_ => format!("{} (MIDI)", t.name),
			})
			.collect();
		if armed.is_empty() && !self.is_recording() {
			return None;
		}
		let s = &self.record.settings;
		let click = match (s.metronome.on_record, s.metronome.on_play) {
			(true, true) => tr("metronome always on"),
			(true, false) => tr("metronome on while recording"),
			(false, true) => tr("metronome on while playing"),
			(false, false) => tr("metronome off"),
		};
		Some(format!(
			"armed: {}; {click}; count-in {} bar(s){}{}",
			if armed.is_empty() {
				"none".to_owned()
			} else {
				armed.join(", ")
			},
			s.count_in_bars,
			if s.punch {
				"; punch in/out on the loop region"
			} else {
				""
			},
			if self.is_recording() {
				"; the user is recording right now"
			} else {
				""
			}
		))
	}

	pub fn takes_open(&self, track: TrackId) -> bool {
		self.view.takes.contains(&track)
	}

	/// Shows / hides the take lanes under `track`.
	pub fn toggle_takes(&mut self, track: TrackId) {
		if self.takes_open(track) {
			self.view.takes.retain(|t| *t != track);
		} else {
			self.view.takes.push(track);
		}
	}

	/// Plays lane `lane` of `track` wherever it has a take (one undo step).
	pub fn select_lane(&mut self, track: TrackId, lane: u32) {
		let Some(t) = self.project().track(track) else {
			return;
		};
		let mut groups = Vec::new();
		let mut commands = Vec::new();
		for c in &t.clips {
			let Some(take) = c.take.filter(|t| t.lane == lane) else {
				continue;
			};
			if groups.contains(&take.group) {
				continue;
			}
			groups.push(take.group);
			if let Some(Command::Batch { commands: cmds, .. }) =
				cadenza_model::takes::select_take(self.project(), c.id)
			{
				commands.extend(cmds);
			}
		}
		if !commands.is_empty() {
			self.apply(Command::Batch {
				label: format!("Play take {}", lane + 1),
				commands,
			});
		}
	}

	/// Plays take `clip` for its whole group (one undo step).
	pub fn select_take(&mut self, clip: ClipId) {
		if let Some(cmd) = cadenza_model::takes::select_take(self.project(), clip) {
			self.apply(cmd);
		}
	}

	/// Swipe comping: plays `clip`'s take from `from` to `to` (one undo step).
	pub fn comp_take(&mut self, clip: ClipId, from: Tick, to: Tick) {
		if let Some(cmd) = cadenza_model::takes::comp_range(self.project(), clip, from, to) {
			self.apply(cmd);
		}
	}

	/// Keeps only the playing take of `group` as an ordinary clip (one undo step).
	pub fn flatten_takes(&mut self, group: TakeGroupId) {
		if let Some(cmd) = cadenza_model::takes::flatten_group(self.project(), group) {
			self.apply(cmd);
		}
	}
}

/// Recording news from the audio thread (see `AudioEvent`).
pub(crate) enum RecordEvent {
	Started(Arc<RecordingMonitor>),
	Failed(String),
	Done(Box<Recording>),
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_engine::{RecordedAudio, RecordedPass};
	use cadenza_model::{Project, Track};

	fn state_with_vox() -> (AppState, TrackId) {
		let mut s = AppState::headless();
		let mut p = Project::new("rec");
		let vox = Track::new("Vox", TrackKind::Audio);
		let id = vox.id;
		p.tracks.push(vox);
		s.load_document(Document::new(p), None);
		(s, id)
	}

	/// Root cause of the record button staying lit forever: `end_recording` used to set
	/// `Phase::Stopping` even when there was no audio thread left to ever answer (the
	/// device gone, e.g. disconnected mid-recording) — nothing would ever move the phase
	/// back to `Idle`, so `is_recording()` (and the button) stayed true forever. Losing
	/// the device while a recording is starting/running must also clear it right away.
	#[test]
	fn a_lost_audio_device_does_not_leave_the_record_button_stuck() {
		let (mut s, vox) = state_with_vox();
		s.toggle_arm(vox);

		// Pressing Stop with no audio thread (`self.audio` is `None` in headless tests,
		// like a device that just disappeared) must resolve to Idle, not get stuck
		// waiting in `Stopping`.
		s.record.phase = Phase::Starting;
		assert!(s.is_recording());
		s.stop_recording();
		assert!(!s.is_recording(), "no thread will ever answer this stop");

		// The audio thread reporting its own disappearance while a recording is in
		// progress must also unstick the button.
		s.record.phase = Phase::Starting;
		s.play_intent = true;
		assert!(s.is_recording());
		s.abort_stuck_recording();
		assert!(!s.is_recording());
		assert!(!s.play_intent());
	}

	/// The button's three states: start, stop, and (mode on, idle) turn the mode off.
	#[test]
	fn record_button_action_covers_start_stop_and_mode_off() {
		assert_eq!(
			record_button_action(false, false),
			RecordButtonAction::Start
		);
		assert_eq!(
			record_button_action(true, false),
			RecordButtonAction::ModeOff
		);
		// Recording wins over mode either way: a click always stops a running capture.
		assert_eq!(record_button_action(false, true), RecordButtonAction::Stop);
		assert_eq!(record_button_action(true, true), RecordButtonAction::Stop);
	}

	/// Reviewer-reported cause A: record mode (R) left lit with nothing recording.
	/// Clicking the record button (or Ctrl+R) in that state must turn the mode off
	/// instead of starting a recording the user did not ask for; while a recording (or
	/// its count-in) is running, the same click still stops it as before.
	#[test]
	fn toggle_record_turns_off_a_lit_mode_with_nothing_recording() {
		let (mut s, _vox) = state_with_vox();
		s.toggle_record_mode();
		assert!(s.record.mode);
		assert!(!s.is_recording());

		s.toggle_record();
		assert!(!s.record.mode, "the button click clears the lit mode");
		assert!(!s.is_recording());

		// Mode on and a recording already running: click still stops the recording,
		// not the mode.
		s.toggle_record_mode();
		assert!(s.record.mode);
		s.record.phase = Phase::Starting;
		s.toggle_record();
		assert!(
			!s.is_recording(),
			"click stops a running recording/count-in"
		);
		assert!(
			s.record.mode,
			"stopping a recording must not also clear record mode"
		);
	}

	#[test]
	fn arming_an_audio_track_gives_it_an_input_and_plans_a_recording() {
		let (mut s, vox) = state_with_vox();
		assert!(s.record_plan().is_err(), "nothing armed");
		s.toggle_arm(vox);
		assert!(s.is_armed(vox));
		assert_eq!(
			s.project().track(vox).unwrap().input,
			Some(TrackInput::mono(0))
		);
		s.cycle_monitor(vox);
		assert_eq!(
			s.project().track(vox).unwrap().input.unwrap().monitor,
			MonitorMode::On
		);
		s.seek(Tick(PPQ * 4));
		let plan = s.record_plan().unwrap();
		assert_eq!(plan.setup.start, Tick(PPQ * 4));
		assert_eq!(plan.play_from, Tick(PPQ * 4));
		assert_eq!(plan.setup.count_in_bars, 0);
		assert_eq!(
			plan.setup.tracks[0].input,
			Some(TrackInput {
				monitor: MonitorMode::On,
				..TrackInput::mono(0)
			})
		);
		// Punch on the loop region: pre-roll from the playhead, capture only the region.
		s.view.transport.loop_range = Some((Tick(PPQ * 8), Tick(PPQ * 16)));
		s.record.settings.punch = true;
		let plan = s.record_plan().unwrap();
		assert_eq!(
			(plan.setup.start, plan.setup.punch_out, plan.play_from),
			(Tick(PPQ * 8), Some(Tick(PPQ * 16)), Tick(PPQ * 4))
		);
		// Punch + loop: loop recording inside the region.
		s.view.transport.loop_on = true;
		let plan = s.record_plan().unwrap();
		assert_eq!(
			(plan.setup.punch_out, plan.play_from),
			(None, Tick(PPQ * 8))
		);
		assert!(
			s.recording_context()
				.unwrap()
				.contains("Vox (audio, In 1, monitor on)")
		);
		s.toggle_arm(vox);
		assert!(!s.is_armed(vox) && s.recording_context().is_none());
	}

	#[test]
	fn a_finished_loop_recording_is_one_undo_step_with_takes() {
		let (mut s, vox) = state_with_vox();
		s.toggle_arm(vox);
		let undo_before = s.document().history_labels().len();
		let beat = 24_000i64;
		let rec = Recording {
			sample_rate: 48_000,
			start: 0,
			punch_out: None,
			stop: 0,
			latency: 0,
			midi_latency: 0,
			passes: (0..3)
				.map(|k| RecordedPass {
					tag: 0,
					file_start: k * 2 * beat as u64,
					frames: 2 * beat as u64,
					wrapped_at: (k < 2).then_some(2 * beat),
				})
				.collect(),
			audio: vec![RecordedAudio {
				track: vox,
				name: "Vox".into(),
				file: PathBuf::from("/tmp/vox.wav"),
				frames: 6 * beat as u64,
				channels: 1,
			}],
			midi_tracks: Vec::new(),
			midi: Vec::new(),
			dropouts: 0,
			warnings: Vec::new(),
		};
		s.record.phase = Phase::Stopping;
		assert!(s.finish_recording(&rec));
		assert!(!s.is_recording());
		let labels = s.document().history_labels();
		assert_eq!(labels.len(), undo_before + 1);
		assert_eq!(labels.last(), Some(&"Record"));
		let clips = &s.project().track(vox).unwrap().clips;
		assert_eq!(clips.len(), 3);
		assert_eq!(clips.iter().filter(|c| c.plays()).count(), 1);
		let group = clips[0].take.unwrap().group;
		// Comp: the first take for the second beat only.
		let first = clips.iter().find(|c| c.take.unwrap().lane == 0).unwrap().id;
		s.comp_take(first, Tick(PPQ), Tick(PPQ * 2));
		let playing: Vec<(u32, Tick)> = s
			.project()
			.track(vox)
			.unwrap()
			.clips
			.iter()
			.filter(|c| c.plays())
			.map(|c| (c.take.unwrap().lane, c.start))
			.collect();
		assert_eq!(playing, vec![(2, Tick::ZERO), (0, Tick(PPQ))]);
		s.undo();
		s.flatten_takes(group);
		assert_eq!(s.project().track(vox).unwrap().clips.len(), 1);
	}

	#[test]
	fn old_prefs_lose_the_unrequested_count_in_and_recording_click() {
		let fresh = RecordSettings::default();
		assert_eq!(fresh.count_in_bars, 0);
		assert!(!fresh.metronome.on_record && !fresh.metronome.on_play);
		assert_eq!(fresh.migrated(), fresh);
		let old: RecordSettings = serde_json::from_str(
			r#"{"count_in_bars":1,"metronome":{"on_play":false,"on_record":true,"level_db":-6.0}}"#,
		)
		.unwrap();
		let m = old.migrated();
		assert_eq!(m.count_in_bars, 0);
		assert!(!m.metronome.on_record, "follows the (off) metronome toggle");
		// Settings saved after the change are kept as they are.
		let mut chosen = RecordSettings {
			count_in_bars: 2,
			..RecordSettings::default()
		};
		chosen.metronome.on_record = true;
		assert_eq!(chosen.migrated(), chosen);
	}

	#[test]
	fn a_hummed_recording_lands_as_notes_on_the_instrument_track() {
		use cadenza_notes::eval::synth::{Phrase, SynthNote, Vowel, render};
		let dir = tempfile::tempdir().unwrap();
		let wav = dir.path().join("Lead rec.wav");
		let r = render(&Phrase {
			notes: [57.0, 60.0, 62.0, 64.0]
				.into_iter()
				.map(|p| SynthNote::new(p, 0.4))
				.collect(),
			vowel: Vowel::U,
			sample_rate: 48_000,
			snr_db: Some(30.0),
			reverb: None,
			seed: 9,
		});
		let frames = r.samples.len() as u64;
		let buf = cadenza_audio_io::AudioBuffer::from_planar(48_000, vec![r.samples]).unwrap();
		cadenza_audio_io::export(
			&buf,
			&wav,
			&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
		)
		.unwrap();
		let mut s = AppState::headless();
		s.analysis_cache = Some(dir.path().join("cache"));
		let mut p = Project::new("hum");
		let lead = Track::new("Lead", TrackKind::Instrument);
		let id = lead.id;
		p.tracks.push(lead);
		s.load_document(Document::new(p), None);
		s.toggle_hum(id);
		assert!(
			s.is_hum(id) && s.is_armed(id),
			"turning hum on arms the track"
		);
		let plan = s.record_plan().unwrap();
		assert_eq!(
			plan.setup.tracks[0].input,
			Some(TrackInput::mono(0)),
			"it records audio"
		);
		let undo_before = s.document().history_labels().len();
		let rec = Recording {
			sample_rate: 48_000,
			start: 0,
			punch_out: None,
			stop: frames as i64,
			latency: 0,
			midi_latency: 0,
			passes: vec![RecordedPass {
				tag: 0,
				file_start: 0,
				frames,
				wrapped_at: None,
			}],
			audio: vec![RecordedAudio {
				track: id,
				name: "Lead".into(),
				file: wav.clone(),
				frames,
				channels: 1,
			}],
			midi_tracks: Vec::new(),
			midi: Vec::new(),
			dropouts: 0,
			warnings: Vec::new(),
		};
		s.record.phase = Phase::Stopping;
		assert!(s.finish_recording(&rec));
		assert_eq!(s.document().history_labels().len(), undo_before + 1);
		let track = s.project().track(id).unwrap();
		assert_eq!(track.clips.len(), 1);
		let clip = &track.clips[0];
		assert!(clip.is_midi());
		let pitches: Vec<u8> = clip.notes().iter().map(|n| n.pitch).collect();
		assert_eq!(pitches, [57, 60, 62, 64]);
		// Hum defaults: on the sixteenth grid.
		for n in clip.notes() {
			assert_eq!((clip.start + n.start).0 % (PPQ / 4), 0, "{n:?}");
		}
		assert!(wav.exists(), "the take audio is kept");
	}
}
