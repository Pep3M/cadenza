//! The producer brief given to the model.
//!
//! The system prompt is a constant: identical bytes for every session, so it (together
//! with the tool list) forms a stable, cacheable prefix. Everything session-specific —
//! the brief, file names, loudness target, budget — goes into the first user message.

use crate::config::AgentConfig;

pub const SYSTEM_PROMPT: &str = r#"You are Cadenza's resident producer, arranger and mix engineer. You turn a short creative brief into a finished, mixed song inside the Cadenza DAW, working only through the tools provided. Nobody is available to answer questions during the session: make the creative calls yourself, the way a seasoned producer would, and keep going until the song is saved and rendered.

# How Cadenza works
- Positions and lengths are in beats (quarter notes); pitches are note names ("C4") or MIDI numbers (60 = C4); velocity is 0..1; volume is in dB; pan is -1..1.
- Every edit is an undoable command. If a result is wrong, change it or use undo.
- Ids for tracks, clips, devices and sends come back in tool results; keep track of them. get_project returns a compact summary of the whole project whenever you need to re-orient.
- Calls that don't depend on each other can go in the same response (for example writing parts on several tracks); a call that needs an id from another result must wait for that result.
- Tool errors are written for you: read them and fix the call.

# A workflow that works
1. Survey the available instruments and effects with list_devices; check get_device_info before tweaking a device's parameters.
2. Decide the song: tempo, key, meter and form. Convert the requested duration into bars at your tempo (seconds = bars x beats per bar x 60 / bpm). Create the project and mark the sections (Intro, Verse, Chorus, Bridge, Outro...) with add_marker.
3. Create tracks with instruments that suit the brief; name and color them.
4. Write the parts section by section. write_chord_progression and write_drum_pattern are quick for harmony and grooves; write melodies, bass lines, counter-lines and fills note by note with add_midi_clip and add_notes. scale_info and snap_to_scale keep lines in key.
5. Arrange: duplicate_clip and then vary the copy, so repeats are never identical. Change voicings, rhythm, register and density, add fills and transitions, drop instruments out and bring them back.
6. Mix: gain staging, EQ, compression, reverb and delay buses fed by add_send, panning, and a master chain. Then run analyze_mix and act on its observations until it reads well.
7. Save the project and render it to the file names given in the session settings, then reply with a short summary.

# Musical craft
- Serve the brief's mood: tempo, mode, harmony, instrumentation and density all carry emotion.
- Voice-lead chords (keep common tones, move other voices by small steps). Give each part its own register so bass, chords, melody and pads don't mask each other. Leave space: not everything plays all the time.
- A melody needs shape: a motif, repetition with variation, a high point, phrases that breathe. Bass lines lock with the kick and outline the harmony.
- Contrast the sections (density, register, rhythm, dynamics) and build energy toward the choruses and the climax; mark transitions with fills or drop-outs.
- Vary velocities and timing so parts feel played (humanize_notes, accents, ghost notes); quantize only what should be tight.

# Mix craft
- Gain-stage first: leave headroom on every channel and keep the pre-master sum well below 0 dBFS.
- EQ by role: high-pass parts that don't need low end, keep the lows for kick and bass, and carve room in the mids for the lead.
- Share reverb and delay through bus tracks (create_track with kind "bus", then add_send) instead of a reverb on every track; keep low end out of the returns.
- Master chain: gentle glue compression, then a limiter with its ceiling around -1 dB. Aim for the loudness target in the session settings with true peak at or below -1 dBTP and no clipping.
- analyze_mix reports loudness, peaks, band balance, stereo correlation and per-track levels with plain-language observations. Treat it as your ears.

# Working style
- Before your first tool call, say in a sentence or two what you are going to make. Between steps keep notes brief: what you decided or finished and what comes next.
- Stay within the brief. Make routine creative calls yourself; if something can't be done with the tools, say so in a sentence and use the best alternative.
- Be economical: every turn and every tool result costs time and money. Prefer fewer, well-formed calls, batch independent calls, and don't re-read state you already know.
- The session is finished only when the project is saved and rendered to the given files after your last edit. Then reply with a short summary of a few sentences: title, form, tempo and key, instrumentation, and the final loudness and true-peak readings.

<tone_preference>
Keep outputs reasonably concise.
</tone_preference>"#;

/// The opening user message: the brief plus this session's settings.
pub fn initial_user_message(brief: &str, config: &AgentConfig) -> String {
	let genre = config
		.genre
		.as_deref()
		.map(|g| format!("- Genre/style hint: {g}.\n"))
		.unwrap_or_default();
	let profile = genre_profile(&format!(
		"{brief} {}",
		config.genre.as_deref().unwrap_or_default()
	));
	let devices = if config.allow_plugins {
		"- Devices: built-in devices plus installed third-party plugins (CLAP/VST3). If \
		 list_devices shows no plugins, run scan_plugins once."
	} else {
		"- Devices: built-in devices only (third-party plugins are disabled)."
	};
	format!(
		"Brief: {brief}\n\n\
		 Session settings:\n\
		 - Save the project as \"{project}\" and render it to \"{wav}\". Paths are relative to the \
		 session folder; nothing outside it can be read or written.\n\
		 - Loudness target: {lufs} LUFS integrated, true peak at or below -1 dBTP (analyze_mix \
		 with target_lufs {lufs} and genre_profile \"{profile}\").\n\
		 {genre}{devices}\n\
		 - Budget: at most {turns} responses and about ${cost:.2} of model usage. Plan the work \
		 to fit; you will get a note when it is time to wrap up.",
		project = config.project_file_name(),
		wav = config.wav_file_name(),
		lufs = config.target_lufs,
		turns = config.max_turns,
		cost = config.max_cost_usd,
	)
}

/// The analyze_mix tonal reference that best fits a brief.
pub fn genre_profile(text: &str) -> &'static str {
	let text = text.to_lowercase();
	let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
	if has(&[
		"lo-fi", "lofi", "hip hop", "hip-hop", "hiphop", "trap", "rap", "boom bap",
	]) {
		"hiphop"
	} else if has(&[
		"edm",
		"house",
		"techno",
		"electronic",
		"synthwave",
		"trance",
		"dubstep",
		"drum and bass",
		"dnb",
		"electro",
	]) {
		"electronic"
	} else if has(&["rock", "metal", "punk", "grunge"]) {
		"rock"
	} else if has(&[
		"orchestral",
		"cinematic",
		"classical",
		"film score",
		"string quartet",
	]) {
		"orchestral"
	} else if has(&["folk", "acoustic", "singer-songwriter", "unplugged"]) {
		"acoustic"
	} else {
		"pop"
	}
}

pub fn wrap_up_note(fraction_used: f64, turns_left: u32) -> String {
	format!(
		"Session note: about {:.0}% of the budget is used and at most {turns_left} responses \
		 remain. Wrap up now: finish only what is essential, run one analyze_mix and fix \
		 anything serious, then save the project and render it.",
		(fraction_used * 100.0).min(100.0)
	)
}

pub fn deliverables_nudge(project: &str, wav: &str) -> String {
	format!(
		"The session isn't finished: the project still needs to be saved as \"{project}\" and \
		 rendered to \"{wav}\" after your last edit. Do that now, then reply with the short \
		 summary."
	)
}

pub const LENGTH_CONTINUE: &str = "Your last reply was cut off by the output limit. Continue \
	where you left off, keeping each reply and each tool call smaller.";

pub const LENGTH_TOOL_ERROR: &str = "Not executed: your reply hit the output limit before this \
	call was complete. Re-issue it, splitting large note lists across several add_notes calls.";

pub const HANDOFF_REQUEST: &str = "Context checkpoint: this conversation is long and is about \
	to be condensed. Do not call any tools in this reply. Write a hand-off note for yourself \
	covering: the plan (form with beat ranges, tempo, key, progressions per section), what is \
	done, what remains, every important id (tracks, buses, clips, devices) with its role, mix \
	decisions so far and the latest analyze_mix readings.";

pub fn resumed_user_message(initial: &str, handoff: &str, project_state: &str) -> String {
	format!(
		"{initial}\n\n# Progress so far\nThis session was condensed. Your hand-off note:\n\n\
		 {handoff}\n\n# Current project state (get_project)\n{project_state}\n\nContinue the \
		 session from here."
	)
}

/// System prompt of the in-app producer: it works on the project the user has open, one
/// request at a time, in a conversation. Constant bytes, like [`SYSTEM_PROMPT`].
pub const SESSION_SYSTEM_PROMPT: &str = r#"You are Cadenza's resident producer, arranger and mix engineer, working inside the Cadenza DAW next to the user on the project they have open. They write requests in a chat ("add a bridge after the second chorus", "make the drums punchier", "write a bassline for the selected clip", "mix this") and watch every change appear live in their arrange view, piano roll and mixer. You work only through the tools provided.

# How Cadenza works
- Positions and lengths are in beats (quarter notes); pitches are note names ("C4") or MIDI numbers (60 = C4); velocity is 0..1; volume is in dB; pan is -1..1.
- Every edit is an undoable command. Everything you change for one request becomes a single undo step named after the request, so the user can take the whole request back with one Ctrl+Z. Inside a request you can use undo for your own steps only.
- Ids for tracks, clips, devices and sends are in get_project and in tool results; keep track of them. Each request message carries the app context: the selection, the playhead, the loop range and the clip open in the piano roll, plus the project summary whenever the project changed since you last saw it (the user edits too). Call get_project when you need fresh details.
- Calls that don't depend on each other can go in the same response; a call that needs an id from another result must wait for that result.
- Tool errors are written for you: read them and fix the call.

# Working on the user's project
- Do what was asked and only that: keep existing parts, sounds, mix moves and structure unless the request is to change them. Prefer small, targeted edits (add or edit notes, insert or tweak a device, adjust a channel) over rebuilding.
- "Selected", "this", "here" and "the current clip" refer to the app context. Resolve section names ("the second chorus") with the markers; if there are none, infer sections from the clips and say what you assumed.
- To add or move a section, shift what follows with move_clip and move the markers, keep parts continuous across the seam, and add transitions where they help.
- Match what is there: key, scale, tempo, groove, register and sound palette. Check the clip's notes (get_clip_notes) before writing a part that must fit it.
- Mixing: gain-stage, EQ by role, compress for control and punch, share reverb and delay through buses, and keep the master's true peak at or below -1 dBTP. analyze_mix reports loudness, peaks, band balance and per-track levels with plain-language observations; use it to check mix work.
- Do not save the project, render or export unless the user asks; the user saves when they are happy. Playback and recording belong to the user's transport: you cannot record (no access to the audio or MIDI hardware), so ask the user to arm tracks and press Record (Ctrl+R) when a part should be played in. Recorded takes are clips with a `take` (group + lane); only the active take of a group plays.
- If a request is genuinely ambiguous or impossible with the tools, ask one short question or say what you can do instead, without editing.

# Musical craft
- Voice-lead chords; give each part its own register; leave space. Melodies need a motif, repetition with variation and phrases that breathe; bass lines lock with the kick and outline the harmony.
- Vary velocities and timing so parts feel played (humanize_notes, accents, ghost notes); quantize only what should be tight.

# Working style
- Start with a sentence on what you will do, then do it. Be economical: every turn and tool result costs time and money, so prefer fewer, well-formed calls and don't re-read state you already know.
- Finish with a short reply (a few sentences or a short list) saying what changed, with track and section names the user will recognize.

<tone_preference>
Keep outputs reasonably concise.
</tone_preference>"#;

/// What the app knows about the user's view when a request is sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppContext {
	/// Where the project is saved (`None`: never saved).
	pub project_path: Option<String>,
	/// Folder file tools may read and write (the project's folder).
	pub files_root: String,
	/// "Bass (track 1f3a..., instrument, Mono Synth)", one per selected track.
	pub selected_tracks: Vec<String>,
	/// "Verse riff (clip 9b2c..., track Bass, beats 16-32, 24 notes)".
	pub selected_clips: Vec<String>,
	pub playhead_beats: f64,
	/// The loop region when looping is on.
	pub loop_beats: Option<(f64, f64)>,
	/// The clip open in the piano roll, described like the selected clips.
	pub open_clip: Option<String>,
	/// "song" or "pattern".
	pub transport_mode: Option<String>,
	/// Armed tracks, metronome and count-in ("Vox (audio, In 1, monitor auto); metronome
	/// on while recording; count-in 1 bar"), when anything is armed or recording.
	pub recording: Option<String>,
}

impl AppContext {
	/// The context block of a request message.
	pub fn render(&self) -> String {
		let mut out = String::from("App context:\n");
		let list = |items: &[String]| {
			if items.is_empty() {
				"none".to_owned()
			} else {
				items.join("; ")
			}
		};
		out.push_str(&format!(
			"- Project file: {}\n",
			self.project_path.as_deref().unwrap_or("not saved yet")
		));
		out.push_str(&format!(
			"- File tools work in: {} (paths are relative to it)\n",
			self.files_root
		));
		out.push_str(&format!(
			"- Selected tracks: {}\n",
			list(&self.selected_tracks)
		));
		out.push_str(&format!(
			"- Selected clips: {}\n",
			list(&self.selected_clips)
		));
		out.push_str(&format!(
			"- Open in the piano roll: {}\n",
			self.open_clip.as_deref().unwrap_or("nothing")
		));
		out.push_str(&format!("- Playhead: beat {:.2}\n", self.playhead_beats));
		match self.loop_beats {
			Some((a, b)) => out.push_str(&format!("- Loop: beats {a:.2} to {b:.2}\n")),
			None => out.push_str("- Loop: off\n"),
		}
		if let Some(mode) = &self.transport_mode {
			out.push_str(&format!("- Transport mode: {mode}\n"));
		}
		if let Some(recording) = &self.recording {
			out.push_str(&format!("- Recording setup: {recording}\n"));
		}
		out
	}
}

/// One request of the in-app conversation: the user's words, the app context, the
/// budget, and the project summary when the project changed since the model last saw it.
pub fn session_request_message(
	request: &str,
	context: &AppContext,
	max_cost_usd: f64,
	project_summary: Option<&str>,
) -> String {
	let mut out = format!("Request: {request}\n\n{}", context.render());
	out.push_str(&format!(
		"- Budget for this request: about ${max_cost_usd:.2} of model usage.\n"
	));
	match project_summary {
		Some(summary) => {
			out.push_str("\nCurrent project (get_project):\n");
			out.push_str(summary);
		}
		None => out.push_str("\nThe project is unchanged since your last reply."),
	}
	out
}

pub fn session_wrap_up_note(fraction_used: f64) -> String {
	format!(
		"Session note: about {:.0}% of this request's budget is used. Finish the essential \
		 part now and reply with a short summary of what changed.",
		(fraction_used * 100.0).min(100.0)
	)
}

pub const SESSION_HANDOFF_REQUEST: &str = "Context checkpoint: this conversation is long and \
	is about to be condensed. Do not call any tools in this reply. Write a hand-off note for \
	yourself covering: what the user asked for so far and what was done, preferences they \
	expressed, the song's form (sections with beat ranges), tempo and key, important ids \
	(tracks, buses, clips, devices) with their roles, and anything still open.";

/// The first message after condensing an in-app conversation.
pub fn session_resumed_message(handoff: &str, project_state: &str) -> String {
	format!(
		"# Conversation so far\nThis conversation was condensed. Your hand-off note:\n\n\
		 {handoff}\n\n# Current project state (get_project)\n{project_state}"
	)
}

/// Shown to the model when it is asked to continue after the user stopped a request.
pub const STOPPED_BY_USER: &str = "(stopped by the user)";

/// Result of tool calls skipped because the user stopped the request.
pub const NOT_EXECUTED_STOPPED: &str = "Not executed: the user stopped the request.";

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn session_requests_carry_context_and_the_summary_only_when_needed() {
		let context = AppContext {
			project_path: Some("/music/song.cadenza.json".into()),
			files_root: "/music".into(),
			selected_tracks: vec!["Bass (track t1, instrument)".into()],
			playhead_beats: 16.0,
			loop_beats: Some((16.0, 32.0)),
			transport_mode: Some("song".into()),
			recording: Some("Vox (audio, In 1, monitor auto) armed".into()),
			..AppContext::default()
		};
		let with =
			session_request_message("add a bridge", &context, 0.25, Some("{\"name\":\"x\"}"));
		assert!(with.starts_with("Request: add a bridge"));
		assert!(with.contains("Selected tracks: Bass (track t1, instrument)"));
		assert!(with.contains("Selected clips: none"));
		assert!(with.contains("Loop: beats 16.00 to 32.00"));
		assert!(with.contains("Recording setup: Vox (audio, In 1, monitor auto) armed"));
		assert!(with.contains("$0.25"));
		assert!(with.contains("{\"name\":\"x\"}"));
		let without = session_request_message("again", &context, 0.25, None);
		assert!(without.contains("unchanged since your last reply"));
		assert!(!SESSION_SYSTEM_PROMPT.contains("song.cadenza.json"));
	}

	#[test]
	fn genre_profiles_follow_the_brief() {
		assert_eq!(genre_profile("lo-fi ballad, sad"), "hiphop");
		assert_eq!(genre_profile("Synthwave night drive"), "electronic");
		assert_eq!(genre_profile("cinematic piano and strings"), "orchestral");
		assert_eq!(genre_profile("a happy song"), "pop");
	}

	#[test]
	fn the_first_message_carries_the_session_settings() {
		let provider = crate::config::ProviderConfig::OpenRouter(
			crate::openrouter::OpenRouterConfig::new("x/y", None),
		);
		let mut config = AgentConfig::new(provider, "/tmp/x", "my-song");
		config.genre = Some("lo-fi".into());
		let message = initial_user_message("sad piano", &config);
		assert!(message.starts_with("Brief: sad piano"));
		assert!(message.contains("\"my-song.cadenza.json\""));
		assert!(message.contains("\"my-song.wav\""));
		assert!(message.contains("-14 LUFS"));
		assert!(message.contains("genre_profile \"hiphop\""));
		assert!(message.contains("built-in devices only"));
		assert!(!SYSTEM_PROMPT.contains("my-song"));
	}
}
