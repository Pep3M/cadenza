# Cadenza — project contract

Open-source DAW + audio editor in Rust whose differentiator is an AI that works as
arranger + sound engineer: it writes MIDI, chooses and inserts instruments/effects,
mixes, automates and renders. The AI is **just another client of the same command API
the UI uses** — never a special path.

Design reference: an analysis of Studio One 5 (modular class factories, EditTasks with
typed parameters, undoable `functions.*`, URI-addressed params, project = archive of
text documents). We take ideas, never code or assets (clean-room).

## Workspace layout

| Crate | Role | Depends on |
|---|---|---|
| `cadenza-core` | ids, `Tick`/`PPQ`(960), `TempoMap`, `TimeSignature`, `ParamAddress` URIs, dB helpers | — |
| `cadenza-model` | project document, `Command` enum, undo/redo `Document`, persistence, AI summary | core |
| `cadenza-theory` | notes, scales, chord symbols, roman numerals, voicings | core |
| `cadenza-dsp` | real-time `Processor` trait + built-in instruments/effects registry | — |
| `cadenza-audio-io` | audio decoding (symphonia), rubato resampling, `AudioCache`, waveform peaks, realtime clip DSP (fades/regions), export WAV/FLAC (+MP3 `mp3` feature) with metadata (`tags`: WAV LIST/INFO, FLAC Vorbis comments, MP3 ID3v2.4), stretch (`stretch` feature) | — |
| `cadenza-library` | style library: packs of style guides, forms, progressions, grooves, MIDI patterns (embedded `core` pack + user packs), validator, search, `realize` | theory |
| `cadenza-analysis` | offline mix analysis: levels/BS.1770 loudness/true peak/bands (`Analyzer`, `LevelStats`), FFT third-octave spectrum + stereo image + octave timeline (`detail`), masking, issues with fix moves, mix targets, roles, reference comparison | core |
| `cadenza-midi-io` | SMF import (-> undoable `Batch`) / export, hardware MIDI input (midir -> rtrb), recorder | core, model, theory, dsp |
| `cadenza-engine` | compiles a `Project` into a render graph; offline render/export (mixdown + stems jobs with progress/cancel), mix analysis (`analyze_mix`, `analyze_mix_detailed`), realtime playback (cpal) + live MIDI monitoring, audio input + input monitoring, recording (WAV takes, MIDI), metronome + count-in | core, model, dsp, audio-io, midi-io, analysis |
| `cadenza-mcp` | MCP server (rmcp, stdio) exposing the command API as AI tools | model, theory, engine, dsp, audio-io, midi-io |
| `cadenza-plugin-host` | CLAP (clack) + VST3 hosting, out-of-process scanner, catalog, per-plugin sandbox process (shm + futex), plugin editor windows (X11) | dsp |
| `cadenza-agent` | embedded AI producer: provider-agnostic `LlmProvider` tool loop (OpenRouter + generic OpenAI-compatible), budget cap, JSONL transcript; `Agent` (compose from a brief) and `SessionAgent` (in-app chat on a shared session) | mcp |
| `cadenza-cli` | `cadenza` binary: `render`, `export` (mixdown/stems), `collect`, `relink`, `pack`, `unpack`, `analyze`, `play` (`--midi-in --monitor`), `monitor`, `midi-ports`, `import-midi`, `export-midi`, `mcp`, `scan-plugins`, `list-plugins`, `compose`, `chat` (one AI request on a project), `mix` (AI sound-engineer pass: `--target-lufs`, `--reference`, `--notes`, `--dry-run`, `-o`; analysis before/after + changed numbers), `library list|validate|import` (style library packs; MIDI -> pattern), `gui` (starts `cadenza-app`) | all |
| `cadenza-app` | `cadenza-app` desktop GUI (egui/eframe, wgpu): transport, arrange, piano roll, mixer with live meters, browser (devices/presets/files), inspector, AI producer chat; `AppState` over the shared MCP `Session` | core, model, theory, dsp, engine, mcp, agent, plugin-host, audio-io, midi-io |
| `cadenza-test-plugins` | tiny CLAP/VST3 plugins for tests (gain, sine, crash; routing: sidechain ducker with presets, multi-output; `clap_gui` scripted editor; VST3 gain with a scripted `IPlugView`) | — |

Future: sandbox and plugin editors on Windows/macOS, native Wayland plugin GUIs.

Cargo features `stretch` (Signalsmith, C++/libclang) and `mp3` (LAME, LGPL) live in
`cadenza-audio-io` and are forwarded by `cadenza-engine` and `cadenza-cli`; both are OFF by
default (`MP3_SUPPORTED` tells whether a build can write MP3).

AI agent rule: never depend on a single LLM vendor's API — new AI features go behind
`LlmProvider`. Secrets come from the environment, an ignored `.env` or the OS keyring (app);
never log or commit them.

Sound engineer (phase 18b): both system prompts carry a "Sound engineer" section (analyze ->
gain staging -> masking EQ -> dynamics -> space -> pan -> master loudness; one apply_mix_moves
per step; re-analyze and report before -> after). `SessionAgent::run_engineer_pass(&
EngineerBrief { target_lufs, reference, notes }, ...)` sends `prompt::engineer_request` as ONE
undo step `ENGINEER_UNDO_LABEL` ("AI mix pass"); `AppContext.reference_file` is readable by
`compare_to_reference` through `PathPolicy::allow_read` even outside the project folder.
Agent modes (`cadenza-agent`): `Agent::run(brief)` composes a whole song into a folder
(`cadenza compose`). `SessionAgent::run_request(request, &AppContext, observer, &CancelToken)`
works on a `SharedSession` (app chat, `cadenza chat`): `SESSION_SYSTEM_PROMPT` (incremental
edits, no save/render unless asked), request message = request + app context (selection,
playhead, loop, open clip, files root, armed tracks/metronome/count-in — recording itself stays with the
user: no recording tools) + `get_project` summary only when the project changed
since the model last saw it; append-only history across requests (prompt caches stay warm),
condensed into a hand-off note past `compact_at_input_tokens` (default 60k); each request is
one undo group; `new_project`/`open_project`/playback tools hidden; `PathPolicy` root = project
folder; per-request budget + session total; failures end the request with
`actionable_error` text and keep the conversation usable. Providers: `ProviderConfig::
OpenRouter` and `OpenAiCompatible` share the Chat Completions code (`openrouter.rs`,
`ApiFlavor`); OpenRouter-only fields: `usage.include`, `provider.require_parameters`,
`reasoning.effort`, `cache_control`; the generic flavor sends `stream_options.include_usage`
and `reasoning_effort`, key optional. Streams emit `TextDelta`/`ThinkingDelta` events.

## Non-negotiable rules

- **Audio thread is sacred**: inside `Processor::process` and the engine's realtime callback:
  no allocation, no locks, no I/O, no panics on valid input. Pre-allocate in `prepare`.
- **All project mutations go through `cadenza_model::Command`** executed by `Document`
  (undoable). No crate mutates `Project` fields directly outside `cadenza-model`.
- Commands carry every id they create (ids generated by the caller) so redo is deterministic.
- AI-facing APIs speak **beats (f64 quarter notes)** and note names; internals speak `Tick`.
- Every AI-exposed operation validates input and returns actionable error messages
  (the error text is read by an LLM).
- Errors: `thiserror` in libraries, `anyhow` only in `cadenza-cli`.
- Public types that cross to the AI derive `Serialize, Deserialize, JsonSchema`.

## Conventions

- Rust 2024, `hard_tabs = true` (rustfmt.toml). Run `cargo fmt` and `cargo clippy --workspace --all-targets` clean.
- English for code, comments, commits (conventional commits: feat/fix/refactor/test/chore/docs).
- Comments only where the *why* is not obvious. No placeholder/TODO stubs in delivered code.
- Unit tests next to the code; integration tests in `crates/<name>/tests/`.
- Add dependencies through `[workspace.dependencies]` in the root `Cargo.toml`, then
  `dep.workspace = true` in the crate. Only touch your own crate plus that table.
- Branches: `main` ← `dev` ← `feat/*`.

## Crate contracts

### cadenza-model

```rust
pub struct Project {
	pub format_version: u32,           // 1
	pub name: String,
	pub tempo: TempoMap,
	pub time_signature: TimeSignature, // bar 1
	pub time_signature_changes: Vec<MeterChange { bar: u32 /* 0-based index >= 1 */, numerator, denominator }>, // optional in JSON
	pub tracks: Vec<Track>,            // display order
	pub master: Channel,
	pub automation: Vec<AutomationLane>,
	pub markers: Vec<Marker>,          // song sections: Intro, Verse 1, Chorus...
}
pub enum TrackKind { Instrument, Audio, Bus, Folder }
pub struct Track { id: TrackId, name, kind, color: Option<String /* #RRGGBB */>,
	instrument: Option<Device>, clips: Vec<Clip>, channel: Channel,
	folder: Option<TrackId> /* the folder track holding it; optional */,
	freeze: Option<Freeze { file: PathBuf /* stereo render from tick 0 */ }> /* optional */,
	input: Option<TrackInput { channel: u16 /* 0-based */, stereo: bool /* channel+1 */, monitor: MonitorMode /* off|auto (default)|on */ }> /* audio tracks */ }
pub struct Channel { volume_db: f32, pan: f32 /* -1..1 */, mute: bool, solo: bool,
	inserts: Vec<Device>, sends: Vec<Send>, output: Output }
pub enum Output { Master, Bus(TrackId) }
pub struct Send { id: DeviceId, target: TrackId /* Bus track */, level_db: f32, pre_fader: bool }
pub struct Device { id: DeviceId, kind: DeviceKind, name: String, bypass: bool,
	params: BTreeMap<String, f32> /* plain (unnormalized) values by param id */,
	state: Option<String> /* opaque plugin state, base64; params apply on top */,
	sidechain: Option<SidechainSource { track: TrackId, pre_fader: bool }> /* key input */,
	aux_outputs: Vec<OutputRoute { port: u32 /* 1-based extra output */, bus: Option<TrackId> /* None = own track */ }>,
	zones: Vec<SampleZone> /* built-in sampler / drum_sampler only */ }
pub struct SampleZone { sample: SampleRef { path: PathBuf /* absolute in memory */, name: String /* original file name */ },
	key_low, key_high, root_key: u8, vel_low, vel_high: f32 /* 0..1 */, tune: f32 /* semitones */, gain_db, pan,
	start: f64, end: Option<f64> /* seconds of the file */, loop_mode: LoopMode /* off|forward|ping_pong */,
	loop_start: f64, loop_end: Option<f64>, loop_crossfade: f64, reverse: bool, attack_ms: f32, decay_ms: Option<f32>,
	choke: u8 /* 1..=16 */, output: u8 /* 0 main, n = extra output n */, round_robin: u8 /* 1..=16 */ }
// New fields are optional in JSON (missing = None/empty), so format_version stays 1.
pub enum DeviceKind { Builtin { id: String }, Clap { plugin_id: String, path: PathBuf },
	Vst3 { class_id: String, path: PathBuf } }
pub struct Clip { id: ClipId, name: String, start: Tick, length: Tick, content: ClipContent,
	take: Option<Take { group: TakeGroupId, lane: u32 /* 0 = first take */, active: bool }> }
pub enum ClipContent { Midi { notes: Vec<Note>, controllers: Vec<ControllerLane { controller: Controller /* "cc0".."cc127" | "pitch_bend" | "channel_pressure" */,
	points: Vec<ControllerPoint { tick /* clip-relative */, value: f32 /* CC/pressure 0..1, bend -1..1 */ }> }> /* optional */ }, Audio { file: PathBuf, offset_seconds: f64, gain_db: f32,
	fade_in: Option<ClipFade>, fade_out: Option<ClipFade> } }
pub struct ClipFade { seconds: f64, curve: FadeCurve /* linear (default) | equal_power | exponential | s_curve */ }
pub struct Note { id: NoteId, pitch: u8, velocity: f32 /* 0..1 */, start: Tick /* relative to clip */, length: Tick }
pub struct AutomationLane { address: ParamAddress, points: Vec<AutomationPoint { tick: Tick, value: f32,
	shape: PointShape /* linear (default, omitted in JSON) | hold | smooth: segment to the next point */ }> }
pub struct Marker { tick: Tick, name: String }
```

`Command` is a serde-tagged enum (`#[serde(tag = "op", rename_all = "snake_case")]`):
AddTrack, RemoveTrack, MoveTrack { track, index } (display order only), UpdateTrack (name/color/input
patch), SetChannel (volume/pan/mute/solo/output patch, `track: Option<TrackId>` = None means
master), SetInstrument, InsertDevice, RemoveDevice,
MoveDevice, SetDeviceBypass, SetParam { address, value }, AddSend, RemoveSend, AddClip,
RemoveClip, UpdateClip, AddNotes, RemoveNotes, UpdateNotes, SetTempo, RemoveTempoPoint,
SetTimeSignature { time_signature, bar /* 0-based index, 0 = bar 1, omitted when 0 */ },
RemoveTimeSignature { bar }, SetTrackFreeze { track, freeze: Option<Freeze> } (instrument/audio only), SetTrackFolder { track, folder: Option }, SetControllerLane { clip, controller, points } (replace; empty removes), SetAutomation (replace lane; empty points removes it), AddMarker, RemoveMarker,
SetDeviceState { track, device, state, params: Option<map> /* Some = replace all params */ },
SetSampleZones { track, device, zones } (replaces the zone map; built-ins only),
SetSidechain { track, device, source: Option<SidechainSource> }, SetOutputRoutes { track, routes }
(instrument only), SetClipFades { clip, fade_in, fade_out } (replaces both; audio clips only),
UpdateAudioClip { clip, file?, offset_seconds?, gain_db? } (audio clips; swaps in processed copies),
Batch { label, commands }.
`ParamAddress` also names send levels: `track/{id}/send/{send_id}` (dB; SetParam, automation);
removing a send (or the bus it feeds) drops its lanes. `model::automation_value(points, tick,
tempo)` evaluates a lane in seconds, exactly like the engine (which interpolates in samples).

Sample zones (phase 13, `model::sample`): typed structured state of the sample-based built-ins
(not the opaque `state`, which stays for plugins), all fields but sample/keys optional in JSON
(format_version 1). Samples are absolute paths like audio clips; `SampleRef.name` keeps the
original file name so a later "collect project files" can relocate/rename them. Validation:
keys 0..=127, low <= high, velocity 0..=1, tune ±48, gain -60..=24, end > start, loop_end >
loop_start, crossfade 0..=10 s, groups <= 16, at most 1024 zones. `root_from_file_name`
("Piano C3", "pad_F#2", "Strings_60"; 60 = C4), `zone_summaries` (get_project lists zones).

Clip fades are in **seconds**, not beats: like `offset_seconds` they shape the recording
(de-click, natural decay) and must not stretch with tempo changes; AI tools accept beats too
and convert at the clip edge. AddClip/SetClipFades reject a fade (or fade_in + fade_out)
longer than the clip at the current tempo; later resizes/tempo changes are not rejected —
the engine scales fades down proportionally to fit.

Time signatures (phase 17): changes are stored by **bar index**, not tick, so a change always
starts a bar and editing an earlier signature only moves later bar lines (content stays at its
ticks, like tempo changes). `cadenza_core::Meter<'_>` (`Project::meter()`, allocation free:
`bar_at/bar_start/bar_floor/bar_ceil/bar_position/tick_at_bar_position/bbt/tick_at_bbt/
bar_lines/segments_between/change_ticks/signature_at`) is the single bar/beat math (beats are
the denominator note); `MeterMap` owns a copy. Summary: `time_signature` (bar 1) +
`time_signature_changes [{bar (1-based), beat, time_signature}]`, marker bars and
`length_bars` follow the changes. At most 1024 changes, bar index 1..=100000.

Controller lanes (phase 17, `model::controller`): MIDI semantics, each point is one event whose
value holds until the next (FL event editor). `Controller` serializes as a string and parses
names ("sustain", "mod wheel", "bend", "aftertouch", "CC 74"); `default_value` (bend center,
CC7 100, CC10 center, CC11 full, else 0) is the resting value; `is_switch` (CC64..69). Pure
helpers: `window` (split/trim: the value in effect at the cut becomes a point at 0), `ramp`
(MIDI-quantized, curved), `splice`. `takes::split_clip` carries lanes; validation: ticks >= 0,
values in range, one lane per controller, <= 100000 points (points past the clip end are kept
but never played). Summary: `controllers: ["cc1 (12 events, 0.00..1.00)"]`.

Folders (phase 17): `TrackKind::Folder` tracks hold other tracks (`Track::folder`, nesting
allowed, no cycles); they carry no audio (no clips/devices/sends: rejected with actionable
errors). Their volume is a dB offset for every track inside (VCA), their mute/solo apply to
them. `Project::folders_of` (innermost first) / `folder_contents` (any depth, display order).
Removing a folder moves its tracks up to its parent (undo restores). Summary: `folder`.

Implemented extras: `ResetParam` (inverse of a first `SetParam`), `UpdateSend`, optional `index`
on AddTrack/InsertDevice/AddSend, `UpdateClip` can move a clip to another track. Instruments are
removed with `SetInstrument { device: null }`. Removing a track drops its lanes and sends that
target it and re-routes its dependants to Master (undo restores all). MIDI clips only on
Instrument tracks, audio clips only on Audio tracks. Device param ranges are NOT validated
here (model doesn't know devices) — callers with registry access (mcp) must validate.

Takes (phase 16, `model::takes`): clips sharing a `Take::group` are alternative takes of one
stretch of a track (loop recording passes, a recording over existing clips); only the active
take plays (`Clip::plays()`; the engine skips the others, `edits::overlaps`/rubber bands ignore
them). `UpdateClip { take }` (double option) changes membership; pure builders return ordinary
commands: `select_take(clip)` (Batch "Select take"), `split_clip(.., right_id, right_take)` (shared
with the app's S split), `split_group(group, at, new_group)` (split comping: each part keeps every
take), `comp_range(clip, from, to)` (swipe: splits at both ends + selects, Batch "Comp take", built
on a scratch `Document`), `flatten_group` (keep what plays). `Track::input` is edited with
`UpdateTrack { input }` (Audio tracks only, channels 0..=63). Both fields are optional in JSON
(format_version stays 1); summaries show `input` and `take`.

Project files (phase 14, `model::files`): a portable project is a folder
`<Name>/<Name>.cadenza.json` + `Audio/` (recordings, imports, processed copies) + `Samples/`
(collected zone files) + `Renders/` (exports) + `.cadenza/` (autosaves, session marker; safe to
delete). In memory every file path is absolute; `save_json(path)` stores paths inside the folder
holding the file relative to it (`/` separators) and `load_json` resolves them against that
folder, so moving the folder keeps working; other paths stay absolute and old absolute
projects load unchanged (format_version stays 1). `save_json` is atomic (temp + fsync +
rename, dir fsync) and keeps the previous file as `<file>.bak` (`backup_path`,
`write_atomic`); `save_json_absolute` (autosaves) writes absolute paths without a backup.
`file_refs()` lists audio clip files and sample zones (`FileRef { path, name, used_by }`),
`missing_files()` the absent ones, `relink_commands(fn)` builds the `UpdateAudioClip` /
`SetSampleZones` commands that repoint references (zones keep their original names);
`ProjectLayout` names the folders, `sanitize_file_name`, `project_stem`.

`Document { project, undo stack, redo stack }`: `execute(Command) -> Result<(), ModelError>`
(applies atomically, records the inverse), `undo()`, `redo()`, `can_undo/can_redo`,
`history_labels()`, `revision()` (bumped by every change: cheap change detection for
co-owners). Undo groups: `begin_group(label)`/`end_group()` fold every step executed in between
into ONE `Batch` undo entry (nested groups count depth, outer label wins; empty group = no
step); inside a group undo/redo cannot reach steps outside it and the group's undone scratch
steps are dropped at the end; `abort_group()` reverts the group. `Project::save_json(path)`, `Project::load_json(path)`,
`Project::summary() -> ProjectSummary` (compact, AI-oriented view: tracks, devices, clips with
note counts, ranges in beats). Validation: pitch 0..=127, velocity 0..=1, positive lengths,
existing ids, bus outputs must target Bus tracks with no cycles, instruments only on Instrument tracks.
The cycle check covers outputs, sends, instrument output routes and sidechains (source -> keyed
track); a device cannot be keyed by its own track; only instruments have `aux_outputs`. Removing
a track disconnects sidechains keyed by it and drops output routes to it (undo restores).

### cadenza-library (phase 18a, design `docs/design/style-library.md`)

Packs = folders of JSON (`pack.json` manifest with SPDX `license`/`attribution`/
`redistributable` + `styles/ forms/ progressions/ grooves/ patterns/<role>/`); item ids
`<pack>/<slug>`. The hand-written `core` pack (`crates/library/packs/core`, CC0) is compiled
in by `build.rs`; user packs load from `$CADENZA_LIBRARY_DIR` or
`$XDG_DATA_HOME/cadenza/library/<pack>/` (`Library::load_default`). Invalid packs/items are
reported in `Library::reports()` (`PackReport { issues }`) and skipped, never fatal;
`check_references` reports dangling ids. `validate` enforces the budgets (summary <= 120
chars; compact JSON: pattern 1,200, style 3,500, form 2,000, progression 600, groove 1,200),
lane keys, step strings (`X x o = . -`, length divides the pattern), license fields.
Patterns: `role`, `harmony` (none | chord | scale | absolute + key), `steps_per_beat`,
`bars`, `meter`, `feel`, `family` + `variant` (main | variation | fill | intro | ending),
`lanes` (ordered): drum names (`DRUM_LANES`, GM superset of `write_drum_pattern`'s, e.g.
shaker 70, claves 75, hi_bongo 60, open_conga 63), chord tones `R 3 5 7 9 11 13` (+/-n
octaves), `chord` (voiced with `voicing`, voice-led), `bass`, `app` (semitone below the next
root), `ant` (next root, anticipated), scale degrees `^1..^7` (`^b3`), absolute notes.
`realize(pattern, &RealizeOptions { bars, key, chords, beats_per_chord, fill_every, vary,
fill, variation, groove, velocity, octave })` is pure -> `Realized { notes: Vec<NoteSpec>
(beats), length_beats, warnings }`; `RealizeError` texts are actionable. `Library::search(
&Query { text, kind, genre, role, section, energy, family })` ranks token matches over
id/name/aliases/genres/tags/family/summary (accents folded, `section_name` maps "Chorus 2"
-> chorus, "Pre-chorus" -> pre; fills demoted unless asked); `card(item)` -> `{id, kind,
name, summary, facts}`; `family_member(pattern, variant)`; `parse_chords` (symbols or roman
numerals with a key, mixable).
MCP (`tools::library`; `Session::library()` loads `Library::load_default` on first use,
`set_library` for tests/the app): `library_search { query, kind, genre, role, section, energy,
family, limit (8, max 25) }` -> `{cards: [{id, kind, summary, facts}], total}`; `library_get {
ids (<= 5) }` -> items as stored + pack `license` (unknown ids suggest the closest);
`apply_pattern { pattern, track_id, start_beat, bars, key, chords (symbols | roman + key |
progression id + key), beats_per_chord, fill_every, vary, groove ("none" drops the
default), velocity, octave }` -> one `AddClip` named `Name [pack/id]` + `{clip_id,
length_beats, summary, warnings}` (drum lanes go through `compose::drum_lane_warnings`, shared
with `write_drum_pattern`: drum_sampler pads without samples, notes drum_machine cannot play;
meter mismatch; pitched pattern on a drum kit); `build_pattern_clip(project, library, args)`
is the pure builder (the app's drag and drop uses it). `apply_form { form, start_bar, tempo,
style }` -> one `Batch` "Apply form '<name>'" (markers replacing ones at the same tick,
SetTempo, `SetProjectMeta` with the style: explicit or the one style listing the form) +
section table; `build_form` is pure. `Project.meta: ProjectMeta { style }` (optional in
JSON, format_version 1; `Command::SetProjectMeta`, no sound: `live::diff` ignores it) and
`ProjectSummary.style`. Budget test: `search_and_get_outputs_stay_under_their_caps`. Agent
prompts carry the library lines; hand-off notes keep the library ids.
Import (`library::import::import_pattern(notes, &ImportOptions)`, used by `cadenza library
import <file.mid> [--pack dir] [--track n | --channel n] [--role] [--key] [--chords
--beats-per-chord] [--steps-per-beat] [--bars] [--genre] [--force]`): quantizes to the grid,
per-step timing deviations + velocity ratios become a `Groove` (`<slug>_groove`, skipped when
the source was already quantized), role from GM channel 10 / program family / register,
pitched notes as chord tones when the chords explain at least half of them, else scale
degrees of the given or inferred (Krumhansl) key; lanes are found by realizing candidates, so
realizing the result over the same chords gives the quantized notes back. Writes
`patterns/<role>/<slug>.json`, `grooves/`, `midi/<slug>.mid` into the pack (default
`<library dir>/user`, created with `LicenseRef-user-imports`, not redistributable) and
re-validates it. `cadenza library validate <pack or library folder>` exits 1 on problems;
`list [--items] [--json]`. Nothing is downloaded.

### cadenza-analysis (phase 18b)

Pure, offline (allocates; never on the audio thread). The level meters moved here from the
engine (`levels`, `loudness`, `bands`, `Analyzer -> LevelStats`; the engine re-exports them).
`spectrum::SpectrumAnalyzer` (realfft, 8192-point Hann, 50 % hop): mean third-octave mid/side
levels (`THIRD_OCTAVE_HZ`, 31 bands; band power = mean square, so a sine reads its RMS),
`Stereo { width (side share), low_correlation, low_side_db }` below 120 Hz, and a `Timeline`
of octave levels (`OCTAVE_HZ`, 10 bands) per hop. `detail::DetailAnalyzer -> Detail` = both.
`roles::Role` (kick, drums, perc, bass, keys, guitar, pad, lead, vocal, strings, brass, fx,
other; `infer_role(name, instrument, library hints)`, `balance_db`, `high_pass_hz`,
`ownership(octave)`). `masking::masking` ranks pairs colliding in the same octave band at
the same time (both within 10 dB of the band total and 6 dB of each other; best band per
pair; `cut` = lower owner, else less energy share). `targets::MixTargets { lufs,
true_peak_db, profile: ToneProfile (pop/electronic/hiphop/rock/acoustic/orchestral curves),
roles, notes, source }` (`from_style`, default -14 LUFS / -1 dBTP / pop).
`issues::find_issues(master, tracks, targets, masking, warnings) -> Vec<Issue { id "i1",
severity, kind, track_id, message, moves }>` (clipping/true peak, loudness via limiter drive or
fader, phase, narrow image, wide low end, tone balance vs curve, over-compression, hot
channels, role balance vs median anchor, rumble -> high-pass, masking -> bell cut).
`moves::MixMove` (tagged `move`: gain, pan, eq {kind bell|low_shelf|high_shelf|high_pass|
low_pass}, compressor, limiter, send) is the fix vocabulary. `reference::compare(mix, ref)`:
loudness/crest/width deltas, octave shape deltas after level matching, findings, master EQ
moves. Engine: `analyze_mix_detailed(project, opts, progress) -> DetailedMix { report,
master: Detail, tracks: Vec<TrackDetail> }` taps every channel like the stem export
(cancellable).

### cadenza-dsp

```rust
pub enum DeviceRole { Instrument, Effect }
pub struct ParamInfo { id: &'static str, name: &'static str, min: f32, max: f32, default: f32,
	unit: &'static str, description: &'static str }
pub struct DeviceInfo { id: &'static str, name: &'static str, role: DeviceRole,
	description: &'static str /* written for an AI choosing devices */, params: &'static [ParamInfo] }
pub enum MidiEvent { NoteOn { offset: u32, pitch: u8, velocity: f32 }, NoteOff { offset: u32, pitch: u8 }, AllNotesOff { offset: u32 },
	ControlChange { offset, controller: u8, value: f32 /* 0..1 */ }, PitchBend { offset, value: f32 /* -1..1 */ }, ChannelPressure { offset, value: f32 } }
// offset()/with_offset()/is_note(); `cc::{MOD_WHEEL, SUSTAIN, ALL_SOUND_OFF, RESET_ALL_CONTROLLERS, ALL_NOTES_OFF}`
pub struct Transport { pub bpm: f64, pub playing: bool, pub beat_position: f64,
	time_sig_numerator: u16, time_sig_denominator: u16, bar_start: f64 /* quarters */, bar_number: i32 } // Default = 4/4
pub trait Processor: Send {
	fn info(&self) -> &'static DeviceInfo;
	fn prepare(&mut self, sample_rate: f32, max_block: usize);
	fn set_param(&mut self, index: usize, value: f32);   // plain value, clamp internally
	fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[MidiEvent], transport: &Transport);
	fn reset(&mut self);
	fn latency(&self) -> u32 { 0 }
	fn tail_seconds(&self) -> f32 { 0.0 }
	fn save_state/load_state, presets() -> Vec<PresetInfo>, load_preset(key)  // non-realtime, plugins
	fn has_sidechain(&self) -> bool; fn set_sidechain(&mut self, l, r)        // key for the next block only
	fn aux_output_count(&self) -> usize; fn aux_output(&self, i) -> Option<(&[f32], &[f32])> // last block
}
pub fn registry() -> &'static [&'static DeviceInfo];
pub fn create(id: &str) -> Option<Box<dyn Processor>>;
pub fn factory_presets(id: &str) -> &'static [FactoryPreset { name, description, params, zones: &[FactoryZone] }];
	fn set_zones(&mut self, zones: Vec<Zone>) -> bool  // sample built-ins; non-realtime, after prepare
pub trait SampleData: Send + Sync { sample_rate, num_channels, channel(i) -> &[f32] } // Arc<dyn SampleData> per zone
```
Sample instruments (phase 13): `sampler` (multi-zone; amp ADSR, SVF LP/HP/BP with its own ADSR,
velocity/key tracking, velocity sensitivity + curve, gate/one-shot, glide + mono legato,
transpose, polyphony 1..64) and `drum_sampler` (16 GM pads `instruments::drum_sampler::PADS`,
one-shot, choke groups, per-zone AD envelope, 8 extra stereo outputs = `Zone::output` 1..=8).
Shared playback in `instruments::sample_voice` (per-zone frame positions precomputed in
`set_zones`; forward/ping-pong loops with an equal-gain seam crossfade; reverse; round-robin and
velocity layers; stolen/choked voices fade 4-5 ms in spare slots), `util::interp` (cubic
Hermite up to 1.25x, table windowed-sinc with cutoff 1/ratio above), `util::pitch` (YIN
`detect_pitch`). `uses_samples(id)`; factory presets carry `FactoryZone`s naming files of the
factory sample folder (`factory_samples::*`).
Performance controls (`util::expression::Expression`): poly_synth, electric_piano and sampler
honor the sustain pedal (`VoiceSlot::key_up/pedal_up`: released keys keep sounding until pedal
up; AllNotesOff lifts it), pitch bend (±2 semitones), mod wheel (5.5 Hz vibrato up to ±0.5
st) and channel pressure (poly_synth/sampler: filter +24 st); drums ignore them. Plugins:
CLAP ports that accept MIDI get raw CC/bend/pressure bytes (`ControllerRoute::Midi`), CLAP-only
ports get per-note tuning/pressure expressions of the held keys (other CCs dropped); VST3
controllers become parameter changes through `IMidiMapping` (queried at instantiation, 128 =
pressure, 129 = bend); the sandbox shm carries them (EV_CC/EV_BEND/EV_PRESSURE). Live MIDI input
maps CC/bend/pressure to these events (`midi_io::input`).
The built-in `compressor` has a sidechain input (detector keyed by it when set).
`drum_machine` plays every GM percussion note 35-81 (phase 18b): kit, cymbals (china, splash,
ride bell) and synthesized hand percussion in `instruments::percussion` (rim, tambourine,
cowbell, bongos, congas, timbales, agogo, cabasa, shaker, whistles, guiro, claves, woodblocks,
cuica, triangle; 8-voice pool, choke pairs; params `perc_tune/perc_decay/perc_level`);
`drum_machine::plays(note)` drives the MCP silent-lane warnings.
Instruments receive silence in `left/right` and must write (not add) their output.
Events are sorted by `offset` within the block.

### cadenza-engine

```rust
pub struct RenderOptions { sample_rate: u32, block_size: usize, start: Tick, end: Option<Tick> /* None = song end + tail */,
	plugins: Option<Arc<PluginCatalog>>, hosting: PluginHosting }
pub struct PluginHosting { mode: HostingMode /* Sandboxed | InProcess */, sandbox: SandboxOptions } // default: from_env()
pub fn render_offline(project: &Project, opts: &RenderOptions) -> Result<StereoBuffer, EngineError>;
pub fn render_to_wav(project: &Project, opts: &RenderOptions, path: &Path) -> Result<MixReport, EngineError>; // 32f, streamed
pub fn render_to_file(project, opts, path, &ExportOptions) -> Result<MixReport, EngineError>; // WAV 16/24/32f, FLAC 16/24, MP3; TPDF dither
pub fn export_options_for_path(path, bit_depth: Option<u32>, mp3_bitrate_kbps: Option<u16>) -> Result<ExportOptions, EngineError>; // by extension
pub fn export_project(project, &RenderOptions, &ExportSpec, progress: &mut dyn FnMut(&ExportProgress) -> bool /* false = cancel */)
	-> Result<ExportReport { files: Vec<ExportedFile { path, track_id, name, gain_db, stats, sample_rate, duration_seconds, bytes }>, mix: MixReport, notes }, EngineError>;
pub struct ExportSpec { start: Tick, end: Option<Tick> /* None = song end */, tail: bool, mixdown: bool,
	stems: StemSet /* none | tracks | buses (buses + tracks routed to master: they sum to the mix) | selected(ids) */,
	stems_through_master, stems_pre_fader, format: ExportOptions, sample_rate: Option<u32> /* resample */,
	normalize: Normalize /* off | peak{peak_db} | loudness{lufs, true_peak_db} */, normalize_stems_individually,
	metadata: Metadata, folder, name /* mix "<name>.<ext>", stems "<name> - <track>.<ext>" */ }
pub fn analyze_mix(project: &Project, opts: &RenderOptions) -> Result<MixReport, EngineError>;
pub fn compile(project: &Project, sample_rate: u32, max_block: usize) -> Result<Graph, EngineError>;
pub struct MixReport { sample_rate, duration_seconds, latency_samples, warnings: Vec<String>,
	master: LevelStats, tracks: Vec<TrackLevel { track_id, name, stats }> }
pub struct LevelStats { peak_db, true_peak_db, rms_db, lufs_integrated /* BS.1770-4 */, lufs_short_term_max,
	crest_factor_db, clipped_samples, band_energy_db: BandEnergy, stereo_correlation } // silence = -144
pub struct Player; // new(), load(&Project) -> Result<Vec<String> /*warnings*/>, play/stop/seek -> Result, position(), is_playing(),
	// clock() -> PlayerClock (Send+Sync: position/is_playing/sample_rate from any thread),
	// meters() -> Option<Arc<MeterBank>> (per load), set_live_volume/set_live_pan(Option<TrackId>, ..) (no recompile),
	// set_live_param(device, param_id, value) / set_live_send(send, db) -> Result<bool> (false: not in the graph, reload),
	// set_automation(&lanes, &tempo) -> Result<bool> (false: tempo changed or a lane was removed, reload),
	// set_plugins, set_hosting, plugin_crashes() / take_plugin_crashes() -> Vec<PluginCrash>,
	// set_midi_input(Option<&MidiPort>), set_monitor_track(Option<TrackId>), dropped_midi_events(),
	// set_loop(Option<(Tick, Tick)>), set_audition(Option<AuditionScope { tracks }>),
	// set_track_clips(track, &clips, &tempo) -> Result<bool> (false: reload)
pub fn midi_input_ports() -> Result<Vec<MidiPort>, EngineError>; pub fn find_midi_input(index_or_name) -> Result<MidiPort, _>;
pub fn compile_hosted(project, sample_rate, max_block, plugins, &PluginHosting) -> Result<Graph, EngineError>; // Graph::plugin_crashes()
pub fn capture_state(&Device, plugins, &PluginHosting) -> Result<Option<String>, EngineError>; // fresh instance: state + params
pub fn plugin_presets(&Device, plugins, &PluginHosting) -> Result<Vec<PresetInfo>, EngineError>;
pub fn preset_state(&Device, key, plugins, &PluginHosting) -> Result<String /*base64*/, EngineError>;
```
Routing: tracks are processed in an order that respects outputs/sends, instrument output routes
and sidechain keys (stable: non-bus tracks, then buses, moved only when needed). A keyed device
gets the source's pre-fader (post-inserts, ignores mute) or post-fader signal; PDC aligns key and
main signal at the device (delaying whichever is earlier; instruments delay their notes). Extra
instrument outputs mix into the track after the instrument or into a bus accumulator (PDC-tapped
after the instrument). Unusable sidechains/routes are ignored with warnings. CLAP: the first
non-main input port is the sidechain; VST3: the first `kAux` input bus; every non-main output
port/bus with channels is an extra output (sandboxed too, via extra shm channels).
Plugin hosting: CLAP/VST3 devices run **sandboxed by default** (one `cadenza-plugin-sandbox`
process each, found via `CADENZA_PLUGIN_SANDBOX` or next to the executable); opt out with
`PluginHosting::in_process()` / `CADENZA_PLUGIN_HOSTING=in-process` (debugging, benchmarks). A
missing sandbox falls back to in-process with one actionable warning. A plugin that crashes or
hangs is bypassed (instrument silent, effect pass-through), the render completes, and
`MixReport.warnings` names track, device, reason and approximate time. The `Player` polls
sandbox monitors off the audio thread; `load` re-creates every device, which restarts crashed
plugins (MCP: `transport_status.plugin_crashes`, `restart_plugins`).
Sandbox realtime policy (`SandboxOptions`): late is not dead — a round trip that misses its
deadline (`max(realtime_timeout 2 ms, chunk duration)`) only drops that chunk (instrument
silent, effect dry), the late reply is retired by sequence number, busy chunks are skipped
without waiting (note-offs and param changes carried over), and `SandboxStats` / engine
`PluginXruns` (`Graph/Player::plugin_xruns`) count it; only no on-time answer for
`hang_timeout` (2 s) is a hang (offline: any miss of `offline_timeout`, 30 s). Realtime
batching: `realtime_chunk` (128) = 4 engine quanta per round trip as 4 sub-blocks in the shm
(events/params/transport stay per quantum), +96 frames of latency reported through
`latency()` so PDC absorbs it; offline is not batched (`offline_chunk` 0). The child's audio
thread asks for `SCHED_FIFO` (`realtime_priority` 10, direct or RealtimeKit via `busctl`;
`RLIMIT_RTTIME` soft limit demotes it on `SIGXCPU`), shm is `mlock`ed. Surge XT stress test:
`cargo test -p cadenza-plugin-host --release --test sandbox_realtime -- --ignored --nocapture`.
Plugin state is captured explicitly (MCP `capture_plugin_state`, automatically on `save_project`,
and after loading a plugin preset) from a fresh instance, then stored with `SetDeviceState`.
MCP preset tools: `list_presets`/`load_preset`/`save_preset` (user presets in
`$CADENZA_PRESET_DIR` or `$XDG_DATA_HOME/cadenza/presets/<device id>/<name>.json`); routing tools
`set_sidechain`, `set_output_routes`. Plugin factory presets: CLAP preset-discovery + preset-load
(VST3 program lists not yet). Descriptors carry `sidechain_channels` and `aux_outputs` (catalog
cache v2: older caches are rescanned).
Plugin editors (phase 11, `cadenza_plugin_host::editor`): the GUI lives in the process that owns
the instance, on its `cadenza-plugin-main` thread, which is also the plugins' event loop (poll on
a wake-up pipe + the X11 connection + plugin fds; CLAP timer-support/posix-fd-support, VST3
`IRunLoop` on the plug frame). Windows: embedded (`set_parent`, api `x11`) preferred over CLAP
floating (so every editor gets the host-drawn title strip below) into our own top-level X11
window (x11rb pure-Rust connection, XWayland on Wayland; `_NET_WM_WINDOW_TYPE_DIALOG`,
fixed-size hints when not resizable); VST3 `IPlugView` attached with `X11EmbedWindowID`;
floating is the fallback when a CLAP plugin cannot embed at all. The window is the plugin's
content size plus a `TITLE_STRIP_HEIGHT` (24 px) strip on top (`editor::geometry`, built-in
8x8 bitmap font in `editor::font`: XWayland here ships no X11 core fonts), drawn with the
title and a close (×) button; dragging it moves the window ourselves (pointer grabbed for the
gesture, window placed at pointer minus press offset with `ConfigureWindow`;
`_NET_WM_MOVERESIZE` was unreliable under Hyprland). New editors center on the RandR monitor under the pointer (whole root if RandR is
unavailable); a position the user drags to is remembered per plugin id for the process's
life and reused next time that plugin opens. Resizes both ways, close button (ours or the
window manager's) = close (reported `Closed`).
`PluginEditor` handle (from `instantiate_with_editor`, `ClapProcessor/Vst3Processor/
SandboxedProcessor::editor()`, `Graph::plugin_editor`): `open(title)/close/is_open/poll()
-> Vec<EditorEvent { GestureBegin, Value{param, plain}, GestureEnd, StateChanged, Closed }>/
save_state()` (live instance). CLAP edits = output events of `process`/`params.flush`, pushed by
the audio thread into a wait-free rtrb; VST3 `begin/perform/endEdit` (main thread) -> value to
the processor through an rtrb + event queue; host values mirrored into the VST3 controller
(`ParamMirror`). `Processor::idle()` (dsp): called by stopped graphs each callback so plugins
flush editor edits (sandbox: `CMD_IDLE` round trip only while an editor is open). Sandbox:
editor requests share the control channel (frames are `Envelope { id, body }`, late replies are
skipped), handled by the child without the processor lock; the child queues events until polled.
`Player`: `open_editor(device, title)/close_editor/open_editors/poll_editors() ->
Vec<EditorUpdate>/capture_editor_states(project, devices)/take_editor_states()`; `load` carries
dirty live states over to the new instances and re-opens open editors; `set_live_param` skips
echoing a value the editor just reported. Display tests: `cargo test -p cadenza-plugin-host
--release --test editor_display -- --ignored --test-threads 1` (scripted plugins + Surge XT/LSP).
Sample zones: `compile` converts `Device.zones` (or any `uses_samples` device) to DSP zones whose
audio is decoded + resampled to the engine rate through `AudioCache::global()` and shared as
`Arc` (`samples::CachedSample`, no copy, nothing on the audio thread); a missing/unreadable
file leaves the zone silent with one warning naming track, device, file and key.
`engine::factory`: synthesized factory content (drum_machine renders + a looped additive sine
pad C2-C5, mono 48 kHz WAV) in `$CADENZA_FACTORY_SAMPLES` or
`$XDG_DATA_HOME/cadenza/samples/factory`, rendered on first use (`ensure_factory_samples`,
versioned), `list_factory_samples`, `preset_zones`, `default_content(id)` (a new sampler starts
with "Sine Pad", a drum_sampler with "Synth Kit"). `auto_zone(path)`/`detect_root`: name, else
pitch, else C4.
Freeze / bounce (phase 17, `engine::bounce`): `render_track(project, opts, track, &BounceScope::
Track | Clips(ids), start, path)` renders a synthetic one-track project (the track at 0 dB,
centered, no sends/folder/freeze, keys and extra-output buses dropped, its device-param lanes
kept; `Clips` keeps only those clips and no inserts) to a 32f WAV: what the track's fader
receives, so playing it back through the fader matches the original (test: frozen render ==
live render). A frozen track compiles to one audio clip of its render from 0 with no devices
loaded (instrument, inserts unloaded: CPU saved); fader/pan/sends/mute still apply; a missing
render plays the track unfrozen with a warning. Freeze files live in `.cadenza/freeze/` (paths
relativized like clips).
Folders: folder tracks get no node; each node's fader is `own_db + folder_db` (sum of its
folders' volumes), `set_channel_volume(folder)` moves every node inside (`Graph::folders`,
realtime safe) and folder volume lanes bind to `AutoTarget::FolderVolume`; a folder's mute
mutes and its solo solos the tracks inside.
Audio clips: files are decoded + resampled to the engine rate at compile time through
`AudioCache::global()` (any format symphonia reads) and played with `render_clip` (realtime
safe). Crossfades (phase 17, `audio_clip::crossfade_overlaps`): where a playing clip starts
inside an earlier one and ends after it, the earlier fades out and the later fades in over the
crossfade, then the earlier stops; the crossfade is the later clip's fade-in when set (length
clamped to the overlap, its curve = the editable handle), else the whole overlap, equal power;
clips nested inside another still sum. Micro-fades (1 ms, linear, `micro_fades`) soften bare
clip edges (no fade, no crossfade); a loop wrap fades audio sources out before the jump and in
after it (`Source::Audio::fade_in`, also after a start/seek past 0). All realtime safe
(`no_alloc` loops over crossfaded clips). An unreadable file is a `MixReport`/`Player::load`
warning (clip silent), never a hard error.
Controller lanes play through `MidiSchedule` (`EventKind::Control`; note-offs, then
controllers, then note-ons at a sample): chase sends every lane's value before the position (or
its resting value) ahead of re-triggered notes, and a clip ending bent or with the pedal down
gets a reset event at its end. Covered by `no_alloc`.
Live MIDI: the midir callback pushes into an rtrb queue whose consumer is handed to the audio
thread (and back to be dropped); each callback drains it into a pre-allocated buffer and
`Graph::push_live_event` feeds the monitored instrument at the next quantum (offset 0: midir
and cpal share no clock). Monitoring sounds while the transport runs; switching the monitored
track releases held notes.
Shared session: `cadenza_mcp::SharedSession = Arc<Mutex<Session>>`;
`CadenzaServer::with_shared_session` lets the app and the agent edit the same document (each tool
locks it per call; render/analyze work on a copy). `Session::change_key()` = (epoch bumped by
`reset`, document revision); `is_clean/mark_clean` track the state last saved/opened
(`save_project` marks clean).
MCP sample tools (`tools::samples`): `list_factory_samples`, `load_sample` (drum pad by name/
note keeping its settings, `layer`; sampler `zone` / `keys` / auto-map with root detection),
`set_zone`, `set_pad` (patch fields; a pad on an unrouted extra output gets a route to its own
track in the same batch); `build_device` fills default content; presets (factory and user) carry
zones; `set_output_routes` accepts built-in extra outputs; `write_drum_pattern` knows rim/pedal_hat/
tambourine/cowbell lanes and warns about lanes a drum_sampler has no sample for. The agent's
`PathPolicy::resolve_sample` allows the session folder or the factory folder.
MCP mix tools (phase 18b, `tools::mix`; shared with the app and `cadenza mix`): targets come
from `Project.meta.style` -> the style's `mix { lufs, true_peak?, profile?, levels {role: dB},
notes }` (`resolve_targets`, overrides `target_lufs`/`genre_profile`/`true_peak_db`, default
-14 LUFS); `run_analysis` = `analyze_mix_detailed` + `assess` (roles via `track_role`: name,
instrument, library clip ids; master limiter state; masking; issues) -> `MixAnalysis`.
`analyze_mix { range, targets..., max_issues (8) }` -> compact summary (targets, master with
tone vs target + octave shape, per-channel role/LUFS/peak/crest/width, top 5 masking pairs,
issues with `moves`, `more_issues`, render `warnings`); `analyze_track { track_id }` (third-
octave spectrum, low-end check, its masking/issues, role defaults); `compare_to_reference {
file, reference_start_seconds, reference_seconds, range }`; `mix_targets` (targets + per-track
role defaults); `apply_mix_moves { moves, label? }` -> `mix_move_commands` (built on a scratch
`Document`, so later moves see earlier ones; EQ inserted first, compressor before a limiter,
limiter last, bell slot = nearby band or free one; params clamped to the device ranges; all or
nothing, errors name the move index) executed as ONE `Batch` "AI mix pass". `compare_passes`
lists before -> after numbers. Floats go out as rounded `f64` / `cadenza_analysis::short`
(no `-9.100000381` noise). Budget test: `outputs_stay_within_their_token_budgets` (24 channels:
analyze_mix < 9k chars, analyze_track < 3k, mix_targets < 5k).
MCP controllers (`tools::controllers`): `set_controller_events { clip_id, controller, events:
[{beat, value}], replace? }`, `draw_controller_curve { clip_id, controller, start_beat, end_beat,
from, to, shape: linear|exponential|logarithmic|sine, cycles?, resolution_beats? }` (replaces
the range), `get_controller_events`. MIDI import turns CC/bend/channel pressure into lanes
(`controller_lanes`, per track or channel), export writes them; MIDI recording keeps them
(`Recorder::take_controls`, `control_lanes`; overdub merges, replace clears the range).
MCP time signatures: `set_time_signature { numerator, denominator, at_bar? (1-based) }`,
`remove_time_signature { bar }`; `render`/export bar ranges, `write_drum_pattern` bars and
`export_midi` follow the changes. MIDI import keeps every signature (`midi_io::meter_changes`:
off-bar changes move to the next bar line with a warning, repeats dropped); export writes them.
MCP file tools: `import_audio`, `get_audio_info`, `set_clip_fades`, `import_midi` (Batch via
Document, one undo), `export_midi`; `render` picks WAV/FLAC/MP3 from the extension and takes
`start_bar/end_bar` (1-based, end included) or beats, `tail`, `output_sample_rate`, `normalize`
(`{mode: off|peak|loudness, peak_db, lufs, true_peak_db}`), `metadata`; it returns the mix report
flattened plus `path`, `gain_db`, `file` (levels as written). `export_stems` (tracks | buses |
selected + `track_ids`, `include_mixdown`, `through_master`, `pre_fader`, format/bit depth,
range, normalization, metadata; default folder = the project's `Renders/`). Portable projects
(`tools::portable`): `collect_files` (copies external clip/zone files into `Audio/`/`Samples/`
under their original names, once per SHA-256 content, reusing identical files already there,
`name (2).ext` on conflicts, factory samples only with `include_factory`; one undo step
"Collect project files"; needs a saved project), `list_missing_files`, `relink_files`
(`search_folder` indexed recursively by lower-case file name, hidden folders skipped, ties go to
the candidate sharing most trailing folders with the old path; one undo step), `pack_project`
(zip of the folder as `<folder>/...` with the current document written fresh, without
`.cadenza/`, backups and (by default) `Renders/`; `outside` lists files not in the folder).
`open_project` and the app go through `portable::open_document`: missing factory samples are
repointed to this machine's factory folder before the history starts; `unpack_project` rejects
unsafe entries and never overwrites. The embedded agent's `PathPolicy` confines all file tools
to the session folder with checked extensions (`relink_files`/`export_stems` folders via
`resolve_dir`/`resolve_dir_create`, default `Renders/`).
Metering: every graph measures each track's post-fader signal and the master per quantum and
publishes at the end of each `Graph::process` into a `MeterBank` of atomics (peak via `fetch_max`
on the float bits, readers take-and-reset it; RMS with 300 ms ballistics is a plain store):
wait-free, no allocation (covered by the `no_alloc` test). Peak hold/fall-off is the UI's job.
Live parameter path: every compiled graph carries a `Resolver` (address -> node/device slot/
param index/send edge, built at compile time, used only off the audio thread). The `Player`
keeps the resolver of the loaded graph; `set_live_param`/`set_live_send` resolve on the control
side and push `Cmd::Param(device, index, value)` / `Cmd::SendLevel` (Copy) through the command
rtrb; the callback calls `Processor::set_param` (built-ins, in-process and sandboxed plugins:
all realtime safe) or retargets the send edge gain (ramped over a quantum). `set_automation`
builds new `Binding`s off the audio thread and sends them boxed through a second rtrb; the
callback swaps them in (`Graph::swap_automation`, matched by load id; sets for a graph still
queued wait, stale ones go back) and returns the old box through a garbage ring. Covered by
the `no_alloc` test. Structural changes (tracks, devices, routing, clips, tempo, bypass,
resetting a param to its default) still recompile.
Loop / audition / rescheduling: `Graph::set_loop` wraps sample-accurately inside a quantum
(collect up to the end, `MidiSchedule::release` note-offs for notes held at the end, seek +
chase at the start, collect the rest; audio clips split the quantum; a quantum ending exactly
on the end wraps at the next one; the output position wraps once the latency has passed;
tails ring on; automation is evaluated per quantum on the timeline position). The loop is a
`Cmd::Loop` kept by the callback across graph swaps; offline renders never loop.
`set_audition`: an `Arc<Audition>` of atomics per graph (mask built off-thread from
`Resolver::feeds`: scoped tracks + the buses they reach through outputs/sends/aux outputs);
out-of-scope nodes still render (sidechain keys work) but are not mixed; auditioned tracks
ignore other solos, mute still holds. `set_track_clips` builds a `ScheduleSwap` off-thread
and sends it boxed through its own rtrb (garbage ring back); `Graph::swap_schedule` swaps
schedule + event buffer and releases only the notes sounding before but not after.
Event buffers are sized for both sides of a wrap (`compile::event_capacity`). All covered by
the `no_alloc` test.
Export jobs (`engine::export`): one offline pass (`render::prepare_range` + `render_analyzed_with`
and a per-quantum hook) renders the mix and taps each stem node (post-fader `buf`, or `pre` for
pre-fader; silent when the mix silences it), latency aligned like the per-track analyzers, into
temporary 32f WAVs in a hidden folder next to the outputs (removed however the job ends).
Stems through the master are then played one by one through a copy of the master channel (a
synthetic project: one audio track on the temp file + master inserts/fader/master lanes, same
tempo). Each file is gained (normalization: by default stems take the mix gain so they still
sum to it; loudness gain is capped by the true-peak ceiling with a note), resampled, measured
and encoded with metadata (dither per `ExportOptions`). Progress fractions weight the main
pass, master passes and encodes; returning false from the callback cancels
(`EngineError::Cancelled`) and deletes what was written. `render_to_file` stays for simple renders.
Engine internals: devices run in fixed 32-sample steps (block-size independent, bit-identical);
PDC aligns every summing point and offline renders trim leading latency; realtime graphs are
swapped via rtrb and returned to be dropped off the audio thread.

Recording (phase 16; `engine::input`, `engine::metronome`, `engine::record`):
- Devices: `Player::open(&AudioConfig { output, input_enabled, input, sample_rate, buffer_frames })`
  (`new()` = defaults), `audio_devices()` (names, channels, common rates), `set_audio_input(Option<
  Option<&str>>)` (a failing input never fails the player: `input_error()`), `input_info()` (device,
  channels, rate, `InputMode`), `input_stats()` (per-channel peaks, overflow/underrun/trim counters),
  `output_latency()`, `set_test_tone(bool)` (440 Hz, -20 dBFS; settings dialog only).
- Input path: the cpal input callback converts to f32 and pushes interleaved frames into an rtrb
  ring (0.5 s) plus one capture stamp per callback; the output callback reads one input frame per
  output frame (`InputReader`, never blocks: an empty ring pads silence = underrun, a full one drops =
  overflow, both counted as dropouts). Same device and rate = `InputMode::Exact` (shared clock: 1:1
  copy after priming one block + margin; a runaway backlog is trimmed). Separate devices or other
  rates = `Adaptive` (linear-interpolation resampler, ratio corrected by a slow controller on the
  ring fill, at most ±0.5 %). The block's frames go to the graph through an `InputFifo` (one quantum
  of cushion, trimmed when nothing consumes it) for monitoring.
- Monitoring: audio tracks with an input hear it instead of their clips, through their inserts,
  fader, sends: `On` always, `Auto` while armed and stopped / recording / counting in, `Off`
  never. A stopped graph renders (like note preview) while any track monitors. Arm states:
  `set_armed(track, bool)`/`disarm_all` (kept across loads; `Cmd::Arm`).
- Metronome: `set_metronome(MetronomeSettings { on_play, on_record, level_db })`; synthesized
  clicks (accent on every bar line, beat = the denominator note, both following time signature
  changes through `metronome::MeterSegment`s built at compile time; the count-in uses the
  signature at the playhead) scheduled per quantum from the tempo
  segments (floor of the exact beat sample, so none is lost between quanta) and delayed by the
  graph latency like the output; offline renders never click (untouched while silent).
- Recording: `start_recording(&RecordSetup { folder, start (punch-in), punch_out, count_in_bars,
  tracks: [RecordTrack { track, name, input: Some(TrackInput) = audio | None = MIDI }],
  buffer_seconds (4) })` -> `Arc<RecordingMonitor>`; plays from the current playhead after a
  frozen-transport count-in (`Graph::start_count_in`: clicks only, monitoring live, then a clean
  start). After each block `Graph::segments()` says which timeline samples the output played
  (split at loop wraps, none during count-in); the `RecordTap` (boxed through an rtrb, back through
  a garbage ring) pushes the recorded input channels of those frames, filtered to [start,
  punch_out + 1 s), into a pre-allocated sample ring + `RecEvent::Audio { tag, frames }`, MIDI
  input as `RecEvent::Midi { tag }` (the block's first heard sample; midir and cpal share no
  clock), playhead jumps as `Wrap`. A full ring skips the block and counts it. The
  `cadenza-record-writer` thread drains into one 32-bit float WAV per audio track (`<track> rec
  <UTC stamp>.wav`, unique), fills dropout gaps with silence (positions kept), splits loop passes
  (`RecordedPass { tag, file_start, frames, wrapped_at }`), publishes peaks per 256 frames + MIDI
  for live drawing. `request_stop_recording()` + `finish_recording(timeout)` (or `stop_recording()`)
  -> `Recording { passes, audio files, midi items, latency, midi_latency, dropouts, warnings }`.
- Latency compensation: a frame heard-tagged `t` was captured `L` frames earlier, `L` = measured
  capture -> playback delay (cpal capture/playback timestamps; without them the average ring
  backlog) + `set_latency_offset(frames)` (user, from the settings in ms). `record_commands(
  project, &Recording, &RecordOptions { merge, overdub, midi_takes, quantize, pattern_clip })`
  builds ONE Batch "Record": each pass = a clip at its tag reading the file from `file_start + L`
  (length to its loop end / punch-out / end - L, so a pass reads into the next one in the same
  file: seamless takes). Several passes, or a recording over existing clips (cut at the range,
  inside parts become the first inactive takes), form a take group, the last pass active. MIDI
  (`Recorder::push_at` at tag - output latency, `release_all` at wraps, quantize): merge into the
  pattern clip / the clip under the start (overdub keeps notes, replace removes the recorded
  range; grows the clip to the bar), or a new bar-aligned clip; loop + `midi_takes` = a take per
  pass.
- `Player::virtual_device(rate, input_channels) -> (Player, VirtualDevice)`: the same callback
  driven by hand (`process(output, input)`: a duplex card with no latency) — the recording
  tests (`tests/recording.rs`: loopback placement exact to the sample with and without a
  latency offset, punch, loop takes from one file, writer-ring overflow, MIDI pattern overdub,
  metronome + count-in, monitor modes) and `no_alloc` (recording + monitoring + metronome +
  loop + MIDI) use it; `set_midi_queue` feeds MIDI without a port.

### cadenza-app

Desktop app (`cadenza-app [project]`, or `cadenza gui [project]`, which execs the binary next to
the CLI so the CLI stays free of GPU/windowing deps). egui 0.36 / eframe with the wgpu renderer
(the crate sets `rust-version = 1.95`); native file dialogs through `rfd` (xdg-desktop-portal).

- `state::AppState` (no egui; unit-tested) holds the `cadenza_mcp::Session` as a
  `SharedSession` — the same document + undo history + path + plugin catalog + hosting the MCP
  tools and the in-app AI agent use — plus selection (ids only, pruned after edits), view
  (zoom/scroll/panels), notices, meters, jobs. Views read a snapshot (`DocView`: project +
  history labels; `state.document()`), never the lock; `state.session()` locks briefly.
- **Every edit is a `Command` through `AppState::apply`** (or `edit_session` to reuse MCP tool
  functions such as `import_audio`, `import_midi`, `save_project`): validated + undoable by
  `Document`, marks dirty and prunes the selection. All paths (and the agent's tool edits,
  picked up each frame by `sync_session(false)` with `try_lock`, so the UI never waits) end in
  `sync_session`, which refreshes the snapshot when the change key moved and runs that
  bookkeeping; saves by any owner clear the dirty flag. `state::live::diff(before, after)` decides
  how the player follows: volume/pan/send levels, device param values and automation lanes go
  live (`set_live_*`, `set_automation`, MIDI clip/note edits on instrument tracks via
  `set_track_clips`; sounding notes keep playing, also for undo/redo and shared MCP tools),
  anything structural schedules a debounced (120 ms) reload. Gesture edits
  (faders, knobs, send levels, clip moves, automation points) are previewed locally (and heard
  live) and committed as ONE command on release; value widgets only commit real user edits.
  Mixer/inspector controls show the automated value at the playhead. `state::edits` holds the
  pure gesture -> Command builders (move/resize/duplicate/split clips, left trim
  `trim_start` keeping content in place, rubber band `clips_in`, new tracks/clips, reorder).
- Automation (`state::automation` builders + `ui::automation`; rows laid out by
  `state::rows`): per-track and master lanes expand under their channel (A key, header "A"
  toggle, track menu "Automate"), lane picker (volume, pan, sends, every device param by
  name), click adds, drag moves (snap; Alt free; Shift vertical), rubber band (Shift adds),
  Ctrl+drag ramp, right-click/Del deletes, double-click cycles linear/hold/smooth, tooltip in
  real units. Right-click "Automate" on mixer faders/pans/sends and inspector controls creates
  and shows the lane. Tempo lane (transport ♩ toggle, pinned under the markers): step curve
  of the `TempoMap`, click adds a change, drag moves, right-click removes; the BPM field edits
  the segment under the playhead. Time signatures (phase 17, `state::meter` builders): the
  transport TIME field edits the signature in effect at the playhead; the signature lane
  (shown with the tempo lane, above it) has a chip per signature, drag a change to another bar,
  right-click a bar = presets / custom / remove. Ruler numbers, grids (`Timeline::grid_lines`:
  bar/beat/step lines restarted at every bar line), snapping (`snap_meter`), bars.beats
  display, pattern length, new clips, loop-the-bar, export bar ranges and MIDI clip growth all
  follow the changes; the ruler tags each change.
- Automation recording (phase 17, `state::autowrite`): transport "A: Read/Touch/Latch" button
  (click cycles, right-click picks; `AppState::set_write_mode`). While playing in Touch/Latch,
  `preview_volume/pan/send/param` and plugin editor values (`editor_updates`: Value = write,
  GestureEnd = release) write `(position, value)` samples per frame into a `Writer` pass and the
  `commit_*` calls release instead of committing a static value; Touch returns to the old
  curve (or static value) one tick after the release, Latch keeps writing the last value until
  stop. Lanes being written are suspended in the player (`sync_write_suspension`). When playback
  stops (or the mode goes to Read) the pass becomes ONE undo step "Write automation" (Batch of
  `SetAutomation`, each stretch replacing the points inside it, thinned by Ramer-Douglas-Peucker
  at 1 % of its range; a loop wrap starts a new stretch so the later pass wins). Nothing of
  this runs on the audio thread.
- Freeze / bounce / consolidate (phase 17, `state::bounce` builders, renders on a worker
  `jobs.bounce`, one undo step when done): track menu Freeze / Unfreeze (FRZ badge on the
  header), Bounce in place (new audio track "<name> (bounce)" below with the render from the
  bar of the first clip, same volume/pan/output/folder, original muted); clip menu
  "Consolidate selected clips" (two or more clips of one track: MIDI merged at once with notes
  and controller lanes, audio rendered without inserts into one clip). Renders go to
  `.cadenza/freeze/` (freeze) or `Audio/` (state folder before the first save). MCP:
  `freeze_track`, `unfreeze_track`; summaries show `frozen`.
- Folder tracks (phase 17, `state::folders` pure builders): "+ Folder" track button, track
  menu "Group into new folder" / "Take out of folder"; headers indent with a guide line per
  folder level, folders have a disclosure triangle (`view.collapsed`, `toggle_folder`) and a
  row that summarizes the clips inside; collapsed contents are hidden in the arrange
  (`Rows::hidden`) and the mixer (folder strips: fold toggle, VCA fader, M/S; no inserts,
  sends, output or pan). Dragging a header (`move_track_to` -> `folders::move_track`, one undo
  step) moves a folder with its contents and files the track: right under an open folder =
  into it, else into the folder of the row above. MCP: `set_track_folder`, create_track kind
  folder.
- Audio editor (`state::audio_edit` + `ui::audio_editor`; double-click an audio clip): shares
  the bottom editor dock with the piano roll (`view.audio_editor`; F7 toggles the dock).
  Source-file time axis, peaks when zoomed out and decoded samples (`PeakCache::samples`) when
  zoomed in; region start/end handles, fade handles + curve pickers, clip gain pill, selection
  with split / trim / delete range (clip commands). Normalize/reverse/gain (and time-stretch/
  pitch with the app's `stretch` feature) render a processed copy on a worker into
  `<project dir>/<project> audio/` (user cache when unsaved) and swap it in with one undoable
  `UpdateAudioClip` batch; the original file is never modified. Overlapping audio clips crossfade
  automatically (`edits::crossfades` mirrors the engine rule): the arrange draws the fade-out/
  fade-in curves, shades the cut rest of the overlap and puts a length handle at the crossfade
  end (drag, snapped unless Alt; double-click resets to the whole overlap; one `SetClipFades`
  on the later clip, `edits::set_crossfade`); nested clips that sum keep the X mark.
- `audio::AudioEngine`: the `Player` on its own thread (like MCP `PlayerHandle`) but never
  blocking the UI: requests are fire-and-forget, a burst of loads is coalesced into the newest,
  results come back as polled `AudioEvent`s; position via `PlayerClock`, levels via `MeterBank`,
  plugin crashes polled every 500 ms and shown as toasts. No device = app works without sound.
- `jobs`: waveform peaks (decoded + `WaveformPeaks` on a worker, cached by path), plugin
  rescans, exports, file copies and file dialogs (files and folders) run off the UI thread.
- Project files (phase 14; `state::safety`, `state::export`, `state::project_io`,
  `ui::project_dialogs`): File menu New / Open (projects and `.cadenza.zip`/`.cadenzapack`
  archives, extracted into a chosen folder) / Open Recent (persisted, 10) / Start Screen / Save /
  Save As / Import / Export… (Ctrl+E) / Collect Project Files / Locate Missing Files / Export
  Project Archive / Autosave interval. Start screen (no project on the command line): New,
  Open, Demo song (bundled `examples/first_light`, written to the state folder), recent list.
  Export dialog: range (whole song, loop region, selected clips, bars) + tail, mixdown, stems
  (every track, buses + direct tracks, chosen tracks; through the master, pre-fader), format
  (WAV 16/24/32f, FLAC 16/24, MP3 when built), dither, sample rate, normalization (peak or LUFS
  + true-peak ceiling, stems linked or individual), metadata, folder (default `Renders/`), file
  name preview; runs `export_project` on a worker with a progress bar and Cancel, then shows a
  per-file loudness table and "Open folder"; settings persist in `Prefs.export`. Missing files on
  open show the Locate dialog (folder search, one undo). "Save As" to a folder that lacks the
  project's external files offers to collect them. Processed audio copies go to `Audio/`.
  Safety (off in `AppState::headless`, on via `enable_safety(state_dir)`): autosave every
  `Prefs.autosave_minutes` (default 2, 0 = off) after the first unsaved change, written on a
  worker from a project clone to `.cadenza/autosave/` (unsaved projects:
  `$XDG_STATE_HOME/cadenza/autosave/untitled-<pid>/`), rotating 5; saves clear them, "Don't save"
  deletes them. A `.cadenza/session.lock` (pid) marks an open project, removed on new/open/quit;
  opening a project whose newest autosave is newer than the file offers Recover (unsaved
  document, same path) or keeping the saved version (reason: crashed when a dead pid's marker
  is left); untitled sessions of dead processes are offered at startup.
- `ui`: views only read state and call its methods; `theme` is the single palette/metrics
  module. Panels (`state::layout::PanelLayout`, FL Studio-like keys): F5 arrange, F6 / Alt+F8
  browser, F7 piano roll, F8 inspector, F9 mixer, F11 / Ctrl+Enter maximize the focused panel
  (last one clicked), F12 close tool panels, F1 shortcut overlay (`layout::SHORTCUTS` is the
  single shortcut table; View menu shows checkmarks + keys). The piano roll docks at the bottom
  (borrowing the mixer's place, given back on close); with the arrange hidden it takes the
  center. Layout, zooms, snap, follow mode, scale, bookmarks persist per user in eframe storage
  (`layout::Prefs`, key `cadenza-prefs`; panel sizes via egui memory).
- Navigation (arrange and piano roll alike, math in `state::timeline`): wheel vertical,
  Shift+wheel horizontal, Ctrl+wheel zoom at the mouse, Alt+wheel track/key height,
  middle-drag pan (Space stays play/stop), themed scrollbars, song overview strip with a
  draggable viewport, Home/End, PageUp/Down (Shift: vertical), Z zoom to selection, Shift+Z fit,
  follow playhead Off/Page/Continuous, edge auto-scroll while dragging. Ruler/markers pinned,
  headers scroll with lanes. Clips: Ctrl+drag copies, Alt disables snap, double-click a MIDI
  clip opens the piano roll.
- Piano roll (`state::piano` pure builders + `ui::piano_roll`), FL Studio parity (phase 13b):
  absolute song time axis with the clip span marked and outside shaded, keyboard (click
  plays, drag glides), tools Draw (P) / Paint (B) / Select (E) / Slice (C). Draw does nearly
  everything: click on empty grid adds a note with the last length/velocity and keeps
  dragging it (pitch + snapped time, auditioned on press and pitch change); drag a note =
  move it (or the selection), its right/left edge = resize end/start; Shift+drag duplicates;
  Ctrl+drag rubber band, Ctrl+click toggles; Alt = no snap; right-click deletes the note,
  holding the right button deletes every note swept over (`sweep_hits`, one `RemoveNotes`);
  double-click = properties popover (pitch, velocity, start, length; `set_note`). Paint drags
  notes at every `paint_cell` (spacing = last length rounded to the snap); Slice splits
  notes at the snapped line (`slice_notes`). Snap is the piano roll's own `PianoSnap`
  ("magnet": Main follows the arrange snap; default Step = 1/16; persisted in `Prefs`).
  Keys (focused panel, consumed before global keys): Ctrl+B `clone_notes` (selection span
  widened to the snap grid, whole bars when >= a bar; copies selected so repeats append),
  Ctrl+C/X/V (`NoteClipboard`: pastes in place, or at the playhead when it was moved since
  the copy and the transport is stopped), Ctrl+A / Ctrl+D or Esc (deselect), Del, Ctrl+Q / Q
  quantize, Ctrl+U glue (selected same-pitch notes), Ctrl+L quick legato (so the global
  loop toggle is Ctrl+L from other panels), Up/Down semitone, Shift or Ctrl+Up/Down octave,
  Left/Right snap step, Shift+Left/Right bar — arrows/legato apply to all notes when nothing
  is selected (`targets`). Event lane (toolbar "Events" toggle; selector left of the lane, `PianoView::event_lane`:
  Velocity, Mod wheel, Expression, Sustain, Pitch bend, Pressure, CC 74, CC 7, any CC#, lanes
  with data marked ●). Velocity: drag paints (relative on a multi-selection), Alt/right-drag
  draws a line (`velocity_line`). Controllers (step graph, value tag under the pointer): pencil
  drag writes one event per snap cell (`controller_paint`), Alt/right-drag a ramp, right-click
  deletes the event under the pointer; switches snap on/off; one `SetControllerLane` per
  gesture (`set_controller`). Selected notes are warm red
  (`theme::NOTE_SELECTED`), brightness by velocity, names when zoom allows, dim ghost notes;
  off-screen notes are culled (2000 notes cost ~1 ms/frame). Opening a clip centers on its
  pitch range (C4 when empty). One command per gesture; editing past the clip end grows the
  clip to the next bar in the same batch (notes before the start are clamped). Notes are
  auditioned through `AppState::preview_note_on/off`, released on mouse up, panel close and
  track change.
- Transport modes (`state::transport`, view state, not in the project): FL-like PAT | SONG
  switch (L). Song mode plays the arrangement, looping the arrange loop region when on
  (Ctrl+L or the ⟲ button; drag the ruler's upper half or Ctrl+drag the ruler to set it, click
  the region to toggle; drawn on ruler and lanes). Pattern mode loops the clip open in the
  piano roll and auditions only its track (`Player::set_audition`); pattern length (FL):
  notes rounded up to the next bar (min one bar) or the clip length if longer
  (`transport::pattern_range`), so notes drawn past the end grow the loop live (no
  restart). Play starts at the pattern start unless the playhead was placed inside it since
  the last stop (`AppState::seek` cues, internal `locate` does not); Stop rewinds to the
  pattern start. No clip open = song with a toast. `AppState::sync_transport` (every tick) sends loop/audition only
  when they change; switching modes while playing restarts; audition only while playing.
- Piano roll access: F7 / double-click an instrument track header / track menu open the
  selected instrument track's clip (`piano::clip_for_track`: selected, under the playhead,
  last opened (`PianoView::recent`)) or create an empty one-bar clip named after the track at
  the first free bar from the playhead (`piano::new_clip`, one undo step). Adding an
  instrument selects its track and hints at F7; the piano roll toolbar has a clip menu (the
  track's clips) and "+" (new clip); the empty dock explains how to open one.
- Drag & drop (`state::dnd`: `Payload` x `DropTarget` -> `DropAction`, executed by
  `AppState::drop_payload`; views only hit-test and show the hint next to the pointer):
  devices -> track/lane (instrument or insert), mixer insert slot, below the tracks (new
  instrument track / bus); presets -> device or track; mixer inserts reorder / move to another
  strip (Ctrl copies); browser Files tab (bookmarks, audio/MIDI filter) and OS file drops:
  audio/MIDI import at the drop beat/track (new tracks below), `.cadenza.json` opens (dirty
  prompt). Other shortcuts: Space, Home, Ctrl+Z/Shift+Z, Ctrl+S/Shift+S/O/N/I, Ctrl+E (export dialog), Ctrl+D, S
  split, Del (automation points first, then clips), A lanes; arrange: drag empty lane =
  rubber band, drag a clip's left edge = trim start. Dirty state is confirmed on new/open/close.
- Sampler editors (`state::samples` pure builders + `ui::instrument_editor`): the bottom editor
  dock shows them when `view.instrument_editor` (opened by the inspector "Edit pads/zones",
  double-click on the instrument in a track header or mixer strip; "Piano roll" goes back).
  Drum sampler: 4x4 pad grid (pad 1 bottom-left; click auditions through the preview path,
  higher = harder), selected pad's waveform (`WaveformPeaks`) with draggable start/end, pad
  knobs (`widgets::knob`, one command per drag) and the device's param knobs (live). Sampler:
  keyboard zone map (click selects, keys audition), start/end/loop markers, zone knobs, loop
  mode toggles, global knobs. Audio file drops (browser Files or OS) on a pad / zone / the
  keyboard go through the shared `load_sample` tool; Alt+drop below the tracks or the Files
  context menu "Load into new sampler" adds a sampler track auto-mapped to the file.
- Style library (phase 18a; `state::library` pure builders + `ui::library`): browser
  "Library" tab (`AppState::library()` = the session's `Library`; `headless()` uses the
  core pack only; `new()` loads user packs and warns about broken ones via
  `report_library`). Search box + filter combos (kind, genre, role, section, energy; Clear),
  cards (kind glyph, name, role/kind tag, summary, facts, `pack · license`; pattern tooltip
  = the step grids). Click a pattern = audition it once at the song tempo with the default
  harmony (`default_harmony`: the song style's first loop, else i-VI-III-VII; A minor /
  C major) on a fitting instrument track (`preview_track`) through `preview_note_on/off`
  (`PatternPreview` events sent by `tick`; refused while playing; click again stops);
  double-click writes it at the playhead; drag = `Payload::Pattern` -> `DropAction::
  AddPattern` on an instrument lane/header (from the bar under the pointer, ghost clip
  shown) or below the tracks (new Drums/Bass/Keys/Pad/Lead track with a fitting built-in
  and preset): `library::add_pattern` = one Batch "Add pattern '<name>'" via the MCP
  `build_pattern_clip` (4 bars minimum, `drop_bars`). Styles, forms, progressions and
  grooves open as read-only documents ("‹ Library" back): style = feel, harmony loops,
  forms with Apply (markers + typical tempo + song style, `apply_library_form`), roles
  with draggable/previewable pattern chips, sounds, mix, do/don't; form = Apply form +
  section table with energy blocks. "Credits" lists every pack (license, attribution,
  links, not-redistributable flag, location) and load problems.
- Mix assistant (phase 18b; `state::mix` logic + `ui::mix_assistant`): the AI panel (F10)
  has PRODUCER | MIX tabs (`mix.open`; AI menu "Mix assistant"). Analyze = `run_analysis` on a
  `cadenza-mix-analysis` thread (progress, Cancel, waker), report card: master LUFS vs target
  (style or override), true peak/crest/width/low-end mono, octave spectrum bars, tone vs the
  profile curve, per-channel loudness bars + peak tick, masking pairs, issues with Fix (the
  issue's moves via `mix_pass_command`, one undo step "Mix fix: ...") and Fix all ("Mix
  fixes"); a stale report (document changed) offers Analyze again. "AI mix pass": measure
  (reuses a fresh report), `AiChat::send_mix_pass` -> `SessionAgent::run_engineer_pass`
  (reference track via File dialog `FileAction::MixReference`, notes), measure again,
  `compare_passes` table with Keep / Revert (undo, only while "AI mix pass" is the last step).
  Library style documents show the mix profile and role levels.
- Fonts: `theme::fonts()` appends egui's bundled Hack to the proportional family (arrows,
  geometric shapes, block elements: `← → ◆ ● ⊘ ▍`); `proportional_fonts_cover_the_app_symbols`
  scans the sources for non-ASCII symbols and checks each against the faces' charmaps.
- Tests: state unit tests, `tests/ui.rs` drives the whole window headless with `egui_kittest`;
  `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --test ui -- --ignored` renders PNGs (wgpu),
  including automation lanes, the audio editor, an empty project, (`render_sampler_screens`)
  the sampler editors, (`render_project_screens`) the start screen, export dialog/progress/
  result, Locate missing files and the recovery prompt, and (`render_recording_screens`) armed
  tracks with take lanes, a recording in progress (fed by the engine's virtual device) and the
  audio settings dialog, and (`render_phase17_screens`) time signature changes + the signature
  lane, controller lanes (mod wheel, pitch bend), an automatic crossfade and a folder (open,
  folded), (`render_mix_screens`) the Mix tab (empty, analyzing, report, issues, pass running, review) and the Files tab glyphs, and (`render_library_screens`) the Library tab (list, filters, preview, pattern
  tooltip, style and form documents, credits, a pattern dragged over a lane).
- Plugin editors (`state::editors` pure logic, `audio::AudioEngine` polls open editors every
  30 ms): Edit button in the inspector (device and track instrument), double-click a plugin
  insert / instrument in the mixer or the instrument name in a track header. Window title
  "<device> — <track>". A gesture = one `SetParam` on release (live meanwhile, shown by the
  inspector); one-off values are batched after 350 ms quiet (>16 params: captured state
  instead); `StateChanged`, close and save capture the live state -> one `SetDeviceState`
  when it differs. States the running instance holds are live in `live::diff` (no reload);
  every reload clears that. Editors opened before the device is loaded wait for the reload.
  The generic editor (inspector sliders) has a filter and "show all" for large plugins.
- Recording (phase 16; `state::record` logic + `ui::record` views, `ui::transport`): arm states
  are session state (`RecordState.armed`, mirrored to the player, kept across device reopen);
  ● on track headers and mixer strips (arming an audio track without an input gives it In 1,
  one undo step; armed instrument tracks get the live MIDI). Audio headers: input selector (mono
  / stereo pairs of the open input, or none), speaker monitor button (`ui::record::
  monitor_button`: click on/off, orange when on, Auto = dimmer outlined speaker with an "A",
  right-click Off/Auto/On), input level as a thin vertical meter on the header's left edge
  next to the color strip. Transport: red record button (Ctrl+R start/stop; R = record mode,
  Play records the armed tracks, nothing armed = plays). Toggling record while playing never
  stops the transport: off = stop capturing (`Player::end_recording`), on = punch in on the
  fly from the playhead (no count-in, no relocate; `Player::start_recording` while playing
  skips the count-in). The metronome toggle switches the click for playing and recording
  alike (right-click: only one of them, level); defaults are no click and no count-in
  (`RecordSettings::revision` 2 migrates older prefs), count-in 0/1/2/4 bars, PUNCH (record only inside the loop region; with the
  loop on: loop recording, a take per pass), "MIDI rec" menu (merge / overdub / takes per pass /
  input quantize); position display turns red while recording. `record_plan()` picks start,
  punch-out and pre-roll (pattern mode: the pattern loop, MIDI into the pattern clip, its track
  armed if nothing is). The recording is drawn live over its lanes (waveform from the writer's
  peaks, MIDI notes) and the finished one lands as one undo step "Record" (`finish_recording`);
  MIDI loop passes are merged into the player's copy of the track as they complete (FL overdub:
  heard on the next pass) without touching the document. Takes: T on the header expands take
  lanes (`view.takes`, `RowKind::Take`); click a take = play it for its region, drag = swipe-comp
  that range (snapped), double-click a lane header = play that take everywhere, menu: flatten /
  delete take; the main lane shows only what plays, with a take-count badge; S on a take splits
  its whole group. Audio settings (File menu): output/input device, sample rate, buffer size,
  input on/off, MIDI input, latency offset (ms), status (rates, clock mode, latencies), test tone
  (off by default, switched off on close); persisted in `Prefs.audio` (+ `Prefs.record` for
  metronome/count-in/punch/MIDI options); new devices reopen the player on its thread
  (`AudioEngine::configure`, device lists probed there). The AI context carries the armed
  tracks/metronome/count-in (`recording_context`). Recordings go to the project's `Audio/`, or
  `$XDG_STATE_HOME/cadenza/recordings/untitled-<pid>/` before the first save.
- AI producer chat (phase 12; `state::ai` logic, `ui::chat` view): F10 / View / AI menu / F1.
  Docked right (`PanelKind::Chat`). `ai::worker` is a thread with its own tokio runtime that
  owns the `SessionAgent` (conversation kept across requests) and everything that may block
  (model calls, keyring); the UI sends commands and polls events (`AiChat::poll`) each frame.
  Chat rows: user bubbles, streamed markdown replies (`ai::markdown`: headings, lists, code,
  bold/italic/code spans), collapsed thinking, compact tool rows (✔/✖, expandable args +
  result), notices, actionable errors (missing/rejected key opens the settings), a footer per
  request (status, cost, session total, undo label); quick-action chips from the selection;
  Enter sends, Shift+Enter new line; ☰ menu: new conversation, settings, transcript path.
  **Edit lock decision**: while a request runs, `apply`/`edit_session`/undo/redo/new/open
  are refused with a notice and the edit panels draw disabled (transport stays live);
  plugin-editor values and finished background edits go through `apply_or_queue` and are
  applied after the request. Each request is one undo step "AI: <request>" (document undo
  group opened/closed by the agent however the request ends); Stop (`CancelToken`) ends at
  the next tool boundary or aborts the model call, keeping the edits as that one step.
  Settings (`ai::settings::AiSettings`, persisted in `Prefs.ai`, no secrets): provider
  OpenRouter | OpenAI-compatible (base URL), model (presets + free text), reasoning effort,
  budget per request, optional price for estimates, allow plugins. Keys (`ai::secrets`):
  env var (`OPENROUTER_API_KEY` / `OPENAI_API_KEY`) > nearest `.env` > stored key; stored keys
  go to the OS keyring (`keyring` 4, Secret Service via zbus) or, when no secret store runs,
  `$XDG_CONFIG_HOME/cadenza/credentials.json` (0600); never shown in clear (password field).
  Transcript + files root for unsaved projects: `$XDG_STATE_HOME/cadenza/sessions/chat-<t>-<pid>/`;
  saved projects use their folder. Headless states use an in-memory key store; tests use
  `cadenza_agent::scripted::ScriptedProvider` (no network).
