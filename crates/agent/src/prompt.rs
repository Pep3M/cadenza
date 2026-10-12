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

#[cfg(test)]
mod tests {
	use super::*;

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
