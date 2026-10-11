//! The MCP server: a thin `rmcp` adapter over the plain tool functions in `tools/*.rs`.
//!
//! Every tool method here does the same three things: lock the session, call the
//! matching function in `tools/`, and convert its `Result<T, McpError>` into
//! `Result<Json<T>, rmcp::ErrorData>` (or `Result<(), rmcp::ErrorData>`). All actual
//! logic — validation, command construction, music theory — lives in `tools/`.

use std::sync::Mutex;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, Json, ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::error::ServeError;
use crate::session::Session;
use crate::tools::{arrange, clips, compose, devices, project, tracks};

const INSTRUCTIONS: &str = "\
Cadenza: you are the arranger and mix engineer for this project. Everything you do goes \
through undoable commands, so experiment freely and use `undo`/`redo`/`history` to back \
out of mistakes.\n\n\
Typical workflow:\n\
1. `get_project` for the current state; `list_devices` to see available instruments and \
effects with their parameter ranges.\n\
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
`add_marker`.\n\n\
Positions and durations are always in beats (quarter notes); pitches are note names \
(\"C4\") or MIDI numbers (60 = C4); velocity is 0..1; volume is dB; pan is -1..1.";

pub struct CadenzaServer {
	session: Mutex<Session>,
	tool_router: ToolRouter<Self>,
}

impl CadenzaServer {
	pub fn new() -> Self {
		Self {
			session: Mutex::new(Session::new()),
			tool_router: Self::tool_router(),
		}
	}

	fn session(&self) -> std::sync::MutexGuard<'_, Session> {
		self.session
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
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
		Ok(Json(project::new_project(&mut self.session(), args)?))
	}

	#[tool(description = "Opens a project from a JSON file, replacing the one currently open.")]
	fn open_project(
		&self,
		Parameters(args): Parameters<project::OpenProjectArgs>,
	) -> Result<Json<cadenza_model::ProjectSummary>, ErrorData> {
		Ok(Json(project::open_project(&mut self.session(), args)?))
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
		Ok(Json(project::undo(&mut self.session())?))
	}

	#[tool(description = "Re-applies the last undone change.")]
	fn redo(&self) -> Result<Json<project::HistoryStep>, ErrorData> {
		Ok(Json(project::redo(&mut self.session())?))
	}

	#[tool(description = "Lists undoable and redoable steps, each with its human-readable label.")]
	fn history(&self) -> Json<project::HistoryOutput> {
		Json(project::history(&self.session()))
	}

	// -- devices ----------------------------------------------------------------------

	#[tool(
		description = "Lists built-in instruments and effects with their parameter ranges, \
			defaults and units, so you can pick and configure one before loading it."
	)]
	fn list_devices(
		&self,
		Parameters(args): Parameters<devices::ListDevicesArgs>,
	) -> Json<Vec<devices::DeviceOut>> {
		Json(devices::list_devices(args))
	}

	// -- tracks / mixer ----------------------------------------------------------------

	#[tool(
		description = "Creates a track (instrument, audio or bus), optionally with an instrument loaded."
	)]
	fn create_track(
		&self,
		Parameters(args): Parameters<tracks::CreateTrackArgs>,
	) -> Result<Json<tracks::CreateTrackOutput>, ErrorData> {
		Ok(Json(tracks::create_track(&mut self.session(), args)?))
	}

	#[tool(description = "Renames a track and/or changes its color.")]
	fn update_track(
		&self,
		Parameters(args): Parameters<tracks::UpdateTrackArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::update_track(&mut self.session(), args)?)
	}

	#[tool(description = "Deletes a track and everything on it (clips, devices, sends).")]
	fn delete_track(
		&self,
		Parameters(args): Parameters<tracks::DeleteTrackArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::delete_track(&mut self.session(), args)?)
	}

	#[tool(
		description = "Changes a channel's volume, pan, mute, solo and/or output. Omit track_id \
			to change the master channel."
	)]
	fn set_channel(
		&self,
		Parameters(args): Parameters<tracks::SetChannelArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::set_channel(&mut self.session(), args)?)
	}

	#[tool(description = "Adds a send from a track to a bus track (e.g. for shared reverb/delay).")]
	fn add_send(
		&self,
		Parameters(args): Parameters<tracks::AddSendArgs>,
	) -> Result<Json<tracks::AddSendOutput>, ErrorData> {
		Ok(Json(tracks::add_send(&mut self.session(), args)?))
	}

	#[tool(description = "Removes a send.")]
	fn remove_send(
		&self,
		Parameters(args): Parameters<tracks::RemoveSendArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::remove_send(&mut self.session(), args)?)
	}

	// -- devices on a channel -----------------------------------------------------------

	#[tool(
		description = "Inserts an effect into a channel's chain. Omit track_id to insert on \
			the master bus. Validates the device id and every parameter against list_devices."
	)]
	fn insert_device(
		&self,
		Parameters(args): Parameters<tracks::InsertDeviceArgs>,
	) -> Result<Json<tracks::InsertDeviceOutput>, ErrorData> {
		Ok(Json(tracks::insert_device(&mut self.session(), args)?))
	}

	#[tool(description = "Removes an insert effect from a channel.")]
	fn remove_device(
		&self,
		Parameters(args): Parameters<tracks::RemoveDeviceArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::remove_device(&mut self.session(), args)?)
	}

	#[tool(
		description = "Sets one or more parameters of a device (instrument or insert) in a \
			single undo step. Values are validated against the device's declared range."
	)]
	fn set_device_params(
		&self,
		Parameters(args): Parameters<tracks::SetDeviceParamsArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::set_device_params(&mut self.session(), args)?)
	}

	#[tool(description = "Bypasses or re-enables a device.")]
	fn set_bypass(
		&self,
		Parameters(args): Parameters<tracks::SetBypassArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::set_bypass(&mut self.session(), args)?)
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
		Ok(tracks::set_param(&mut self.session(), args)?)
	}

	#[tool(
		description = "Replaces the automation curve for a parameter address; an empty points list removes it."
	)]
	fn set_automation(
		&self,
		Parameters(args): Parameters<tracks::SetAutomationArgs>,
	) -> Result<(), ErrorData> {
		Ok(tracks::set_automation(&mut self.session(), args)?)
	}

	// -- clips / notes -------------------------------------------------------------------

	#[tool(
		description = "Creates a MIDI clip on an Instrument track, optionally with initial notes."
	)]
	fn add_midi_clip(
		&self,
		Parameters(args): Parameters<clips::AddMidiClipArgs>,
	) -> Result<Json<clips::AddMidiClipOutput>, ErrorData> {
		Ok(Json(clips::add_midi_clip(&mut self.session(), args)?))
	}

	#[tool(description = "Adds notes to an existing MIDI clip.")]
	fn add_notes(
		&self,
		Parameters(args): Parameters<clips::AddNotesArgs>,
	) -> Result<Json<clips::AddNotesOutput>, ErrorData> {
		Ok(Json(clips::add_notes(&mut self.session(), args)?))
	}

	#[tool(description = "Removes notes from a clip by id, or every note with all: true.")]
	fn remove_notes(
		&self,
		Parameters(args): Parameters<clips::RemoveNotesArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::remove_notes(&mut self.session(), args)?)
	}

	#[tool(
		description = "Transposes notes by a number of semitones. Omit note_ids to transpose the whole clip."
	)]
	fn transpose_notes(
		&self,
		Parameters(args): Parameters<clips::TransposeNotesArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::transpose_notes(&mut self.session(), args)?)
	}

	#[tool(
		description = "Snaps every note in a clip onto a rhythmic grid, with adjustable strength."
	)]
	fn quantize_notes(
		&self,
		Parameters(args): Parameters<clips::QuantizeNotesArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::quantize_notes(&mut self.session(), args)?)
	}

	#[tool(
		description = "Applies deterministic random timing and velocity variation to every \
			note in a clip; the same seed always produces the same result."
	)]
	fn humanize_notes(
		&self,
		Parameters(args): Parameters<clips::HumanizeNotesArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::humanize_notes(&mut self.session(), args)?)
	}

	#[tool(description = "Moves a clip to a new start beat and/or a different track.")]
	fn move_clip(
		&self,
		Parameters(args): Parameters<clips::MoveClipArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::move_clip(&mut self.session(), args)?)
	}

	#[tool(
		description = "Resizes a clip; notes that would start outside the new length are rejected."
	)]
	fn resize_clip(
		&self,
		Parameters(args): Parameters<clips::ResizeClipArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::resize_clip(&mut self.session(), args)?)
	}

	#[tool(description = "Deletes a clip and its notes.")]
	fn delete_clip(
		&self,
		Parameters(args): Parameters<clips::DeleteClipArgs>,
	) -> Result<(), ErrorData> {
		Ok(clips::delete_clip(&mut self.session(), args)?)
	}

	#[tool(
		description = "Duplicates a clip (with fresh ids for the clip and its notes) to a new position."
	)]
	fn duplicate_clip(
		&self,
		Parameters(args): Parameters<clips::DuplicateClipArgs>,
	) -> Result<Json<clips::DuplicateClipOutput>, ErrorData> {
		Ok(Json(clips::duplicate_clip(&mut self.session(), args)?))
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
		Ok(Json(compose::write_chord_progression(
			&mut self.session(),
			args,
		)?))
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
		Ok(Json(compose::write_drum_pattern(
			&mut self.session(),
			args,
		)?))
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
		Ok(compose::snap_to_scale(&mut self.session(), args)?)
	}

	// -- arrangement -------------------------------------------------------------------

	#[tool(description = "Sets the tempo from a beat onward (default: the whole song).")]
	fn set_tempo(
		&self,
		Parameters(args): Parameters<arrange::SetTempoArgs>,
	) -> Result<(), ErrorData> {
		Ok(arrange::set_tempo(&mut self.session(), args)?)
	}

	#[tool(description = "Sets the project's time signature.")]
	fn set_time_signature(
		&self,
		Parameters(args): Parameters<arrange::SetTimeSignatureArgs>,
	) -> Result<(), ErrorData> {
		Ok(arrange::set_time_signature(&mut self.session(), args)?)
	}

	#[tool(
		description = "Adds a song-section marker (Intro, Verse 1, Chorus...) at a beat position."
	)]
	fn add_marker(
		&self,
		Parameters(args): Parameters<arrange::AddMarkerArgs>,
	) -> Result<(), ErrorData> {
		Ok(arrange::add_marker(&mut self.session(), args)?)
	}

	#[tool(description = "Removes the marker at a beat position.")]
	fn remove_marker(
		&self,
		Parameters(args): Parameters<arrange::RemoveMarkerArgs>,
	) -> Result<(), ErrorData> {
		Ok(arrange::remove_marker(&mut self.session(), args)?)
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
