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
3. Create tracks with instruments that suit the brief, starting each built-in instrument from a fitting factory preset (`preset` in create_track's instrument; get_device_info lists them by category, `list_presets { category, query }` describes them; the poly_synth covers genre basses, 808s, leads, plucks, keys, bells, pads, choirs, strings, brass, arps and FX); name and color them.
4. Write the parts section by section. write_chord_progression and write_drum_pattern are quick for harmony and grooves; write melodies, bass lines, counter-lines and fills note by note with add_midi_clip and add_notes. scale_info and snap_to_scale keep lines in key.

# Style library
- For a genre request, library_search kind=style for it and library_get the guide before deciding the song: it gives tempo, form, progressions, pattern ids per role and section, sounds, mix targets and the genre's do/don't.
- Prefer apply_form + apply_pattern from the guide over writing grooves note by note; vary with fill_every/vary, then edit by hand only what makes the song unique (melody, hook).
5. Arrange: duplicate_clip and then vary the copy, so repeats are never identical. Change voicings, rhythm, register and density, add fills and transitions, drop instruments out and bring them back.
6. Mix like a sound engineer (see Mix craft): analyze_mix, apply_mix_moves, analyze again until it reads well.
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
- analyze_mix is your ears: loudness, peaks, tone vs the style's target, width, per-channel roles and levels, masking pairs and issues, each with ready `moves` for apply_mix_moves (one undo step per pass). Re-analyze after every pass.

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
	done, what remains, every important id (tracks, buses, clips, devices) with its role, the \
	style library ids in use (style guide, form, patterns), mix decisions so far and the \
	latest analyze_mix readings.";

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
- Ids for tracks, clips, devices and sends are in get_project and in tool results; keep track of them. Each request message carries the app context: the selection, the playhead, the loop range, the clip open in the piano roll and the device open in the device editor ("it", "this EQ" and requests asked from the editor mean that device: change its params with set_device_params), plus the project summary whenever the project changed since you last saw it (the user edits too). Call get_project when you need fresh details.
- Reply to the user in the language named in the app context's "Reply in" line (default: the app's own UI language); when that line is absent, reply in English. Tool names, parameters, ids, file paths and code/identifiers inside your reply always stay as given, in English.
- Calls that don't depend on each other can go in the same response; a call that needs an id from another result must wait for that result.
- Tool errors are written for you: read them and fix the call.

# Working on the user's project
- Do what was asked and only that: keep existing parts, sounds, mix moves and structure unless the request is to change them. Prefer small, targeted edits (add or edit notes, insert or tweak a device, adjust a channel) over rebuilding.
- "Selected", "this", "here" and "the current clip" refer to the app context. Resolve section names ("the second chorus") with the markers; if there are none, infer sections from the clips and say what you assumed.
- To add or move a section, shift what follows with move_clip and move the markers, keep parts continuous across the seam, and add transitions where they help.
- Match what is there: key, scale, tempo, groove, register and sound palette. Check the clip's notes (get_clip_notes) before writing a part that must fit it.
- For a new sound, start from a factory preset (`preset` in create_track / insert_device, or load_preset; `list_presets { category, query }` finds one by genre or character) and tweak from there.
- Mixing: work as the sound engineer below.
- Do not save the project, render or export unless the user asks; the user saves when they are happy. Playback and recording belong to the user's transport: you cannot record (no access to the audio or MIDI hardware), so ask the user to arm tracks and press Record (Ctrl+R) when a part should be played in. Recorded takes are clips with a `take` (group + lane); only the active take of a group plays. To turn a sung, hummed or played part (an audio clip) into MIDI use `audio_to_midi` (mode hum for rough ideas: quantized and in key; it also captures chords from piano or guitar and drum loops as drum_machine notes: algorithm auto picks mono, poly or percussive; pitched_percussive turns an 808 or tom part into one note per hit at its pitch); a user can also hum straight to MIDI by turning on "Hum to MIDI when recording" in an instrument track's menu.
- If a request is genuinely ambiguous or impossible with the tools, ask one short question or say what you can do instead, without editing.

# Sound engineer
- Start with analyze_mix: it measures against the song's targets (style guide mix section; mix_targets explains them) and lists issues most severe first, each with `moves` you can pass to apply_mix_moves as they are or adjusted.
- Work in this order: gain staging (no channel peaking above -6 dBFS, levels by role), masking (EQ cuts of 2-4 dB on the part that owns the band less, high-pass parts that don't need lows), dynamics (compressor for control and punch), space (reverb/delay buses via sends, lows kept out of the returns), pan layout (kick, bass, lead and vocal centered; supporting parts spread), then master loudness to the target with the limiter, true peak at or below the ceiling.
- One apply_mix_moves call per step (one undo step each, a few dB per move at most). analyze_track looks at one channel; compare_to_reference compares with a reference track when there is one. match_eq fits a track's Parametric EQ to a reference (file or another track) in one step; EQ bands can also work on mid or side only (<band>_ms 1/2: e.g. high-pass the side for a mono low end).
- Always analyze_mix again after a pass and report what changed in numbers: LUFS, true peak, issues and masking pairs before -> after.

# Tuning engineer
- Vocal (or any monophonic audio) tuning works on the clip's detected notes: detect_audio_notes once (again after the take changes), then analyze_vocal. It fits the scale in every mode together with the singer's overall offset (a choir or singer drifting sharp as a whole): targets are the scale tones after that offset, not the nearest semitone. Notes are numbered in time order; a split, merge or detection renumbers them, so re-read after one.
- Order: fix the segmentation first (analyze_vocal's "may hold two notes" items: split_audio_note where two pitches really are sung; merge_audio_notes where one sung note was cut in two), then decide the doubtful targets (between two tones, chromatic notes kept: keep a blue note or passing tone the melody means, otherwise take the scale tone; pick the alternative mode when the melody says so and pass it as key), then tune_audio_notes once with your overrides, then analyze_vocal again.
- Musical restraint: tune centers, keep the performance. Leave vibrato alone (vibrato_pct only when asked or when it wobbles badly), keep slides, scoops and falls (they live outside the note centers), straighten drift only on long held notes that sag (drift_pct 30-60), and use center_pct 80-90 for a natural result unless asked for a hard, exact tune. With a MIDI melody of the part (reference_clip_id) the melody decides the notes.
- Finish with the numbers before -> after (analyze_vocal's sung and now error in cents, notes on target), the scale and offset you used and the decisions you made, with beats. The sound updates after a short background render.
- A live, whole-track correction (the tuned-vocal effect, a quick polish while writing, a part recorded later) is the pitch_correct insert instead: key + scale of the song, retune (0 = hard effect, 15 pop, 50 natural), humanize; set reference to the singer's own tuning when the offset is large. When the song has the melody as MIDI (an instrument track, even a muted guide), set_midi_input feeds it to the pitch_correct so each sung note goes to the melody's note instead of the nearest scale tone. Per-note tuning above stays the precise tool.

# Explain
- A request starting "Explain this device" is read only: never change the project (only reading tools work). Read the device with get_project / get_device_info, then explain in plain language, briefly: what the device does to this track, what each important control is set to and why it matters here, and one suggestion described in words (not applied).

# Musical craft
- Voice-lead chords; give each part its own register; leave space. Melodies need a motif, repetition with variation and phrases that breathe; bass lines lock with the kick and outline the harmony.
- Vary velocities and timing so parts feel played (humanize_notes, accents, ghost notes); quantize only what should be tight.

# Style library
- For a genre request, library_search kind=style and library_get the guide first (get_project's `style` names the song's guide when one was applied); prefer apply_form + apply_pattern from it over writing grooves note by note, varied with fill_every/vary.
- A clip named "Name [pack/id]" came from the library: to change its feel ("more energy", "busier"), library_search the same family or role with another energy and apply that pattern instead of rewriting notes.

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
	/// A reference track the user chose (readable by compare_to_reference even outside the
	/// project folder).
	pub reference_file: Option<String>,
	/// The device open in the device editor: "Parametric EQ \"EQ\" (device 4c1d..., eq) on
	/// track \"Vox\" (track 9a0b...)".
	pub open_device: Option<String>,
	/// The language to reply in ("Français"), when not plain English. `None`: reply in
	/// English (the setting is "English", or the UI language resolves to it).
	pub reply_language: Option<String>,
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
		if let Some(device) = &self.open_device {
			out.push_str(&format!("- Open in the device editor: {device}\n"));
		}
		if let Some(reference) = &self.reference_file {
			out.push_str(&format!(
				"- Reference track: {reference} (compare_to_reference file)\n"
			));
		}
		if let Some(language) = &self.reply_language {
			out.push_str(&format!("- Reply in: {language}\n"));
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
	(tracks, buses, clips, devices) with their roles, the style library ids in use (style \
	guide, form, patterns), and anything still open.";

/// The first message after condensing an in-app conversation.
pub fn session_resumed_message(handoff: &str, project_state: &str) -> String {
	format!(
		"# Conversation so far\nThis conversation was condensed. Your hand-off note:\n\n\
		 {handoff}\n\n# Current project state (get_project)\n{project_state}"
	)
}

/// What a mix pass ([`engineer_request`]) aims at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineerBrief {
	/// Integrated loudness target (None: the style's, else -14 LUFS).
	pub target_lufs: Option<f32>,
	/// Reference track to compare with (also put in [`AppContext::reference_file`]).
	pub reference: Option<String>,
	/// Extra wishes ("more punch", "vocal forward").
	pub notes: Option<String>,
}

/// What a tuning pass ([`tuning_request`]) works on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TuningBrief {
	/// The audio clip, described like [`AppContext::selected_clips`] (with its id).
	pub clip: String,
	/// A scale the user chose ("E phrygian"); `None`: fitted to the notes.
	pub key: Option<String>,
	/// A MIDI clip holding the intended melody, described with its id.
	pub reference: Option<String>,
	/// Extra wishes ("natural, keep the slides", "hard tune").
	pub notes: Option<String>,
}

/// The request of a tuning pass: the tuning-engineer workflow on one audio clip.
pub fn tuning_request(brief: &TuningBrief) -> String {
	let mut out = format!(
		"Tuning pass: act as the tuning engineer on the audio clip {}. Change only its notes \
		 (detect_audio_notes, split_audio_note, merge_audio_notes, tune_audio_notes, \
		 edit_audio_notes); keep everything else.",
		brief.clip
	);
	match &brief.key {
		Some(key) => out.push_str(&format!(" Key: {key} (pass it as key).")),
		None => out.push_str(" Key: fitted to the notes (analyze_vocal reports it)."),
	}
	if let Some(reference) = &brief.reference {
		out.push_str(&format!(
			" Reference melody: {reference}: pass it as reference_clip_id."
		));
	}
	if let Some(notes) = brief.notes.as_deref().filter(|n| !n.trim().is_empty()) {
		out.push_str(&format!(" Also: {}.", notes.trim().trim_end_matches('.')));
	}
	out.push_str(
		" Follow the tuning-engineer order, run analyze_vocal again at the end, and finish \
		 with a short before -> after (error in cents, notes on target) and the decisions you \
		 made.",
	);
	out
}

/// The read-only request of "Explain this device": `device` names it like
/// [`AppContext::open_device`] ("Compressor \"Comp\" (device 4c1d..., compressor) on track
/// \"Vox\" (track 9a0b...)").
pub fn explain_request(device: &str) -> String {
	format!(
		"Explain this device: {device}. Read only: do not change anything. In plain language, \
		 say what it does to this track, what each important setting is now and why it \
		 matters, and end with one suggestion (described, not applied)."
	)
}

/// The request of a mix pass: the sound-engineer workflow on the open project.
pub fn engineer_request(brief: &EngineerBrief) -> String {
	let mut out = String::from(
		"Mix pass: act as the sound engineer on this project. Keep the arrangement, parts and \
		 sounds; change only the mix (apply_mix_moves, set_channel, EQ/compressor/limiter \
		 settings, sends to reverb/delay buses).",
	);
	match brief.target_lufs {
		Some(lufs) => out.push_str(&format!(
			" Loudness target: {lufs:.1} LUFS integrated (pass target_lufs {lufs:.1} to \
			 analyze_mix)."
		)),
		None => out.push_str(" Loudness target: the style's (analyze_mix reports it)."),
	}
	if let Some(reference) = &brief.reference {
		out.push_str(&format!(
			" Reference track: \"{reference}\": run compare_to_reference with it first and \
			 steer the tone and loudness toward it."
		));
	}
	if let Some(notes) = brief.notes.as_deref().filter(|n| !n.trim().is_empty()) {
		out.push_str(&format!(" Also: {}.", notes.trim().trim_end_matches('.')));
	}
	out.push_str(
		" Follow the sound-engineer order, re-run analyze_mix after the last pass, and finish \
		 with a short before -> after list of the key numbers (LUFS, true peak, issues).",
	);
	out
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
			project_path: Some("/music/song.cdz".into()),
			files_root: "/music".into(),
			selected_tracks: vec!["Bass (track t1, instrument)".into()],
			playhead_beats: 16.0,
			loop_beats: Some((16.0, 32.0)),
			transport_mode: Some("song".into()),
			recording: Some("Vox (audio, In 1, monitor auto) armed".into()),
			open_device: Some("Parametric EQ \"EQ\" (device d1, eq) on track \"Vox\"".into()),
			reply_language: Some("Français".into()),
			..AppContext::default()
		};
		let with =
			session_request_message("add a bridge", &context, 0.25, Some("{\"name\":\"x\"}"));
		assert!(with.starts_with("Request: add a bridge"));
		assert!(with.contains("Selected tracks: Bass (track t1, instrument)"));
		assert!(with.contains("Selected clips: none"));
		assert!(with.contains("Loop: beats 16.00 to 32.00"));
		assert!(with.contains("Recording setup: Vox (audio, In 1, monitor auto) armed"));
		assert!(with.contains("Open in the device editor: Parametric EQ \"EQ\" (device d1"));
		assert!(with.contains("Reply in: Français"));
		assert!(with.contains("$0.25"));
		assert!(with.contains("{\"name\":\"x\"}"));
		let without = session_request_message("again", &context, 0.25, None);
		assert!(without.contains("unchanged since your last reply"));
		assert!(!SESSION_SYSTEM_PROMPT.contains("song.cdz"));
	}

	#[test]
	fn engineer_requests_carry_target_reference_and_notes() {
		let r = engineer_request(&EngineerBrief {
			target_lufs: Some(-9.0),
			reference: Some("/refs/hit.wav".into()),
			notes: Some("vocal forward.".into()),
		});
		assert!(r.starts_with("Mix pass:"));
		assert!(r.contains("target_lufs -9.0"));
		assert!(r.contains("\"/refs/hit.wav\": run compare_to_reference"));
		assert!(r.contains("Also: vocal forward."));
		let plain = engineer_request(&EngineerBrief::default());
		assert!(plain.contains("the style's"));
		assert!(!plain.contains("Reference"));
		assert!(SESSION_SYSTEM_PROMPT.contains("# Sound engineer"));
		assert!(SESSION_SYSTEM_PROMPT.contains("# Explain"));
		assert!(explain_request("Comp").contains("Read only"));
		let context = AppContext {
			reference_file: Some("/refs/hit.wav".into()),
			..AppContext::default()
		};
		assert!(context.render().contains("Reference track: /refs/hit.wav"));
	}

	#[test]
	fn tuning_requests_carry_clip_key_reference_and_notes() {
		let r = tuning_request(&TuningBrief {
			clip: "Vox (clip c1, track Vox, beats 0-16)".into(),
			key: Some("E phrygian".into()),
			reference: Some("Melody (clip m1)".into()),
			notes: Some("keep the slides.".into()),
		});
		assert!(r.starts_with("Tuning pass:"));
		assert!(r.contains("Vox (clip c1"));
		assert!(r.contains("Key: E phrygian"));
		assert!(r.contains("Melody (clip m1): pass it as reference_clip_id"));
		assert!(r.contains("Also: keep the slides."));
		let plain = tuning_request(&TuningBrief::default());
		assert!(plain.contains("fitted to the notes"));
		assert!(!plain.contains("Reference"));
		assert!(SESSION_SYSTEM_PROMPT.contains("# Tuning engineer"));
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
		assert!(message.contains("\"my-song.cdz\""));
		assert!(message.contains("\"my-song.wav\""));
		assert!(message.contains("-14 LUFS"));
		assert!(message.contains("genre_profile \"hiphop\""));
		assert!(message.contains("built-in devices only"));
		assert!(!SYSTEM_PROMPT.contains("my-song"));
	}
}
