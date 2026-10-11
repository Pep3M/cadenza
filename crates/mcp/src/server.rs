//! The MCP server: a thin `rmcp` adapter over the plain tool functions in `tools/*.rs`.
//!
//! Every tool method here does the same few things: lock the session, call the matching
//! function in `tools/`, and convert its `Result<T, McpError>` into
//! `Result<Json<T>, rmcp::ErrorData>` (or `Result<(), rmcp::ErrorData>`). All actual logic
//! — validation, command construction, music theory, rendering — lives in `tools/`.
//!
//! Mutating tools go through [`CadenzaServer::run_mut`], which additionally pushes the
//! post-mutation project into the realtime player if it is currently playing, so playback
//! reflects edits made while the transport is running.

use std::sync::Mutex;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, Json, ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::error::ServeError;
use crate::player::PlayerHandle;
use crate::session::Session;
use crate::tools::{arrange, clips, compose, devices, playback, project, render, tracks};

const INSTRUCTIONS: &str = "\
Cadenza: you are the arranger and mix engineer for this project. Everything you do goes \
through undoable commands, so experiment freely and use `undo`/`redo`/`history` to back \
out of mistakes.\n\n\
Typical workflow:\n\
1. `get_project` for the current state; `list_devices` to find instruments and effects. \
Built-in devices are always available. Third-party plugins (e.g. Surge XT, LSP) appear \
after `scan_plugins` has run once on this machine (call it on first use if list_devices \
reports no plugins); search them with `list_devices { query, role, source }` and inspect \
their parameters with `get_device_info { device, param_query }` — plugins can have \
hundreds of params, so always filter. Plugin device ids look like \"clap:<plugin id>\".\n\
2. `create_track` for each instrument (synth, bass, drums...), optionally loading an \
instrument device inline.\n\
3. Write parts with `write_chord_progression` (chord symbols or roman numerals + a key, \
with voicing and comping pattern) and `write_drum_pattern` (ASCII step grids on GM lanes \
like kick/snare/closed_hat); or build clips by hand with `add_midi_clip`/`add_notes`. Use \
`scale_info`/`snap_to_scale` to keep melodies in key, `quantize_notes`/`humanize_notes`/\
`transpose_notes` to shape them.\n\
4. Route tracks to reverb/delay buses with `create_track` (kind: bus) + `add_send`; shape \
tone with `insert_device`/`set_device_params` on a track or the master.\n\
5. Balance the mix with `set_channel` (volume/pan/mute/solo) and mark song sections with \
`add_marker`. Use `analyze_mix` to get loudness/peak/band readings plus plain-language \
mix observations before committing to a `render`. `play`/`stop` audition on the default \
output device; edits made while playing are pushed to the player automatically.\n\n\
Positions and durations are always in beats (quarter notes); pitches are note names \
(\"C4\") or MIDI numbers (60 = C4); velocity is 0..1; volume is dB; pan is -1..1.";

pub struct CadenzaServer {
	session: Mutex<Session>,
	player: Mutex<Option<PlayerHandle>>,
	tool_router: ToolRouter<Self>,
}

impl CadenzaServer {
	pub fn new() -> Self {
		Self {
			session: Mutex::new(Session::new()),
			player: Mutex::new(None),
			tool_router: Self::tool_router(),
		}
	}

	fn session(&self) -> std::sync::MutexGuard<'_, Session> {
		self.session
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
	}

	fn player(&self) -> std::sync::MutexGuard<'_, Option<PlayerHandle>> {
		self.player
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
	}

	/// Runs a mutating tool function against the session, then — if the player exists and
	/// is currently playing — pushes the resulting project into it so playback reflects
	/// the edit.
	fn run_mut<T>(
		&self,
		f: impl FnOnce(&mut Session) -> Result<T, crate::McpError>,
	) -> Result<T, ErrorData> {
		let value = f(&mut self.session())?;
		self.sync_player_if_playing();
		Ok(value)
	}

	fn sync_player_if_playing(&self) {
		let still_playing = matches!(self.player().as_ref(), Some(p) if p.is_playing_hint());
		if still_playing {
			let (project, plugins) = self.project_and_plugins();
			if let Some(p) = self.player().as_ref() {
				let _ = p.load(project, plugins);
			}
		}
	}

	/// The current project plus the plugin catalog if the project uses plugins.
	fn project_and_plugins(
		&self,
	) -> (
		cadenza_model::Project,
		Option<std::sync::Arc<cadenza_plugin_host::PluginCatalog>>,
	) {
		let mut session = self.session();
		let plugins = session.plugins_for_project();
		(session.document().project().clone(), plugins)
	}

	/// Opens the default audio output device on first use.
	fn ensure_player(&self) -> Result<(), crate::McpError> {
		let mut guard = self.player();
		if guard.is_none() {
			*guard = Some(PlayerHandle::start()?);
		}
		Ok(())
	}
}

impl Default for CadenzaServer {
	fn default() -> Self {
		Self::new()
	}
}

#[tool_router(router = tool_router)]
impl CadenzaServer {
	// -- project --------------------------------------------------------------------

	#[tool(description = "Creates a new empty project, replacing the one currently open.")]
	fn new_project(
		&self,
		Parameters(args): Parameters<project::NewProjectArgs>,
	) -> Result<Json<cadenza_model::ProjectSummary>, ErrorData> {
		Ok(Json(self.run_mut(|s| project::new_project(s, args))?))
	}

	#[tool(description = "Opens a project from a JSON file, replacing the one currently open.")]
	fn open_project(
		&self,
		Parameters(args): Parameters<project::OpenProjectArgs>,
	) -> Result<Json<cadenza_model::ProjectSummary>, ErrorData> {
		Ok(Json(self.run_mut(|s| project::open_project(s, args))?))
	}

	#[tool(description = "Saves the current project to disk as JSON.")]
	fn save_project(
		&self,
		Parameters(args): Parameters<project::SaveProjectArgs>,
	) -> Result<Json<project::SaveProjectOutput>, ErrorData> {
		Ok(Json(project::save_project(&mut self.session(), args)?))
	}

	#[tool(
		description = "Returns a compact, AI-oriented summary of the whole project: tracks, \
			devices, clips, mixer settings, tempo, markers. Positions are in beats."
	)]
	fn get_project(&self) -> Json<cadenza_model::ProjectSummary> {
		Json(project::get_project(&self.session()))
	}

	#[tool(description = "Lists the notes of a MIDI clip, with names and beat positions.")]
	fn get_clip_notes(
		&self,
		Parameters(args): Parameters<project::GetClipNotesArgs>,
	) -> Result<Json<Vec<project::NoteOut>>, ErrorData> {
		Ok(Json(project::get_clip_notes(&self.session(), args)?))
	}

	#[tool(description = "Reverts the last change.")]
	fn undo(&self) -> Result<Json<project::HistoryStep>, ErrorData> {
		Ok(Json(self.run_mut(project::undo)?))
	}

	#[tool(description = "Re-applies the last undone change.")]
	fn redo(&self) -> Result<Json<project::HistoryStep>, ErrorData> {
		Ok(Json(self.run_mut(project::redo)?))
	}

	#[tool(description = "Lists undoable and redoable steps, each with its human-readable label.")]
	fn history(&self) -> Json<project::HistoryOutput> {
		Json(project::history(&self.session()))
	}

	// -- devices ----------------------------------------------------------------------

	#[tool(
		description = "Searches available instruments and effects: built-in devices (always \
			available, returned with their full parameter lists) and scanned third-party plugins \
			(compact entries: id, name, vendor, format, role, features, param_count, latency; \
			use get_device_info for their params). Filter with role, query (case-insensitive \
			words over name/vendor/features/description) and source (builtin|plugin|all); \
			paged with limit (default 50)/offset. Device ids are stable: built-ins use plain ids \
			(\"poly_synth\"), plugins use \"clap:<plugin id>\" or \"vst3:<class id>\"; pass \
			them to create_track.instrument or insert_device. When a plugin is listed in both \
			formats, prefer the CLAP version."
	)]
	fn list_devices(
		&self,
		Parameters(args): Parameters<devices::ListDevicesArgs>,
	) -> Json<devices::ListDevicesOutput> {
		let catalog =
			(args.source != Some(devices::SourceFilter::Builtin)).then(|| self.session().plugins());
		Json(devices::list_devices(catalog.as_deref(), args))
	}

	#[tool(
		description = "Describes one device (built-in id or plugin id like \"clap:<plugin id>\") \
			with its parameters: id, name, module (plugin group), min, max, default, stepped. \
			Filter with param_query (words matched against name/module, e.g. \"filter cutoff\") \
			and page with limit (default 100)/offset. Non-automatable plugin params are hidden \
			unless include_non_automatable is true. Plugin params can then be set by id or by \
			exact name / \"module/name\"."
	)]
	fn get_device_info(
		&self,
		Parameters(args): Parameters<devices::GetDeviceInfoArgs>,
	) -> Result<Json<devices::DeviceInfoOutput>, ErrorData> {
		let catalog = crate::convert::needs_catalog(&args.device).then(|| self.session().plugins());
		Ok(Json(devices::get_device_info(catalog.as_deref(), args)?))
	}

	#[tool(
		description = "Scans the machine for installed third-party plugins (CLAP and VST3) and updates \
			the plugin catalog. Each new or changed plugin file is loaded in a separate, \
			crash-safe process; files that crash or hang are blocklisted and reported with a \
			reason. Unchanged files are skipped, so rescans are fast. Omit paths to scan the \
			standard locations. Run it once before using plugins, or after installing new ones."
	)]
	async fn scan_plugins(
		&self,
		Parameters(args): Parameters<devices::ScanPluginsArgs>,
	) -> Result<Json<devices::ScanPluginsOutput>, ErrorData> {
		Ok(Json(devices::scan_plugins(&self.session, args).await?))
	}

	// -- tracks / mixer ----------------------------------------------------------------

	#[tool(
		description = "Creates a track (instrument, audio or bus), optionally with an instrument \
			loaded: a built-in id (\"poly_synth\") or a plugin id (\"clap:<plugin id>\") from \
			list_devices, plus optional params."
	)]
	fn create_track(
		&self,
		Parameters(args): Parameters<tracks::CreateTrackArgs>,
	) -> Result<Json<tracks::CreateTrackOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| tracks::create_track(s, args))?))
	}

	#[tool(description = "Renames a track and/or changes its color.")]
	fn update_track(
		&self,
		Parameters(args): Parameters<tracks::UpdateTrackArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::update_track(s, args))
	}

	#[tool(description = "Deletes a track and everything on it (clips, devices, sends).")]
	fn delete_track(
		&self,
		Parameters(args): Parameters<tracks::DeleteTrackArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::delete_track(s, args))
	}

	#[tool(
		description = "Changes a channel's volume, pan, mute, solo and/or output. Omit track_id \
			to change the master channel."
	)]
	fn set_channel(
		&self,
		Parameters(args): Parameters<tracks::SetChannelArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_channel(s, args))
	}

	#[tool(description = "Adds a send from a track to a bus track (e.g. for shared reverb/delay).")]
	fn add_send(
		&self,
		Parameters(args): Parameters<tracks::AddSendArgs>,
	) -> Result<Json<tracks::AddSendOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| tracks::add_send(s, args))?))
	}

	#[tool(description = "Removes a send.")]
	fn remove_send(
		&self,
		Parameters(args): Parameters<tracks::RemoveSendArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::remove_send(s, args))
	}

	// -- devices on a channel -----------------------------------------------------------

	#[tool(
		description = "Inserts an effect into a channel's chain. Omit track_id to insert on \
			the master bus. device is a built-in id or a plugin id (\"clap:<plugin id>\") from \
			list_devices; params are validated against the device's ranges (plugin params by id, \
			exact name or \"module/name\"; see get_device_info)."
	)]
	fn insert_device(
		&self,
		Parameters(args): Parameters<tracks::InsertDeviceArgs>,
	) -> Result<Json<tracks::InsertDeviceOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| tracks::insert_device(s, args))?))
	}

	#[tool(description = "Removes an insert effect from a channel.")]
	fn remove_device(
		&self,
		Parameters(args): Parameters<tracks::RemoveDeviceArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::remove_device(s, args))
	}

	#[tool(
		description = "Sets one or more parameters of a device (instrument or insert) in a \
			single undo step. Values are validated against the device's declared range. For \
			plugin devices, keys may be the param id or the exact (case-insensitive) name or \
			\"module/name\" from get_device_info."
	)]
	fn set_device_params(
		&self,
		Parameters(args): Parameters<tracks::SetDeviceParamsArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_device_params(s, args))
	}

	#[tool(description = "Bypasses or re-enables a device.")]
	fn set_bypass(
		&self,
		Parameters(args): Parameters<tracks::SetBypassArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_bypass(s, args))
	}

	#[tool(
		description = "Sets a single parameter by its raw address (volume, pan, or a device \
			parameter). Prefer set_channel/set_device_params unless you already have an address \
			from get_project."
	)]
	fn set_param(
		&self,
		Parameters(args): Parameters<tracks::SetParamArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_param(s, args))
	}

	#[tool(
		description = "Replaces the automation curve for a parameter address; an empty points list removes it."
	)]
	fn set_automation(
		&self,
		Parameters(args): Parameters<tracks::SetAutomationArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_automation(s, args))
	}

	// -- clips / notes -------------------------------------------------------------------

	#[tool(
		description = "Creates a MIDI clip on an Instrument track, optionally with initial notes."
	)]
	fn add_midi_clip(
		&self,
		Parameters(args): Parameters<clips::AddMidiClipArgs>,
	) -> Result<Json<clips::AddMidiClipOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| clips::add_midi_clip(s, args))?))
	}

	#[tool(description = "Adds notes to an existing MIDI clip.")]
	fn add_notes(
		&self,
		Parameters(args): Parameters<clips::AddNotesArgs>,
	) -> Result<Json<clips::AddNotesOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| clips::add_notes(s, args))?))
	}

	#[tool(description = "Removes notes from a clip by id, or every note with all: true.")]
	fn remove_notes(
		&self,
		Parameters(args): Parameters<clips::RemoveNotesArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::remove_notes(s, args))
	}

	#[tool(
		description = "Transposes notes by a number of semitones. Omit note_ids to transpose the whole clip."
	)]
	fn transpose_notes(
		&self,
		Parameters(args): Parameters<clips::TransposeNotesArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::transpose_notes(s, args))
	}

	#[tool(
		description = "Snaps every note in a clip onto a rhythmic grid, with adjustable strength."
	)]
	fn quantize_notes(
		&self,
		Parameters(args): Parameters<clips::QuantizeNotesArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::quantize_notes(s, args))
	}

	#[tool(
		description = "Applies deterministic random timing and velocity variation to every \
			note in a clip; the same seed always produces the same result."
	)]
	fn humanize_notes(
		&self,
		Parameters(args): Parameters<clips::HumanizeNotesArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::humanize_notes(s, args))
	}

	#[tool(description = "Moves a clip to a new start beat and/or a different track.")]
	fn move_clip(
		&self,
		Parameters(args): Parameters<clips::MoveClipArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::move_clip(s, args))
	}

	#[tool(
		description = "Resizes a clip; notes that would start outside the new length are rejected."
	)]
	fn resize_clip(
		&self,
		Parameters(args): Parameters<clips::ResizeClipArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::resize_clip(s, args))
	}

	#[tool(description = "Deletes a clip and its notes.")]
	fn delete_clip(
		&self,
		Parameters(args): Parameters<clips::DeleteClipArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| clips::delete_clip(s, args))
	}

	#[tool(
		description = "Duplicates a clip (with fresh ids for the clip and its notes) to a new position."
	)]
	fn duplicate_clip(
		&self,
		Parameters(args): Parameters<clips::DuplicateClipArgs>,
	) -> Result<Json<clips::DuplicateClipOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| clips::duplicate_clip(s, args))?))
	}

	// -- composition helpers ---------------------------------------------------------

	#[tool(
		description = "Writes a chord progression into a new MIDI clip. Accepts chord symbols \
			(\"Am7 Dm9 G13 Cmaj7\") or, with a key, roman numerals (\"vi ii V I\"). Choose a \
			voicing (close/open/drop2/drop3/shell/rootless) and a comping pattern \
			(sustain/arpeggio_up/arpeggio_down/arpeggio_updown/strum/pulse). Returns the clip id \
			and the resolved chords with their concrete pitches."
	)]
	fn write_chord_progression(
		&self,
		Parameters(args): Parameters<compose::WriteChordProgressionArgs>,
	) -> Result<Json<compose::WriteChordProgressionOutput>, ErrorData> {
		Ok(Json(
			self.run_mut(|s| compose::write_chord_progression(s, args))?,
		))
	}

	#[tool(
		description = "Writes a drum pattern into a new MIDI clip from ASCII step grids, one per \
			GM lane (kick, snare, clap, closed_hat, open_hat, low_tom, mid_tom, high_tom, crash, \
			ride, or any note name/MIDI number). Steps: X accent, x normal, o ghost, '.'/'-' rest; \
			spaces and '|' are ignored; the pattern repeats to fill the requested bars."
	)]
	fn write_drum_pattern(
		&self,
		Parameters(args): Parameters<compose::WriteDrumPatternArgs>,
	) -> Result<Json<compose::WriteDrumPatternOutput>, ErrorData> {
		Ok(Json(
			self.run_mut(|s| compose::write_drum_pattern(s, args))?,
		))
	}

	#[tool(
		description = "Looks up the notes of a scale, e.g. \"C major\", \"A minor\", \"F# dorian\"."
	)]
	fn scale_info(
		&self,
		Parameters(args): Parameters<compose::ScaleInfoArgs>,
	) -> Result<Json<compose::ScaleInfoOutput>, ErrorData> {
		Ok(Json(compose::scale_info(args)?))
	}

	#[tool(description = "Snaps every out-of-key note in a clip onto the nearest tone of a scale.")]
	fn snap_to_scale(
		&self,
		Parameters(args): Parameters<compose::SnapToScaleArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| compose::snap_to_scale(s, args))
	}

	// -- arrangement -------------------------------------------------------------------

	#[tool(description = "Sets the tempo from a beat onward (default: the whole song).")]
	fn set_tempo(
		&self,
		Parameters(args): Parameters<arrange::SetTempoArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| arrange::set_tempo(s, args))
	}

	#[tool(description = "Sets the project's time signature.")]
	fn set_time_signature(
		&self,
		Parameters(args): Parameters<arrange::SetTimeSignatureArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| arrange::set_time_signature(s, args))
	}

	#[tool(
		description = "Adds a song-section marker (Intro, Verse 1, Chorus...) at a beat position."
	)]
	fn add_marker(
		&self,
		Parameters(args): Parameters<arrange::AddMarkerArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| arrange::add_marker(s, args))
	}

	#[tool(description = "Removes the marker at a beat position.")]
	fn remove_marker(
		&self,
		Parameters(args): Parameters<arrange::RemoveMarkerArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| arrange::remove_marker(s, args))
	}

	// -- rendering / analysis --------------------------------------------------------

	#[tool(
		description = "Renders the project to a 32-bit float stereo WAV file and returns the \
			mix report (levels, loudness, band energy, per track). CPU-heavy; runs off the main \
			session lock."
	)]
	async fn render(
		&self,
		Parameters(args): Parameters<render::RenderArgs>,
	) -> Result<Json<cadenza_engine::MixReport>, ErrorData> {
		let (project, plugins) = self.project_and_plugins();
		Ok(Json(render::render(project, plugins, args).await?))
	}

	#[tool(
		description = "Renders and measures the mix (no audio written) and returns the report \
			plus plain-language observations (e.g. peaks too hot, loudness off target, a track \
			much louder than the rest, clipping, mono/phase issues, muddy or harsh bands) to \
			guide the next mix move."
	)]
	async fn analyze_mix(
		&self,
		Parameters(args): Parameters<render::AnalyzeMixArgs>,
	) -> Result<Json<render::AnalyzeMixOutput>, ErrorData> {
		let (project, plugins) = self.project_and_plugins();
		Ok(Json(render::analyze_mix(project, plugins, args).await?))
	}

	// -- transport -----------------------------------------------------------------

	#[tool(
		description = "Starts (or resumes) playback on the default audio output device, \
			reloading the current project first. Opens the output device on first use; returns \
			a clear error if none is available."
	)]
	fn play(
		&self,
		Parameters(args): Parameters<playback::PlayArgs>,
	) -> Result<Json<playback::TransportStatus>, ErrorData> {
		self.ensure_player()?;
		let (project, plugins) = self.project_and_plugins();
		let guard = self.player();
		Ok(Json(playback::play(
			guard.as_ref().unwrap(),
			project,
			plugins,
			args,
		)?))
	}

	#[tool(description = "Stops playback.")]
	fn stop(&self) -> Result<Json<playback::TransportStatus>, ErrorData> {
		Ok(Json(playback::stop(self.player().as_ref())?))
	}

	#[tool(description = "Reports whether the transport is playing and its current beat position.")]
	fn transport_status(&self) -> Result<Json<playback::TransportStatus>, ErrorData> {
		Ok(Json(playback::transport_status(self.player().as_ref())?))
	}
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CadenzaServer {
	fn get_info(&self) -> ServerConfig {
		ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
			.with_instructions(INSTRUCTIONS)
	}
}

/// Serves `CadenzaServer` over stdio until the client disconnects.
pub async fn serve_stdio() -> Result<(), ServeError> {
	let running = CadenzaServer::new()
		.serve(rmcp::transport::stdio())
		.await
		.map_err(Box::new)?;
	running.waiting().await?;
	Ok(())
}
