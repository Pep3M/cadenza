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

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
	CallToolRequestParams, CallToolResponse, NumberOrString, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{
	ErrorData, Json, Peer, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router,
};

use crate::error::ServeError;
use crate::player::PlayerHandle;
use crate::session::{Session, SharedSession};
use crate::tools::{
	arrange, audio, clips, compose, devices, midi, playback, portable, presets, project, render,
	samples, tracks,
};

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
Samples: the built-in `drum_sampler` (16 GM pads, starts with the factory 'Synth Kit') and \
`sampler` (multi-zone, starts with 'Sine Pad') play audio files: `list_factory_samples`, \
`load_sample` (pad or zone), `set_pad`, `set_zone`; their zones show in get_project.\n\
Devices have presets: `list_presets`/`load_preset` for a quick starting sound, \
`save_preset` to keep one for later. Duck a pad or bass under the kick with a compressor \
keyed by it (`set_sidechain`); give the separate outputs of a multi-output drum plugin \
their own buses with `set_output_routes`.\n\
5. Balance the mix with `set_channel` (volume/pan/mute/solo) and mark song sections with \
`add_marker`. Use `analyze_mix` to get loudness/peak/band readings plus plain-language \
mix observations before committing to a `render`. `play`/`stop` audition on the default \
output device; edits made while playing are pushed to the player automatically.\n\n\
Third-party plugins run in crash-isolated sandbox processes: a plugin that crashes or \
hangs is bypassed and the work continues. `render`/`analyze_mix` then name it in \
`warnings` (re-render to retry); during playback `transport_status` lists it under \
`plugin_crashes` and `restart_plugins` reloads it. Replace a plugin that keeps crashing.\n\n\
Audio and MIDI files: `import_audio` puts a recording on an Audio track (then \
`set_clip_fades`, `get_audio_info`); `import_midi`/`export_midi` read and write .mid files. \
`export_stems` writes per-track/bus stems (and the mixdown) with range, format, \
normalization and metadata. Project files: `list_missing_files`/`relink_files` fix moved \
audio, `collect_files` copies outside files into the project folder, `pack_project` zips it.\n\n\
Positions and durations are always in beats (quarter notes); pitches are note names \
(\"C4\") or MIDI numbers (60 = C4); velocity is 0..1; volume is dB; pan is -1..1.";

pub struct CadenzaServer {
	/// Possibly shared with other owners (the app): every tool locks it per call.
	session: SharedSession,
	player: Mutex<Option<PlayerHandle>>,
	tool_router: ToolRouter<Self>,
	/// Peer handed to tools called in-process (see [`CadenzaServer::call_tool_json`]).
	local_peer: OnceLock<Peer<RoleServer>>,
	local_request_id: AtomicU32,
}

/// One tool as advertised to an LLM: name, description and JSON Schema of its arguments.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
	pub name: String,
	pub description: String,
	pub input_schema: serde_json::Value,
}

/// Handler behind the detached in-process peer; it never receives a request.
struct InProcessPeerHandler;

impl ServerHandler for InProcessPeerHandler {}

impl CadenzaServer {
	pub fn new() -> Self {
		Self::with_session(Session::new())
	}

	/// A server around an existing session (e.g. one with a custom plugin cache).
	pub fn with_session(session: Session) -> Self {
		Self::with_shared_session(session.into_shared())
	}

	/// A server working on a session someone else also edits (the desktop app hands its
	/// session to the embedded AI agent this way). Each tool call locks the session only
	/// for its own duration; renders and analysis work on a copy.
	pub fn with_shared_session(session: SharedSession) -> Self {
		Self {
			session,
			player: Mutex::new(None),
			tool_router: Self::tool_router(),
			local_peer: OnceLock::new(),
			local_request_id: AtomicU32::new(1),
		}
	}

	/// Every tool this server exposes, sorted by name, for embedding the tools in an
	/// in-process agent without an MCP connection.
	pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
		self.tool_router
			.list_all()
			.into_iter()
			.map(|tool| ToolDefinition {
				name: tool.name.to_string(),
				description: tool.description.as_deref().unwrap_or_default().to_owned(),
				input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
			})
			.collect()
	}

	/// Calls a tool in-process through the same router the MCP transport uses, so the
	/// behavior (argument validation, error texts, structured output) is identical.
	///
	/// Returns the tool's structured JSON output (or `{"ok": true}` for tools without
	/// output); `Err` carries the LLM-readable error text. Must be called from within a
	/// tokio runtime.
	pub async fn call_tool_json(
		&self,
		name: &str,
		args: serde_json::Value,
	) -> Result<serde_json::Value, String> {
		let mut params = CallToolRequestParams::new(name.to_owned());
		match args {
			serde_json::Value::Object(map) => params = params.with_arguments(map),
			serde_json::Value::Null => {}
			other => {
				return Err(format!(
					"tool arguments must be a JSON object, got: {other}"
				));
			}
		}
		let id = self.local_request_id.fetch_add(1, Ordering::Relaxed);
		let context = ToolCallContext::new(
			self,
			params,
			RequestContext::new(NumberOrString::Number(id.into()), self.local_peer()),
		);
		match self.tool_router.call(context).await {
			Ok(CallToolResponse::Complete(result)) => {
				let text = result
					.content
					.iter()
					.filter_map(|c| c.as_text().map(|t| t.text.as_str()))
					.collect::<Vec<_>>()
					.join("\n");
				if result.is_error == Some(true) {
					return Err(text);
				}
				if let Some(structured) = result.structured_content {
					return Ok(structured);
				}
				if text.is_empty() {
					return Ok(serde_json::json!({ "ok": true }));
				}
				Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
			}
			Ok(_) => Err(format!(
				"tool '{name}' asked the client for more input, which in-process calls do not support"
			)),
			Err(err) if err.message == "tool not found" => Err(format!("unknown tool '{name}'")),
			Err(err) => Err(err.message.into_owned()),
		}
	}

	/// Tool handlers never talk back to the client, but the router's call context needs a
	/// peer; this one is detached (its service loop ends immediately).
	fn local_peer(&self) -> Peer<RoleServer> {
		self.local_peer
			.get_or_init(|| {
				let (io, _closed) = tokio::io::duplex(64);
				let running = rmcp::service::serve_directly(InProcessPeerHandler, io, None);
				running.peer().clone()
			})
			.clone()
	}

	/// The session the tools act on.
	pub fn shared_session(&self) -> SharedSession {
		Arc::clone(&self.session)
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
			let (project, plugins, hosting) = self.project_and_plugins();
			if let Some(p) = self.player().as_ref() {
				let _ = p.load(project, plugins, hosting);
			}
		}
	}

	/// The current project, the plugin catalog if the project uses plugins, and where
	/// plugins run.
	fn project_and_plugins(
		&self,
	) -> (
		cadenza_model::Project,
		Option<std::sync::Arc<cadenza_plugin_host::PluginCatalog>>,
		cadenza_engine::PluginHosting,
	) {
		let mut session = self.session();
		let plugins = session.plugins_for_project();
		let hosting = session.plugin_hosting().clone();
		(session.document().project().clone(), plugins, hosting)
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

	#[tool(
		description = "Saves the current project to disk as JSON. By default it first captures \
			every plugin's current state into the project (capture_plugin_states: true) so \
			reopening it sounds exactly the same; the capture is one undoable step."
	)]
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

	// -- presets / plugin state ---------------------------------------------------------

	#[tool(
		description = "Lists the presets of a device already in the project (track_id + \
			device_id from get_project; omit track_id for a master insert): factory presets \
			(built-in devices ship a few named starting points; plugins expose their own) and \
			user presets saved with save_preset. Plugins are loaded to list theirs, so this can \
			take a moment."
	)]
	fn list_presets(
		&self,
		Parameters(args): Parameters<presets::ListPresetsArgs>,
	) -> Result<Json<presets::ListPresetsOutput>, ErrorData> {
		Ok(Json(presets::list_presets(&mut self.session(), args)?))
	}

	#[tool(
		description = "Loads a preset (by name from list_presets) into a device in one undo \
			step. It replaces the device's explicit params (built-ins: the preset's values, \
			others at defaults) and, for plugins, its stored state. Tweak afterwards with \
			set_device_params."
	)]
	fn load_preset(
		&self,
		Parameters(args): Parameters<presets::LoadPresetArgs>,
	) -> Result<Json<presets::LoadPresetOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| presets::load_preset(s, args))?))
	}

	#[tool(
		description = "Saves a device's current settings (explicit params, plus the plugin's \
			full state for plugins) as a user preset named `name`, reusable in any project \
			with load_preset. Files live in the user preset folder (one JSON per preset)."
	)]
	fn save_preset(
		&self,
		Parameters(args): Parameters<presets::SavePresetArgs>,
	) -> Result<Json<presets::SavePresetOutput>, ErrorData> {
		Ok(Json(presets::save_preset(&mut self.session(), args)?))
	}

	#[tool(
		description = "Captures the current internal state of plugin devices (as the plugin \
			itself serializes it, including settings that are not exposed as params) and \
			stores it in the project in one undoable step. Pass device_id (+ track_id) for one \
			device, or nothing for every plugin. save_project does this automatically."
	)]
	fn capture_plugin_state(
		&self,
		Parameters(args): Parameters<presets::CapturePluginStateArgs>,
	) -> Result<Json<presets::CapturePluginStateOutput>, ErrorData> {
		Ok(Json(
			self.run_mut(|s| presets::capture_plugin_state(s, args))?,
		))
	}

	// -- routing ----------------------------------------------------------------------

	#[tool(
		description = "Feeds a device's sidechain (key) input from another track: e.g. a \
			compressor on the bass or pads keyed by the kick ducks them on every kick (EDM \
			pump, kick/bass separation); a de-esser or gate keyed by a guide track. track_id + \
			device_id select the keyed device (omit track_id for a master insert); \
			source_track_id is the trigger track (omit/null to disconnect). pre_fader: true \
			taps the source before its fader and mute, so a muted 'ghost' kick track can key \
			without being heard. Works with the built-in compressor (try its 'Sidechain Pump' \
			preset) and plugins with a sidechain input (see get_device_info). Delay \
			compensation keeps the key aligned; routes that would form a feedback loop are \
			rejected."
	)]
	fn set_sidechain(
		&self,
		Parameters(args): Parameters<tracks::SetSidechainArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_sidechain(s, args))
	}

	#[tool(
		description = "Chooses which extra outputs of a multi-output instrument plugin (e.g. a \
			drum sampler with separate kick/snare/hat outputs; see get_device_info aux_outputs) \
			are heard and where: each route sends output `port` (1-based) to a Bus track \
			(bus_track_id) for its own inserts and fader, or mixes it into the instrument's \
			track (no bus). The main output always plays on the track; unlisted extra outputs \
			are silent. Replaces the previous routes; [] = main output only."
	)]
	fn set_output_routes(
		&self,
		Parameters(args): Parameters<tracks::SetOutputRoutesArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| tracks::set_output_routes(s, args))
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
		description = "Replaces the automation curve for a parameter address (volume, pan, a send level or a device parameter); each point may set its shape (linear, hold, smooth). An empty points list removes the lane."
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

	// -- samplers ---------------------------------------------------------------------

	#[tool(
		description = "Lists the factory samples (synthesized drum hits and a sine multisample, \
			rendered on first use) with absolute paths for load_sample, each with its kind \
			(drum with its GM note, or tonal with its recorded pitch), plus the factory presets \
			built from them (load_preset on a sampler / drum_sampler)."
	)]
	fn list_factory_samples(&self) -> Result<Json<samples::ListFactorySamplesOutput>, ErrorData> {
		Ok(Json(samples::list_factory_samples()?))
	}

	#[tool(
		description = "Loads an audio file into the built-in sampler or drum_sampler of a track \
			(track_id; device_id defaults to the track's instrument). drum_sampler: pass pad \
			(kick, snare, clap, rim, closed_hat, pedal_hat, open_hat, low_tom, mid_tom, high_tom, \
			crash, ride, tambourine, cowbell, or a note like \"C2\"/36) — replaces that pad's \
			sample keeping its settings, or adds a layer with layer: true. sampler: with nothing \
			else the file is mapped across the whole keyboard at its root (from the file name \
			like \"Piano C3\", else detected pitch, else C4; override with root); zone: N \
			replaces one zone's sample; keys: \"C2-B3\" adds a zone on that range. path must be \
			absolute. One undo step; returns the zones."
	)]
	fn load_sample(
		&self,
		Parameters(args): Parameters<samples::LoadSampleArgs>,
	) -> Result<Json<samples::LoadSampleOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| samples::load_sample(s, args))?))
	}

	#[tool(
		description = "Edits one zone of a sampler or drum_sampler by index (see get_project \
			instrument zones): key range, root, velocity range (layers), tune, gain_db, pan, \
			start/end (seconds into the file; end 0 = file end), loop_mode off|forward|ping_pong \
			with loop_start/loop_end/loop_crossfade (seconds; a 0.02-0.1 s crossfade hides the \
			seam), reverse, attack_ms/decay_ms (decay 0 = ring out), choke group, output (0 \
			main, 1-8 extra outputs), round_robin group; remove: true deletes it. Omitted \
			fields keep their value. One undo step."
	)]
	fn set_zone(
		&self,
		Parameters(args): Parameters<samples::SetZoneArgs>,
	) -> Result<Json<samples::ZonesOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| samples::set_zone(s, args))?))
	}

	#[tool(
		description = "Shapes one drum_sampler pad (every layer on its note): gain_db, pan, \
			tune (semitones), attack_ms, decay_ms (short = tight, 0 = ring out), start (seconds, \
			skips the attack), reverse, choke (same group cuts each other: closed + open hat), \
			output (1-8 sends the pad to an extra output so it can get its own bus via \
			set_output_routes; it plays on the track until routed), clear: true unloads it. pad \
			is a drum name, note name or MIDI number. One undo step."
	)]
	fn set_pad(
		&self,
		Parameters(args): Parameters<samples::SetPadArgs>,
	) -> Result<Json<samples::ZonesOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| samples::set_pad(s, args))?))
	}

	// -- audio and MIDI files ----------------------------------------------------------

	#[tool(
		description = "Imports an audio file (WAV, AIFF, FLAC, MP3, OGG, M4A, CAF; any sample \
			rate) as a clip covering the whole file at at_beats, on an existing Audio track \
			(track_id) or on a new Audio track named after the file. One undo step. Returns the \
			clip, its length in beats and the file's duration, sample rate and channels."
	)]
	fn import_audio(
		&self,
		Parameters(args): Parameters<audio::ImportAudioArgs>,
	) -> Result<Json<audio::ImportAudioOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| audio::import_audio(s, args))?))
	}

	#[tool(
		description = "Describes an audio file (path) or the file of an audio clip (clip_id): \
			duration in seconds and beats, sample rate, channels, format; for a clip also its \
			position, offset, gain and fades."
	)]
	fn get_audio_info(
		&self,
		Parameters(args): Parameters<audio::GetAudioInfoArgs>,
	) -> Result<Json<audio::GetAudioInfoOutput>, ErrorData> {
		Ok(Json(audio::get_audio_info(&self.session(), args)?))
	}

	#[tool(
		description = "Sets the fade-in and/or fade-out of an audio clip, each as a length in \
			seconds or beats (0 removes it; omit a side to keep it) with a curve: linear, \
			equal_power (crossfades), exponential (long fade-outs) or s_curve. Use a few \
			milliseconds (0.005 s) to remove clicks at clip edges. The fades must fit in the clip."
	)]
	fn set_clip_fades(
		&self,
		Parameters(args): Parameters<audio::SetClipFadesArgs>,
	) -> Result<Json<audio::SetClipFadesOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| audio::set_clip_fades(s, args))?))
	}

	#[tool(
		description = "Imports a standard MIDI file (.mid) in one undo step: new instrument \
			tracks per source track (or per channel with split_by_channel; drums get \
			drum_machine, the rest poly_synth), or one clip on an existing instrument track \
			(track_id, at_beats). Also applies the file's tempo map, time signature and \
			markers. Returns the created tracks/clip and warnings about data that was dropped."
	)]
	fn import_midi(
		&self,
		Parameters(args): Parameters<midi::ImportMidiArgs>,
	) -> Result<Json<midi::ImportMidiOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| midi::import_midi(s, args))?))
	}

	#[tool(
		description = "Writes the project's MIDI to a standard MIDI file (.mid): every \
			instrument track with notes, or only track_id, or only one clip (clip_id). \
			Includes the tempo map, time signature and markers."
	)]
	fn export_midi(
		&self,
		Parameters(args): Parameters<midi::ExportMidiArgs>,
	) -> Result<Json<midi::ExportMidiOutput>, ErrorData> {
		Ok(Json(midi::export_midi(&self.session(), args)?))
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
			GM lane (kick, snare, clap, rim, closed_hat, pedal_hat, open_hat, low_tom, mid_tom, \
			high_tom, crash, ride, tambourine, cowbell, or any note name/MIDI number); plays on \
			drum_machine and drum_sampler alike (warnings name lanes a drum_sampler has no \
			sample for). Steps: X accent, x normal, o ghost, '.'/'-' rest; \
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

	#[tool(
		description = "Sets the time signature from a bar on (default bar 1 = the whole song). A later bar adds a time signature change (e.g. a 2/4 bar before a chorus, a 7/8 section); notes stay at their beats, bar lines move. get_project lists time_signature_changes."
	)]
	fn set_time_signature(
		&self,
		Parameters(args): Parameters<arrange::SetTimeSignatureArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| arrange::set_time_signature(s, args))
	}

	#[tool(
		description = "Removes the time signature change at a bar (>= 2); the signature before it continues."
	)]
	fn remove_time_signature(
		&self,
		Parameters(args): Parameters<arrange::RemoveTimeSignatureArgs>,
	) -> Result<(), ErrorData> {
		self.run_mut(|s| arrange::remove_time_signature(s, args))
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

	// -- portable projects -----------------------------------------------------------

	#[tool(
		description = "Copies every file the project uses from outside its folder (audio clips, \
			processed copies, sample zones; factory samples only with include_factory) into the \
			project folder (Audio/, Samples/), copying identical content once, and points the \
			references at the copies in one undo step. Needs a saved project. dry_run reports \
			the plan only. Run it before sharing or moving a project."
	)]
	fn collect_files(
		&self,
		Parameters(args): Parameters<portable::CollectFilesArgs>,
	) -> Result<Json<portable::CollectFilesOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| portable::collect_files(s, args))?))
	}

	#[tool(
		description = "Lists the files the project refers to that are missing on disk (audio \
			clips and sample zones), with their original names. Fix them with relink_files."
	)]
	fn list_missing_files(&self) -> Result<Json<Vec<portable::MissingFile>>, ErrorData> {
		Ok(Json(portable::list_missing_files(&self.session())))
	}

	#[tool(
		description = "Searches search_folder recursively for the project's missing files (by \
			original file name; ties go to the path whose folders match best) and relinks every \
			match at once, in one undo step. Reports what was relinked and what is still missing."
	)]
	fn relink_files(
		&self,
		Parameters(args): Parameters<portable::RelinkFilesArgs>,
	) -> Result<Json<portable::RelinkFilesOutput>, ErrorData> {
		Ok(Json(self.run_mut(|s| portable::relink_files(s, args))?))
	}

	#[tool(
		description = "Packs the saved project's folder into one archive (.cadenza.zip) for \
			sharing, with the current (even unsaved) project state; caches, autosaves, backups \
			and (unless include_renders) Renders/ are left out. Files outside the folder are \
			listed in 'outside': run collect_files first to include them."
	)]
	fn pack_project(
		&self,
		Parameters(args): Parameters<portable::PackProjectArgs>,
	) -> Result<Json<portable::PackOutput>, ErrorData> {
		Ok(Json(portable::pack_current(&self.session(), args)?))
	}

	// -- rendering / analysis --------------------------------------------------------

	#[tool(
		description = "Renders the project to a stereo audio file and returns the mix report \
			(levels, loudness, band energy, per track) plus the written file's levels. The path \
			extension picks the format: .wav (32-bit float, or bit_depth 16/24), .flac (24-bit, \
			or 16) or .mp3 (if this build supports it; mp3_bitrate_kbps). 16/24-bit output is \
			TPDF-dithered. Range: start_beat/end_beat or start_bar/end_bar (bars from 1, end bar \
			included); tail appends the effect decay. Optional output_sample_rate (resampled), \
			normalize (peak, or loudness LUFS with a true-peak ceiling) and metadata (title, \
			artist...). CPU-heavy; runs off the main session lock."
	)]
	async fn render(
		&self,
		Parameters(args): Parameters<render::RenderArgs>,
	) -> Result<Json<render::RenderOutput>, ErrorData> {
		let (project, plugins, hosting) = self.project_and_plugins();
		Ok(Json(render::render(project, plugins, hosting, args).await?))
	}

	#[tool(
		description = "Exports stems: one file per instrument/audio track (stems: tracks), per \
			bus and track feeding the master directly (buses: they sum to the mix), or per \
			listed track (selected + track_ids), optionally with the mixdown. Options: \
			through_master (run each stem through the master chain), pre_fader, range (beats or \
			bars), tail, format wav/flac/mp3 + bit_depth, output_sample_rate, normalize (stems \
			share the mix gain unless normalize_individually), metadata. Files are \
			\"<name> - <track>.<ext>\" in folder (default: the project's Renders/). CPU-heavy."
	)]
	async fn export_stems(
		&self,
		Parameters(args): Parameters<render::ExportStemsArgs>,
	) -> Result<Json<render::ExportStemsOutput>, ErrorData> {
		let (project, plugins, hosting) = self.project_and_plugins();
		let renders = self
			.session()
			.path()
			.map(|p| cadenza_model::ProjectLayout::new(p).renders_dir());
		Ok(Json(
			render::export_stems(project, plugins, hosting, args, renders).await?,
		))
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
		let (project, plugins, hosting) = self.project_and_plugins();
		Ok(Json(
			render::analyze_mix(project, plugins, hosting, args).await?,
		))
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
		let (project, plugins, hosting) = self.project_and_plugins();
		let guard = self.player();
		Ok(Json(playback::play(
			guard.as_ref().unwrap(),
			project,
			plugins,
			hosting,
			args,
		)?))
	}

	#[tool(description = "Stops playback.")]
	fn stop(&self) -> Result<Json<playback::TransportStatus>, ErrorData> {
		Ok(Json(playback::stop(self.player().as_ref())?))
	}

	#[tool(
		description = "Reports whether the transport is playing, its current beat position, and \
			any plugin that crashed or hung during playback (it is bypassed; see \
			restart_plugins)."
	)]
	fn transport_status(&self) -> Result<Json<playback::TransportStatus>, ErrorData> {
		Ok(Json(playback::transport_status(self.player().as_ref())?))
	}

	#[tool(
		description = "Restarts crashed plugins by reloading the current project into the \
			player (every plugin is re-created from the project's saved state; playback \
			position and play state are kept). Use it after transport_status reports \
			plugin_crashes; if the same plugin crashes again, replace or bypass it. Renders \
			and analyze_mix always start with fresh plugins and need no restart."
	)]
	fn restart_plugins(&self) -> Result<Json<playback::TransportStatus>, ErrorData> {
		let (project, plugins, hosting) = self.project_and_plugins();
		Ok(Json(playback::restart_plugins(
			self.player().as_ref(),
			project,
			plugins,
			hosting,
		)?))
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
