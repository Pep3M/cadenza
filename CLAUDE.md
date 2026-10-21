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
| `cadenza-core` | ids, `Tick`/`PPQ`(960), `TempoMap`, `TimeSignature`, `ParamAddress` URIs, dB helpers, per-user folders (`paths`) | — |
| `cadenza-model` | project document, `Command` enum, undo/redo `Document`, persistence, AI summary | core |
| `cadenza-theory` | notes, scales, chord symbols, roman numerals, voicings | core |
| `cadenza-dsp` | real-time `Processor` trait + built-in instruments/effects registry | — |
| `cadenza-audio-io` | audio decoding (symphonia), rubato resampling, `AudioCache`, waveform peaks, realtime clip DSP (fades/regions), export WAV/FLAC (+MP3 `mp3` feature) with metadata (`tags`: WAV LIST/INFO, FLAC Vorbis comments, MP3 ID3v2.4), stretch (`stretch` feature) | — |
| `cadenza-library` | style library: packs of style guides, forms, progressions, grooves, MIDI patterns (embedded `core` pack + user packs), validator, search, `realize` | core, theory |
| `cadenza-analysis` | offline mix analysis: levels/BS.1770 loudness/true peak/bands (`Analyzer`, `LevelStats`), FFT third-octave spectrum + stereo image + octave timeline (`detail`), masking, issues with fix moves, mix targets, roles, reference comparison | core |
| `cadenza-midi-io` | SMF import (-> undoable `Batch`) / export, hardware MIDI input (midir -> rtrb), recorder | core, model, theory, dsp |
| `cadenza-engine` | compiles a `Project` into a render graph; offline render/export (mixdown + stems jobs with progress/cancel), mix analysis (`analyze_mix`, `analyze_mix_detailed`), realtime playback (cpal) + live MIDI monitoring, audio input + input monitoring, recording (WAV takes, MIDI), metronome + count-in | core, model, dsp, audio-io, midi-io, analysis |
| `cadenza-mcp` | MCP server (rmcp, stdio) exposing the command API as AI tools | model, theory, engine, dsp, audio-io, midi-io |
| `cadenza-plugin-host` | CLAP (clack) + VST3 hosting, out-of-process scanner, catalog, per-plugin sandbox process (Unix: shm + futex / macOS `os_sync`), plugin editor windows (X11) | dsp |
| `cadenza-agent` | embedded AI producer: provider-agnostic `LlmProvider` tool loop (OpenRouter + generic OpenAI-compatible), budget cap, JSONL transcript; `Agent` (compose from a brief) and `SessionAgent` (in-app chat on a shared session) | mcp |
| `cadenza-cli` | `cadenza` binary: `render`, `export` (mixdown/stems), `collect`, `relink`, `pack`, `unpack`, `analyze`, `play` (`--midi-in --monitor`), `monitor`, `midi-ports`, `import-midi`, `export-midi`, `mcp`, `scan-plugins`, `list-plugins`, `compose`, `chat` (one AI request on a project), `mix` (AI sound-engineer pass: `--target-lufs`, `--reference`, `--notes`, `--dry-run`, `-o`; analysis before/after + changed numbers), `library list|validate|import` (style library packs; MIDI -> pattern), `gui` (starts `cadenza-app`) | all |
| `cadenza-app` | `cadenza-app` desktop GUI (egui/eframe, wgpu): transport, arrange, piano roll, mixer with live meters, browser (devices/presets/files), inspector, AI producer chat; `AppState` over the shared MCP `Session` | core, model, theory, dsp, engine, mcp, agent, plugin-host, audio-io, midi-io |
| `cadenza-test-plugins` | tiny CLAP/VST3 plugins for tests (gain, sine, crash; routing: sidechain ducker with presets, multi-output; `clap_gui` scripted editor; VST3 gain with a scripted `IPlugView` and an `IUnitInfo` program list) | — |

Future: sandbox on Windows, plugin editors on Windows/macOS (HWND/NSView), native Wayland plugin GUIs.

## Platforms, CI and releases

| | Linux | macOS (arm64 + x86_64) | Windows (x86_64 MSVC) |
|---|---|---|---|
| build, tests | yes | yes | yes |
| plugin sandbox | futex | `os_sync_wait_on_address` (14.4+, `dlsym`), else polling | no (`SANDBOX_SUPPORTED` false): `PluginHosting::from_env` = in-process, logged once |
| plugin editors | X11/XWayland | no | no (`EDITORS_SUPPORTED` false: inspector "Edit" disabled with a hint, generic controls) |
| realtime promotion | `SCHED_FIFO`/RealtimeKit | Mach time-constraint policy | `THREAD_PRIORITY_TIME_CRITICAL` |
| per-user folders | XDG | `~/Library/{Application Support,Caches}` | `%APPDATA%` / `%LOCALAPPDATA%` |

Folders come from `cadenza_core::paths::{data,config,cache,state}_dir()` (each ends in
`cadenza`; an absolute `XDG_*_HOME` wins on every OS, which tests use). Canonicalize user
paths with `paths::real_path` (no Windows `\\?\` prefix; tests compare against it since
macOS temp dirs resolve to `/private/var` and Windows ones expand `RUNNER~1`). VST3 class ids
are the SDK's printed form on every OS (`vst3::com::tuid_to_hex` swaps Windows' COM GUID
fields), so ids in projects are portable. Repo file names must differ beyond case
(`core_pack_file_names_differ_beyond_case`). Sandbox and editor
integration tests are `#![cfg]`-gated to the platforms that have them; `plugin_support::
sandboxed()` is in-process on Windows so the parity tests still run. `plugin-host` allows
dead code off Linux (that plumbing is type-checked, not wired). Cross-check locally without
the platform: rustup targets + `cargo check/clippy --target ...` (C build scripts need a
target C toolchain or a stub `CC_<target>`); CI is the real check.
CI (`.github/workflows/ci.yml`, toolchain pinned in `RUST_TOOLCHAIN`): Linux fmt/clippy
`-D warnings`/`cargo test --release` + `cargo deny check` (`deny.toml`: permissive licenses +
MPL-2.0 + egui's font licenses; Cadenza crates are `GPL-3.0-or-later`, `publish = false`, see `LICENSE`) on every push/PR; Windows and
macOS clippy + release tests (+ `cargo check --target x86_64-apple-darwin`) only on PRs to
main/staging, pushes to main, `v*` tags and `workflow_dispatch` (`platforms` input); docs-only
changes skipped, superseded runs cancelled. No secrets: AI tests use the scripted provider.
Release (`release.yml`, `v*` tags or manual; `packaging/`; every release bumps `[workspace.package] version` and adds its section to `CHANGELOG.md`): Linux tar.gz + AppImage (built on
ubuntu-22.04), Windows zip (no sandbox binary), macOS universal `Cadenza.app` (lipo, ad-hoc
signed) in a dmg; each ships `cadenza`, `cadenza-app`, `cadenza-plugin-scanner` (+
`cadenza-plugin-sandbox` on Unix) side by side, plus LICENSE/README. Tags publish a GitHub Release
(prerelease when the tag has a `-suffix`) with `SHA256SUMS` and the installers (`install.sh`:
Linux/macOS, `install.ps1`: Windows; both read `releases/latest`, verify the checksums and put
wrapper scripts, not symlinks, on the PATH); a manual run with `draft_release=<tag>` makes a draft
(no tag). AUR: `packaging/arch/PKGBUILD` (`cadenza-bin`, repackages the release
tar.gz into `/opt/cadenza` + `/usr/bin` symlinks); on a release bump `pkgver`, reset `pkgrel`,
update `sha256sums` (from `SHA256SUMS`), regenerate `.SRCINFO` (`makepkg --printsrcinfo`) and
push both to the AUR repo. README banner: `docs/assets/banner.png`, rendered from `banner.html` (headless Chrome, 2x).
Icons: `packaging/icons/generate.py` (rsvg-convert + iconutil) writes `cadenza.svg` (also
`packaging/linux/`), `cadenza-macos.svg` (Apple grid: 824/1024 body + shadow) -> `cadenza.icns`
(bundle `CFBundleIconFile`), `cadenza.ico` (embedded in the Windows exes by the app/cli `build.rs`
with `winresource`) and `cadenza-256.png` (window icon, `include_bytes!` in the app); outputs are committed. TODO: Developer ID signing +
notarization (macOS), Authenticode (Windows).

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
playhead, loop, open clip, open device editor, files root, armed tracks/metronome/count-in — recording itself stays with the
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
	pub armed: Vec<TrackId>,           // armed for recording, arming order (optional in JSON; stale ids dropped on load)
	pub midi_map: Vec<MidiMapping { controller: Controller, channel: Option<u8 /* 0..=15, None = any */>, target: ParamAddress,
		min, max: f32 /* plain values */, mode: absolute|relative /* encoders, CCs only */, takeover: jump|pickup }>, // T36, optional in JSON
}
pub enum TrackKind { Instrument, Audio, Bus, Folder }
pub struct Track { id: TrackId, name, kind, color: Option<String /* #RRGGBB */>,
	instrument: Option<Device>, clips: Vec<Clip>, channel: Channel,
	folder: Option<TrackId> /* the folder track holding it; optional */,
	freeze: Option<Box<Freeze { file: PathBuf /* stereo render from tick 0 */, source: Option<String> /* fingerprint */ }>> /* optional */,
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
	zones: Vec<SampleZone> /* built-in sampler / drum_sampler only */,
	modulators: Vec<Modulator> /* built-ins only, optional in JSON */ }
pub struct Modulator { id: ModulatorId, kind /* flattened, tag "type" */: Lfo { shape: sine|triangle|saw|square|random,
	rate_hz, sync_beats: Option<f32> /* quarter notes */, phase: 0..1, retrigger: free|note|transport }
	| Envelope { source: own_input|sidechain, attack_ms, release_ms, gain_db }, targets: Vec<ModTarget { param, depth: -1..1 of the range }> }
pub struct SampleZone { sample: SampleRef { path: PathBuf /* absolute in memory */, name: String /* original file name */ },
	key_low, key_high, root_key: u8, vel_low, vel_high: f32 /* 0..1 */, tune: f32 /* semitones */, gain_db, pan,
	start: f64, end: Option<f64> /* seconds of the file */, loop_mode: LoopMode /* off|forward|ping_pong */,
	loop_start: f64, loop_end: Option<f64>, loop_crossfade: f64, reverse: bool, attack_ms: f32, decay_ms: Option<f32>,
	choke: u8 /* 1..=16 */, output: u8 /* 0 main, n = extra output n */, round_robin: u8 /* 1..=16 */ }
// New fields are optional in JSON (missing = None/empty), so format_version stays 1.
pub enum DeviceKind { Builtin { id: String }, Clap { plugin_id: String, path: PathBuf },
	Vst3 { class_id: String, path: PathBuf } }
pub struct Clip { id: ClipId, name: String, start: Tick, length: Tick, content: ClipContent,
	take: Option<Take { group: TakeGroupId, lane: u32 /* 0 = first take */, active: bool }>,
	muted: bool /* omitted when false; `UpdateClip { muted }`; `Clip::sounds()` = plays() && !muted: engine + MIDI export skip it */,
	link: Option<LinkId> /* linked MIDI copies (FL pattern instances), optional */ }
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
RemoveTimeSignature { bar }, SetArmedTracks { armed } (audio/instrument tracks, no repeats;
RemoveTrack disarms), SetMidiMap { map } (replace; <= 256, built-in params / volume / pan / sends only,
one controller per target and one target per controller+channel; removing a target drops its
mappings like its automation lanes, undo restores; stale ones dropped on load), SetTrackFreeze { track, freeze: Option<Freeze> } (instrument/audio only), SetTrackFolder { track, folder: Option }, SetControllerLane { clip, controller, points } (replace; empty removes), SetAutomation (replace lane; empty points removes it), AddMarker, RemoveMarker,
SetDeviceState { track, device, state, params: Option<map> /* Some = replace all params */ },
SetSampleZones { track, device, zones } (replaces the zone map; built-ins only),
SetModulators { track, device, modulators } (replace; built-ins only; <= 8 modulators, <= 16 targets,
unique ids/params, ranges in `model::modulation`, sidechain source needs `Device.sidechain`; param
existence is the caller's check like SetParam; summary `modulators: ["LFO sine 1/8 -> filter_cutoff +0.30"]`),
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

Linked clips (T39, `model::links`): clips sharing `Clip::link` hold identical MIDI content
(validated on AddClip / `UpdateClip { link }` (double option, `null` = unique) and on load);
AddNotes/RemoveNotes/UpdateNotes/SetControllerLane on one copy are applied to every copy in
the same step (`apply::sync_link`), so undo restores all. Lengths may differ: shared notes
must start inside the longest copy (`validate::content_span`), shorter copies just stop
earlier (engine/export skip notes past a clip's end; load validation does not bound linked
notes). Builders: `copy_clip` (linked copy; an unlinked source joins `new_link`),
`make_unique` / `unique_commands` (also frees the last remaining copy; a copy holding shared
notes past its end is replaced by a trimmed one with the same id), `link_count`;
`takes::split_clip` unlinks first. Summary: `link: [other clip ids]`; MCP `duplicate_clip
{ linked }` (default false), notes/controller tool descriptions mention linked copies.
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
low_pass}, compressor, limiter, send {bus_id | effect: reverb|delay}) is the fix vocabulary;
a send by `effect` targets the song's return (bus carrying that built-in, `return_bus`) and
creates "Reverb"/"Delay" (100 % wet) in the same batch when missing, only raising an existing
send (the narrow-image issue proposes two reverb sends). `reference::compare(mix, ref)`:
loudness/crest/width deltas, octave shape deltas after level matching, findings, master EQ
moves. `masking::collisions_for(target, inputs)` / `issues::track_collisions(tracks, id)` (T32): every other
track colliding with one track, per octave band (EQ collision view). `match_eq` (T31): `match_target(reference, source)` (third-octave reference - source, 5-tap
smoothed, level matched over 63 Hz-12.5 kHz, near-silent bands 0) and `fit(target, &MatchOptions { amount 0..1,
max_gain_db (6), max_bands (5) }) -> MatchFit { bands: [MatchBand { kind, freq_hz, gain_db, q }], target_db,
fitted_db, before_db, residual_db }`: greedy (LS, 3 bells, HS on sixth-octave grids, least-squares gains) then 3
refinement passes; RBJ shapes at 48 kHz (`band_response_db`). moves. Engine: `analyze_mix_detailed(project, opts, progress) -> DetailedMix { report,
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
`response` module (device editor pictures, pure/allocation-light, reusing each processor's own
coefficient/shaping code so the picture and the sound cannot drift): `eq_response_db`/
`eq_band_response_db`/`EqBands` (7-band EQ magnitude), `poly_synth_filter_response_db`/
`sampler_filter_response_db` (`util::svf::svf_magnitude_db`: exact SVF transfer function),
`saturator_shape` (static waveshaping curve), `delay_sync_steps`/`delay_time_seconds`,
`compressor_curve_db`.
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
Wavetables (T35, `util::wavetable`): poly_synth `oscN_wave` 4 = Wavetable (appended choice; defaults
bit-identical, pinned by `poly_synth_defaults_stay_bit_identical_with_wavetables`) with `oscN_table`
(Basic sine→tri→saw→square, PWM, Formant vowels, Harmonics sweep, Digital sync/crush) and
`oscN_position` 0..1 (smoothed; automatable/modulatable). 5 tables × 32 frames, generated once per
process (`tables()`, own radix-2 FFT, `PolySynth::new`) as 10 mipmaps by harmonic truncation (512..1
harmonics, `Table::level_for(inc)` keeps them under Nyquist, chosen at control rate); linear between
samples and frames. `response::wavetable_frame` draws a frame from the same tables. Presets "Vowel
Pad", "Digital Pluck"; MCP `get_device_info` lists `wavetables` for poly_synth.
Performance controls (`util::expression::Expression`): poly_synth, electric_piano and sampler
honor the sustain pedal (`VoiceSlot::key_up/pedal_up`: released keys keep sounding until pedal
up; AllNotesOff lifts it), pitch bend (±2 semitones), mod wheel (5.5 Hz vibrato up to ±0.5
st) and channel pressure (poly_synth/sampler: filter +24 st); drums ignore them. Plugins:
CLAP ports that accept MIDI get raw CC/bend/pressure bytes (`ControllerRoute::Midi`), CLAP-only
ports get per-note tuning/pressure expressions of the held keys (other CCs dropped); VST3
controllers become parameter changes through `IMidiMapping` (queried at instantiation, 128 =
pressure, 129 = bend); the sandbox shm carries them (EV_CC/EV_BEND/EV_PRESSURE). Live MIDI input
maps CC/bend/pressure to these events (`midi_io::input`).
T34 (all params appended, defaults bit-identical: `dsp::tests::new_params_at_their_defaults_keep_the_sound_bit_identical`
FNV hashes, Linux x86_64 only): `reverb` `duck` (0..24 dB) + `duck_release` = wet ducked by a 5 ms-attack peak follower
on the dry input or the sidechain (the reverb is keyable; `reverb_duck_db`: 0 below -40 dBFS, full from -10), reading
`GAIN_REDUCTION_DB` = duck dB. `delay` `taps` 1..4 + `tap_spread` + `tap{2,3,4}_level/_pan`: extra read heads on the same
lines at time x (1 + (n-1) x spread), balance-panned, repeating with the line feedback (ping-pong: each tap reads both
bouncing lines); `response::delay_taps` is shared with the editor. `limiter` `oversample` 0/1/2 = 1x/2x/4x: polyphase
Kaiser FIR up/down (`OS_TAPS` 32 per phase, +32 samples latency via `latency()`, `limiter_latency_samples`), the same
gain path + 4x true-peak detector at the high rate; every mode preallocated. `param_changes_latency(device, param)`
(limiter oversample): `Player::set_live_param` returns false and `live::diff` reloads, so PDC is recompiled.
The built-in `compressor` has a sidechain input (detector keyed by it when set); readings `GAIN_REDUCTION_DB`,
`KEY_LEVEL_DB` (detector key, instant peak + 20 dB/300 ms fall). Listen (phase T27): `Processor::set_listen(Listen
{ Off | Key | Band(u8) }) -> bool` (default no-op, realtime, monitoring only, not state): compressor `Key` = output the
detector signal; `eq` `Band(b)` = the input isolated (`eq::listen_coeffs`: bell = 0 dB band-pass at freq/Q, shelves =
24 dB/oct LP/HP at the corner, HP/LP bands = what they cut). `reverb` `freeze` param (1 = lossless loop: unit gains,
no damping, input muted; 50 ms glide; tail_seconds unchanged).
Key filter + dynamic EQ (T30): compressor `key_hp` (20 = off ..10 kHz) / `key_lp` (100..20000 = off), 12 dB/oct on
the detector only (own input or sidechain; Listen Key hears it filtered), `compressor::key_filter_coeffs` shared with
`response::compressor_key_response_db`. `eq`: shelves + peaks get `<band>_dyn` (0/1), `_threshold` (dB, -24),
`_ratio` (3) + shared `dyn_attack`/`dyn_release` (params 18..34, appended: old projects unchanged, defaults
bit-identical); law: band at 0 dB below threshold, above it moves toward its gain by `over * (1 - 1/ratio)` (6 dB knee,
the compressor's gain computer), never past it (gain = depth); detector = the band's listen filter of the input, or
the sidechain (`eq` now `has_sidechain`). Readings `EQ_BAND_GAIN_DB_START..+EQ_BANDS` = each band's gain now;
`response::{EqBands::with_offsets, eq_response_with_offsets_db, dynamic_band_gain(_at), dynamic_param_indices}`.
Mid/side EQ (T31): every `eq` band has `<band>_ms` (`hp_ms`..`lp_ms`, params 35..41, 0 stereo | 1 mid | 2 side;
appended, defaults bit-identical). Only while some band is not stereo the EQ encodes M = (L+R)/2, S = (L-R)/2 (stereo
bands filter both, mid/side bands one) and decodes; switching domains resets the filter states; a mid/side dynamic
band detects on its channel; Listen on a mid/side band hears only that channel. `response::{EqChannel,
channel_param_index}`, `EqBands::{channel, uses_mid_side, channel_response_db(side, f)}`.
`drum_machine` plays every GM percussion note 35-81 (phase 18b): kit, cymbals (china, splash,
ride bell) and synthesized hand percussion in `instruments::percussion` (rim, tambourine,
cowbell, bongos, congas, timbales, agogo, cabasa, shaker, whistles, guiro, claves, woodblocks,
cuica, triangle; 8-voice pool, choke pairs; params `perc_tune/perc_decay/perc_level`);
`drum_machine::plays(note)` drives the MCP silent-lane warnings.
`drum_sampler` publishes one note-on counter per pad of `PADS` (`readings::DRUM_PAD_TRIGGERS_START`,
`DRUM_PADS`; `readings::MAX` = 16) for the editor's pad flashes.
Voice readings (T28, `instruments::voice_readings`; `readings::MAX` = 64): `poly_synth`,
`electric_piano`, `sampler`, `drum_sampler` publish the held-key mask (`HELD_NOTES_START`, 8
integer-valued 16-bit words; note-on to note-off, whatever the source: playback, live MIDI,
previews) and the newest `MAX_VOICES` (4) voices (`VOICE_COUNT`, `VOICES_START` x `VOICE_FIELDS`:
note, zone, playhead seconds of the file, released, ms since note-on / release, amp level);
`HeldNotes::decode` / `decode_voices` read them. A stopped graph that renders nothing marks the
probe inactive (no stale readings). App: `AppState::live_voices(device)` (`state::live_voices`)
/ `lit_keys(r, &live)` (previews + held keys) light the synth keyboards, the sampler map/keys
strip (zones by the voices playing them) and drum pads (`live_glow`); playheads on the selected
zone / pad waveform (`ui::devices::playheads`, faint on sounding pad thumbnails); ADSR voice
dots (poly synth, sampler) and EP note-picture dots (`voice_time`) from the voices.
`drum_machine` readings: one trigger counter per voice (`readings::DRUM_VOICE_TRIGGERS_START`,
`DRUM_VOICES` = 7 in `DRUM_VOICE_NAMES` order kick..perc), +1 per note-on (percussion notes count as perc).
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
	// set_track_clips(track, &clips, &tempo) -> Result<bool> (false: reload), device_xruns()
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
`RLIMIT_RTTIME` soft limit demotes it on `SIGXCPU`), shm is `mlock`ed. The same code is
public as `cadenza_plugin_host::realtime::{current_thread_id, promote_thread}`: the host's
cpal output callback publishes its tid on its first call (`Shared::audio_thread`, no alloc)
and a `cadenza-audio-rt` helper thread promotes it (`AUDIO_RT_PRIORITY` 10,
`CADENZA_AUDIO_RT_PRIORITY`, 0 = off), logging one line; `Player::audio_scheduling()` (shown
in the app's Audio settings status). Surge XT stress test:
`cargo test -p cadenza-plugin-host --release --test sandbox_realtime -- --ignored --nocapture`.
Plugin state is captured explicitly (MCP `capture_plugin_state`, automatically on `save_project`,
and after loading a plugin preset) from a fresh instance, then stored with `SetDeviceState`.
MCP preset tools: `list_presets`/`load_preset`/`save_preset` (user presets in
`$CADENZA_PRESET_DIR` or `$XDG_DATA_HOME/cadenza/presets/<device id>/<name>.json`); routing tools
`set_sidechain`, `set_output_routes`. Plugin factory presets: CLAP preset-discovery + preset-load;
VST3 `IUnitInfo` program lists (phase 19a, `Vst3Plugin::program_lists`: each list with the
`kIsProgramChange` parameter of the unit using it; keys `vst3-program:<param>:<i>:<count>`,
category = list name; loading sets the controller's parameter and sends it to the processor
in a zero-sample process call; lists no parameter selects are skipped). Descriptors carry `sidechain_channels` and `aux_outputs` (catalog
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
MIDI learn (T36, `engine::midi_map`): `Player::set_midi_map(&project)` (also done by `load`)
resolves `Project::midi_map` off the audio thread into a `MidiMapTable` (device param index,
current value) boxed through its own ring (+ garbage ring), kept across graph swaps; the callback
applies mapped CC/bend/pressure from the live MIDI queue (absolute; relative = two's complement
steps of 1/128 of the range; pickup waits until the control passes the value), drops them from
what the instrument and MIDI recording get, and publishes into `Player::midi_map_monitor() ->
Arc<MidiMapMonitor>` (wait-free: `last_controller()` for learning, `value(i)` per mapping).
`Cmd::Param/Volume/Pan/SendLevel` update the table's current value. Tests `tests/midi_map.rs`, `no_alloc`.
Live MIDI: the midir callback pushes into an rtrb queue whose consumer is handed to the audio
thread (and back to be dropped); each callback drains it into a pre-allocated buffer and
`Graph::push_live_event` feeds the monitored instrument at the next quantum (offset 0: midir
and cpal share no clock). Note previews (`Player::preview_note_on/off`, phase 19a) play on
their own track: the callback points the graph's live target at it while preview notes are
held and gives it back to the monitored (armed) track at the next block after the release
(an arm change meanwhile waits); the app flushes a pending debounced reload before a preview
so a just-changed instrument is the one heard. Monitoring sounds while the transport runs; switching the monitored
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
lists before -> after numbers. Match EQ (T31, `tools::match_eq`, shared with the app): `match_eq { track_id?
(master), device_id (a built-in eq insert), reference_file | reference_track_id, amount 0..100 %, max_gain_db (6,
<= 12), max_bands 1..5, reference_start_seconds/reference_seconds, start_beat/end_beat, dry_run }`:
`analyze_match_sources` renders the song once (`analyze_mix_detailed`) on a scratch `Document` with that EQ and every
later insert bypassed = the EQ's input (+ the reference track's detail or the file via `analyze_reference`);
`plan_match` fits (target zeroed where the device's HP/LP cut > 3 dB) -> `fit_changes` (LS, peaks by frequency, HS;
stereo, static; unused bands 0 dB; HP/LP/output untouched) -> ONE Batch "Match EQ to <reference>" (`match_label`,
`match_command`); output `{bands, applied ["Peak 1: 400 Hz -1.5 dB Q 0.8"], difference_db, residual_db,
undo_label?, summary}` (< 1.5k chars, test). The agent's `PathPolicy` resolves `reference_file` like
compare_to_reference. lists before -> after numbers. Floats go out as rounded `f64` / `cadenza_analysis::short`
(no `-9.100000381` noise). Budget test: `outputs_stay_within_their_token_budgets` (24 channels:
analyze_mix < 9k chars, analyze_track < 3k, mix_targets < 5k).
Modulation (T33, `engine::modulation`): a device with modulators gets a `Modulation` (built at
compile, preallocated) run in `Device::run` per quantum: LFOs (value at the quantum start; free
phase reset with the device, note = every note-on, transport = from `Transport.beat_position`;
sync from the transport bpm; random = per-cycle hash, deterministic) and envelope followers
(per-sample peak one-pole on the input, an instrument's previous output, or the sidechain key:
the key is built even for devices without a key input; 0..1 over -60..0 dBFS + gain) set each
target to `clamp(base + sum(depth * value) * (max - min))` via `set_param`; graph writes to a
modulated param (initial, live, automation: `graph::Device::set_param`) only move `base`. No
extra smoothing (32-sample steps, the built-ins smooth params). Modulator edits recompile.
`DeviceProbe::modulation()` publishes each modulator's value for the probed device. Tests:
`tests/modulation.rs` (LFO in an offline render, follower level, offline == realtime), `no_alloc`.
MCP `set_modulators { track_id, device_id, modulators: [{type, ..., targets}], sidechain_track_id? }`
(`tools::modulation`: params checked against the registry, ids kept by position, optional
sidechain in the same Batch) -> `{modulators: [lines]}`.
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
Device probe (redesign D1, `engine::probe`): `Player::set_probe(Option<DeviceId>)` (kept across
loads, `Cmd::Probe`; any device: instrument, insert, master insert) -> `Player::probe() ->
Option<Arc<DeviceProbe>>` (one per player, attached to every loaded graph): `Device::run` copies
the target's in/out per quantum into a preallocated 0.5 s SPSC ring of atomics (full = drop +
`dropped_frames`; `drain(&mut Vec<ProbeFrame { input, output }>)`, `is_active`) and its
`Processor::readings(&mut [f32]) -> usize` (dsp `readings::{MAX, GAIN_REDUCTION_DB}`: compressor,
limiter) as `readings()/reading(i)`. Listen (T27): `Player::set_listen(device, Listen)` / `listen()` -> `Cmd::Listen` -> `Graph::set_listen` (old target back to
Off; re-applied to every swapped-in graph, kept across loads; never offline; `no_alloc` covered).
App: `AudioEngine::probe` -> `AudioEvent::Probe`,
`AppState::set_probe/probe_target/probe`. Covered by `no_alloc`.
Master tap + monitor switches (T26): `Player::master_tap() -> Arc<DeviceProbe>` (one per player,
attached to every loaded graph) gets the master's post-fader output per rendered quantum (both
sides of each frame), before the metronome. `Player::set_monitor(MonitorFx { mono, dim })`
(`Cmd::MonitorFx`, kept across loads in the callback) acts on the device output only, after the
meters/tap/metronome and before the test tone: mono = (l+r)/2, dim = `DIM_DB` (-20); never in
offline renders/exports, not in the project. Covered by `no_alloc` and `tests/probe.rs`.
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
  start; a graph swapped in mid count-in continues it, `Graph::continue_count_in`, so an
  edit cannot start the song early; `PlayerClock::count_in()` -> `CountInStatus { heard_beats,
  beats, per_bar }`, `bar_beat()` = (-2, 1)...: the app's position display shows "-1.3" and
  COUNT-IN, also at bar 1 with no song pre-roll; count-in stays 0 bars unless chosen). After
  each block `Graph::segments()` says which timeline samples the output played
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
  `freeze_track`, `unfreeze_track`; summaries show `frozen` and `freeze_stale`. Stale freezes
  (phase 19a, `model::freeze`): `Freeze.source` = `Project::freeze_source(track)` (FNV-1a of
  clips, instrument, inserts, their device lanes and tempo; file names only, fader/sends left
  out) taken when the render starts; `freeze_is_stale` compares it (no fingerprint = never
  stale). `DocView::freeze_stale` caches it per change; the FRZ badge (header + mixer strip,
  `arrange::freeze_badge`) turns amber "FRZ!" and a click refreezes; track menu "Refreeze".
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
  automatically (`edits::crossfades` mirrors the engine rule): the arrange draws what plays
  (clips by start, later on top; waveforms shaped by `edits::crossfade_gain`, the earlier clip
  fading out under the later one and cut at the crossfade end), the fade-out/fade-in curves and
  a length handle at the crossfade end (drag, snapped unless Alt; double-click resets to the whole overlap; one `SetClipFades`
  on the later clip, `edits::set_crossfade`); nested clips that sum keep the X mark.
- `audio::AudioEngine`: the `Player` on its own thread (like MCP `PlayerHandle`) but never
  blocking the UI: requests are fire-and-forget, a burst of loads is coalesced into the newest,
  results come back as polled `AudioEvent`s; position via `PlayerClock`, levels via `MeterBank`,
  plugin crashes polled every 500 ms and shown as toasts. No device = app works without sound.
- Dropouts (phase 19a, `state::xruns`): the player thread sends `AudioEvent::Xruns { device:
  Player::device_xruns() (cpal `Xrun` errors counted in the stream's error callback), plugins:
  plugin_xruns() }` when they change (500 ms poll); the transport shows "⚠ n" left of the audio
  status only when there are dropouts since the last reset (amber fill for 3 s after a new
  one), tooltip per source, click resets. `VirtualDevice::report_xrun` for tests.
- `jobs`: waveform peaks (decoded + `WaveformPeaks` on a worker, cached by path), plugin
  rescans, exports, file copies and file dialogs (files and folders) run off the UI thread.
- Project files (phase 14; `state::safety`, `state::export`, `state::project_io`,
  `ui::project_dialogs`): File menu New / Open (projects and `.cadenza.zip`/`.cadenzapack`
  archives, extracted into a chosen folder) / Open Recent (persisted, 10) / Start Screen / Save /
  Save As / Import / Export… (Ctrl+E) / Collect Project Files / Locate Missing Files / Export
  Project Archive / Autosave interval. Start screen (no project on the command line;
  `ui::start`, a full page under the menu bar, no workspace behind it): recovery banner
  (instead of the modal), "Describe it to the producer" (new project + chat request),
  "From a style" (new project + the style's first form via `apply_library_form`), Empty
  project, Demo song (bundled `examples/first_light`, written to the state folder), recent
  list with tempo/signature/style, track count, age and a clip thumbnail (`state::start`:
  summaries read on a worker, cached per path, invalidated on save), footer (F1, plugin
  count + Rescan, version). An empty project shows a centered "Add your first track" target
  (+ Instrument/Audio/Bus/Folder) and "Or ask the producer" (input + chips) in the arrange.
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
  Responsive shell (design 12, pure `layout::Responsive::for_window(w, h, chat_pinned)` in
  `view.responsive`, set every frame): below 1600 px the browser is a 40 px icon rail (an icon /
  F6 opens it as an overlay, `PanelLayout.browser_overlay`, closed by Esc, a press on the
  workspace or a drop) and the inspector an overlay from the right; below 1800 px the AI panel is
  an overlay drawer unless pinned (`chat_pinned`, header Pin/Unpin); Esc closes the top overlay;
  floating windows stay above overlays and shrink/move into a smaller window. Docked side panels
  have min/max widths (`BROWSER/INSPECTOR/CHAT_WIDTH`, arrange keeps `ARRANGE_MIN_WIDTH`), right
  panels run full height, mixer 180 px-60 %, editor 200 px-70 % of the height. `TransportLayout`
  folds Click/Count-in/MIDI record/Punch/Automation/Follow into "Modes ▾" (dot = not default)
  below 1600, then drops captions/labels/time; `StatusLayout` shortens counts and chips; the
  mixer goes Narrow by itself when Normal strips do not fit (`MixerView::auto_narrow`).
- Floating windows (T37, FL Studio-like; pure registry `state::windows::Windows` in `view.windows`,
  views `ui::windows::FloatingUi`): built-in device editors (one `DeviceEditorUi` each), track
  properties (`open_track_window`: mixer strip double-click, track menu "Track properties…") and clip
  properties (`open_clip_window`: clip menu, Alt+double-click) float over the workspace, several at
  once (`WindowItem::{Device, Track, Clip}`): opening an open one focuses it, a click focuses (keys +
  probe go to the focused one; a press on a panel blurs), Esc / × close, Keep on top, Dock (devices),
  new ones cascade, clamped to the screen; rects remembered per item and per kind in
  `Prefs.windows` (`SavedWindows`). Track / clip windows draw `inspector::item_window` (the
  inspector's Track / Clip rows).
- Inspector (F8, optional: hidden by default, `ui::inspector` + pure `state::inspector`): header (colour chip, name, what it is) over a
  TRACK | CLIP | DEVICE tab strip (`InspectorView::follow`: the tab follows the selection's `Focus`
  until the user clicks another, which holds until the selection changes; `Targets`: track or master,
  one clip, selected device else the track's instrument; empty tabs dimmed with a hint). Track = name,
  16 colours, kind, instrument, input/monitor, output, volume/pan, M/S/arm, sends, inserts (click =
  Device tab, double-click = editor), delete; Clip = name, start/length typed in bars.beats.ticks
  (`parse_position`/`parse_length`), track, notes/pitch/velocity ranges or file/offset/gain/fades,
  a notes/waveform preview, open piano roll / audio editor, the instrument's compact form inline;
  Device = the compact bodies (or generic sliders + Edit for plugins). History is a fixed footer
  (numbered, AI steps badged, foldable, ~30 % of a short panel). One command per edit.
- Navigation (arrange and piano roll alike, math in `state::timeline`): wheel vertical,
  Shift+wheel horizontal, Ctrl+wheel zoom at the mouse, Alt+wheel track/key height,
  middle-drag pan (Space stays play/stop), themed scrollbars, song overview strip with a
  draggable viewport, Home/End, PageUp/Down (Shift: vertical), Z zoom to selection, Shift+Z fit,
  follow playhead Off/Page/Continuous, edge auto-scroll while dragging. Ruler/markers pinned,
  headers scroll with lanes. Clips: Ctrl+drag copies, Alt disables snap, double-click a MIDI
  clip opens the piano roll.
- Arrange toolbar + tools (redesign T20, pure `state::arrange_tool`): a toolbar row above the
  overview (title, Snap + grid, ↖ Select V | ✏ Draw D | ✂ Split C, Tempo lanes toggle, zoom);
  `view.arrange_tool` (persisted in `Prefs`). Select = the gestures above; Draw: click on an
  empty instrument lane = one-bar clip at the grid cell, drag = clip over the cells touched
  (`draw_span`, Alt free; ghost with position · length), clips still move; Split: hover shows
  the snapped cut line, click splits the clip (or the selection it belongs to,
  `split_targets`/`split_at`, one step). S stays split-at-playhead. Ruler: bar lines + numbers,
  beat ticks only when readable, loop = cyan bar with end brackets on the ruler top (tint +
  edges over the lanes), playhead flag cap; marker chips sized to the name (amber bar + tint).
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
  Look (redesign T21, design 04): toolbar = clip chip (colour, name, "on <track>" menu) + "+",
  tool chips with glyph + key (`arrange::toolbar_chip`, glyph only < 1320 px), Snap toggle +
  step menu, Key chip (root/scale), "Scale rows" (`PianoView::scale_rows`: in-scale rows
  lighter, root lighter still), Ghosts, Events, count, Quantize Q, Fit/−/+; one row per key
  (C labels, hovered/held key tinted), notes in the track colour by velocity with a release
  grip and names (flats when `piano::prefers_flats`), selected pale with a white outline,
  ghosts outlined; ruler and playhead flag shared with the arrange (`arrange::paint_playhead`);
  lane tabs row (`EventLane::tabs`: Velocity, Mod wheel, Pitch bend, Sustain, Expression +
  lanes with data, "+ CC" menu) over the lane, left column = name + scale or the value at the
  playhead (`controller_text`); velocity stems (white when selected), controllers violet.
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
  draws a line (`velocity_line`); Alt+wheel over a note (FL) moves the velocity of the
  selection it belongs to, else of that note (`wheel_targets`, `nudge_velocities`, 2 % per
  notch), previewed with a live tooltip and committed as one `UpdateNotes` when Alt is released
  or the wheel rests 0.6 s (over empty grid Alt+wheel stays key height; Ctrl+wheel stays
  zoom); a resting pointer on a note shows its name and velocity. Controllers (step graph, value tag under the pointer): pencil
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
- Playlist editing (FL, T38; pure `state::playlist`, one undo step per gesture): arrange tools
  Select V / Draw D / Paint B / Split C / Mute T. Ctrl+C/X/V clips (`ClipClipboard`: relative
  tracks kept; paste at the playhead if it moved since the copy, else right after the copied
  span, appending; onto the track under the pointer), Ctrl+B clone after itself (grid / whole
  bars, copies selected), Ctrl+G glue (= consolidate), Q quantize clip starts, Ctrl+click a
  header / track menu selects its clips. Paint: drag repeats the pressed / selected / last
  clip (`view.paint_source`) on the template's own grid, skipping occupied cells; Split drag =
  knife cutting every track it crosses at the line's (snapped) x ("Slice clips"); right-drag in
  Draw/Paint deletes the clips swept over; Mute click/sweep toggles (muted clips dim + hatched,
  clip menu Mute/Unmute). Track height: drag a header's bottom edge (per track,
  `view.track_heights`, `Prefs.track_heights`), double-click / track menu resets, Alt+wheel
  scales all. T39: copies of MIDI clips (Ctrl+V / Ctrl+B / Ctrl+D / Paint / Ctrl+drag) are
  linked by default (`playlist::Copier`: the source joins a new link in the same step; the
  clipboard assigns a link so pastes of a cut clip share one), Shift = independent; linked
  clips draw a chain glyph, a selected copy outlines its siblings, the piano roll chip / clip
  window say "linked ×N"; clip menu / inspector "Make unique" (`playlist::make_unique`),
  "Chop into…" 2/4/8/16 parts / every beat / bar (`chop`, scratch-document splits),
  Shift+drag a MIDI clip's right edge = `stretch` (notes + controller ticks scaled; the copy
  becomes unique), a plain grow of a MIDI clip whose content ends inside it repeats the
  content (`resize_repeating`: period = content rounded up to bars, at most the old length;
  repeats are materialized as notes/points with fresh ids, so the engine and piano roll need
  nothing new). Splitting / left-trimming / stretching a linked copy unlinks it first.
- Sampler editors (`state::samples` pure builders; `ui::instrument_editor` keeps the drop hit areas): the device
  editor's body for `sampler` / `drum_sampler` (inspector "Edit pads/zones", double-click on
  the instrument in a track header or mixer strip). Drum sampler (redesign 17d,
  `ui::devices::drum_sampler` + pure `state::drum_sampler_editor`; `InstrumentEditorUi`
  keeps its `PadHits` for drops): KIT · ALL PADS column (gain, velocity, polyphony, kit
  tune; "Map folder…" / a folder dropped on the kit or a pad (OS or browser Files folder
  rows, `DropTarget::Kit`, `DropAction::MapKit`) = `map_kit` by file / folder names
  (`kit_slot` keywords; kick/snare overflow to Kick 2/Snare 2; loaded pads keep their
  settings) as ONE step "Map kit from folder", unplaced files named in a notice); 4x4 pads
  (pad 1 bottom-left; name, C1 = 36 note, mini waveform in the pad colour, tags C1 choke /
  →2 output / L3 RR3, "Empty · Drop sample", a dragged file shows a blue dashed "Drop to
  replace"; click auditions + selects, higher = harder; drag a pad onto another = swap,
  Alt = copy (`move_pad`, one step); right-click choke group / output / reverse / replace /
  add layer / clear / locate file (browser Files tab); double-click an empty pad = file
  dialog; keys 1–8, Shift+1–8 = pads 9–16 while focused; pads flash from the processor's
  `readings::DRUM_PAD_TRIGGERS_START` counters); selected pad: file menu, layer chips
  (velocity split or round-robin, `set_round_robin` / `add_layer` / `remove_layer`), ⇆
  Reverse, waveform (decoded samples when zoomed in) with start/end flags per layer
  (double-click resets; wheel zooms, Shift scrolls) under the A/D envelope the processor
  plays (draggable nodes), tune / gain / pan / attack / decay knobs, choke / output pickers;
  every gesture one `SetSampleZones` step. <1080 px stacked, <700 px and the inspector the
  compact form (small pads + pad knobs). Sampler (redesign 17e, `ui::devices::sampler` +
  pure `state::sampler_editor`): ZONE MAP (x = key with a piano strip that auditions, y =
  velocity with a labelled axis; `MapWindow` wheel zooms keys, Shift velocity): zones are
  boxes coloured per key range ("C3–B3 / 80–127", root dot); drag the body = keys +
  velocity (root follows), edges resize, the dot ⇔ = root (`drag_zone`, one step each);
  double-click an empty spot = a zone of the last loaded file (`InstrumentView.last_sample`,
  else the selected zone's) filling it (`new_zone` / `fit`), double-click a zone =
  `fit_to_neighbours`; right-click auto-map by file name (`auto_map`), split velocity,
  duplicate, fit, round-robin group, locate, delete; arrows nudge the selected zone while
  focused; preview notes light the map and keys; a file dropped on an empty spot adds a
  zone (`DropTarget::ZoneArea`, `SampleSlot::Area`). Selected zone: name + range, Off /
  Forward / Ping-pong, ⇆ Rev, waveform with start / end / loop flags and the crossfade ramp
  foot (`effective_crossfade`: forward only, as the processor clamps it), "Loop 1.42 – 2.36
  s · xfade 120 ms", root key readout (note names), tune / gain / pan / crossfade knobs.
  GLOBAL · ALL ZONES: amp ADSR, filter Off/LP/HP/BP + `FreqDisplay` with
  `sampler_filter_response_db` (node ⇔ cutoff ⇕ resonance, wheel env amount), cutoff /
  reso / env / filter velocity knobs or the "▸ Filter envelope" ADSR; bottom row trigger
  Gate | One-shot, polyphony / transpose / glide readouts (type or drag ⇕), legato,
  velocity + curve presets, key track. <1360 px map above zone | globals, <560 px stacked
  (both scroll); the inspector shows the compact form (small map + zone knobs). Audio file drops (browser Files or OS) on a pad / zone / the
  keyboard go through the shared `load_sample` tool; Alt+drop below the tracks or the Files
  context menu "Load into new sampler" adds a sampler track auto-mapped to the file.
- Style library (phase 18a; `state::library` pure builders + `ui::library`): browser
  "Library" tab (`AppState::library()` = the session's `Library`; `headless()` uses the
  core pack only; `new()` loads user packs and warns about broken ones via
  `report_library`). Design 08 (T22): while the tab is open the browser is its own wide
  panel ("browser-library", 70 % of the window, max window - 320 px); >= 640 px it is three
  columns (`ui::library::column` clips each): facet chips (genre most used first, 10 + "+ n
  more"; role; section; energy 1-5; Clear), packs + "Credits and licenses" | search box +
  kind tabs with counts (`kind_counts`) + "n results · sorted by fit" + a card grid
  (200 px min, culled off-screen) | the open document beside (>= 980 px, else in place of
  the results, × "Close document"); < 640 px it stacks (filter combos, one card per row,
  documents replace the list, "‹ Library" back). Cards: kind tag, `pack · license`, name,
  `card_subtitle`, a picture (`mini_picture`: the pattern realized over Am F C G, drums on
  rows, cached in egui memory; groove timing dots; progression numerals; form/style section
  strips in section colours), `card_meta`, `card_energy` dots, playing state (green ring,
  ■, playhead from `pattern_preview_progress`); tooltip = summary, facts, step grids.
  Click a pattern = audition it once at the song tempo with the default
  harmony (`default_harmony`: the song style's first loop, else i-VI-III-VII; A minor /
  C major) on a fitting instrument track (`preview_track`) through `preview_note_on/off`
  (`PatternPreview` events sent by `tick`; refused while playing; click again stops);
  double-click writes it at the playhead; drag = `Payload::Pattern` -> `DropAction::
  AddPattern` on an instrument lane/header (from the bar under the pointer, ghost clip
  shown) or below the tracks (new Drums/Bass/Keys/Pad/Lead track with a fitting built-in
  and preset): `library::add_pattern` = one Batch "Add pattern '<name>'" via the MCP
  `build_pattern_clip` (4 bars minimum, `drop_bars`). Styles, forms, progressions and
  grooves open as read-only documents: style = "Start a song in this style" (first form)
  / "Ask producer…" (prefills the chat), Feel (tempo vs the project's), Harmony numeral
  cells, Forms with Apply + section strip (`form_length`), Roles as role-coloured
  draggable/previewable pattern chips (`role_rows`), Sounds, Mix target tiles, Do / Don't
  columns; form = Apply form + strip + section table (bars, energy blocks, roles).
  "Credits" lists every pack (license tag, attribution, links, not-redistributable flag,
  location) and load problems.
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
  Report order: Master, Issues (Fix all on the header), Tone, Channels, Masking; each folds
  (`MixSection`, `mix.folded`; header summary while folded). Until the user folds anything,
  panels shorter than `MixSection::COMPACT_HEIGHT` fold tone/channels/masking. Look (design 06,
  T23; pure helpers in `state::mix`): range/"analyzed n s ago" bar with the Target menu
  (style, `TARGET_PRESETS`, custom) + Analyze (progress bar + Cancel while measuring, the old
  report dimmed); empty state = one big Analyze; Master = big LUFS readout, peak/crest/width/
  lows-mono tiles and a −30..−6 loudness bar with the target tick; stale bar "Mix changed since
  this analysis · Re-analyze"; issue cards (severity glyph, `issue_parts` headline + detail,
  track chip = `open_track_window`, move chips, Fix / ✔ Fixed); Tone = smooth octave spectrum
  over the dashed `tone_target` (gap per band = `tone_deviation`), worst band shaded; Channels =
  LUFS bars with the role's suggested band (`channel_rows`, median anchor ± `BAND_HALF_DB`),
  peak tick, ok / +n; Masking = both colours, `octave_range`, 5-step `masking_level`. Pinned
  footer: reference, brief (`mix.notes`), budget, "AI mix pass" (running: spinner + Stop); a
  finished pass shows its own review page (summary = last reply, Keep / Revert, `review_rows`
  before → after against the targets).
  Library style documents show the mix profile and role levels.
- Mixer (`ui::mixer`, options in `state::mixer::MixerView`, persisted in `Prefs.mixer`):
  toolbar (Normal 124 px | Narrow 76 px strips, Input/Inserts/Sends/Routing rows on/off,
  + Bus), a left gutter naming the rows once plus the fader's dB scale (every strip shares
  one `StripLayout`), strips typed by look (tracks raised, buses cooler after a vertical
  BUSES divider or a dashed "+ Bus" ghost strip, folders as VCA, master accent-tinted and
  wider; the toolbar folds by measured label widths, `mixer_fold`: rows into a "Rows ▾"
  menu, then the layout / meter choices too), input row, inserts with status dot / struck-through bypass, sends as mini-faders
  (PRE tag), output menu with "New bus from this track" (`mixer::new_bus_from`, one undo
  step), pan knob (`widgets::pan_knob`), long fader with a light cap. T26: output menu lists
  sidechain targets (`mixer::sidechain_targets`: devices on other strips/master that take a key
  and stay cycle free, checked when keyed by this track; `toggle_sidechain` = one
  `SetSidechain`); Peak | Peak+RMS toolbar switch (`MixerView.meter: MeterMode`, `meter_bars`);
  the master strip shows LOUDNESS S/I/TP (`mixer::MasterLoudness` = `LiveLoudness` fed from the
  player's master tap via `AudioEvent::Ready.master`, silence while stopped, click resets) in
  its sends/output rows and Mono / Dim −20 toggles (`AppState::set_monitor_fx`, monitoring only,
  kept across device reopens).
- Look: the Graphite design system (`docs/design/redesign-brief`; tokens in `theme`: 7 surfaces,
  text/accent/semantic `*_SOFT` states, meters, 16 `TRACK_COLORS`, clip tint alphas, radii,
  metrics). Fonts: IBM Plex Sans/Mono (OFL-1.1, `crates/app/assets/fonts`, embedded) with one
  egui family per weight (`theme::medium/semibold/mono_medium`, `heading()`), egui's faces as
  fallbacks and Hack last in proportional (arrows, shapes, blocks: `← → ◆ ● ⊘ ▍`);
  `theme::applied(ctx)` re-themes fresh contexts (the first pass is discarded so the weight
  families exist); `proportional_fonts_cover_the_app_symbols` checks every non-ASCII symbol.
- i18n (T40, `crate::i18n`): gettext-style, the English text is the key. `tr("Save")` ->
  `&'static str`, `trf("{n} undo steps", &[("n", &n)])` (named placeholders), `trn("{n} track",
  "{n} tracks", n)` / `trn_with` (plurals, CLDR order per language), `mark("..")` in const
  tables + `tr_str(s)` where they are drawn (also strings from other crates: DSP param names,
  roles, profiles, pass rows listed in `i18n/external.rs`, synced with the registry by a
  test). `locales/<code>.toml` (`"English" = "traducción"` or `["one", "other"]`) is compiled
  by `build.rs` into sorted static tables: a lookup is a binary search, no allocation; a
  missing entry falls back to English. `every_language_file_matches_the_sources` scans the
  sources for every `tr/trf/trn/trn_with/mark` literal: each must be in every file, no unused
  keys, placeholders equal. Add a string: wrap it, add the key + translation to every
  `locales/*.toml`. Add a language: a `Language` variant (code, native name, plural rule,
  decimal comma, table) + `locales/<code>.toml`. Language: View > Language (System default +
  languages, `Prefs.language`, live); the app sets the process language
  (`set_global`, OS locale via LC_ALL/LC_MESSAGES/LANG then `sys-locale`), headless states only
  this thread's (`set_thread`, so parallel tests stay English). Numbers: `i18n::decimal/num`
  (decimal comma in es/pt-BR/fr; `format_value`, `format_db`), typed values accept ',' (`delocalize`
  in `parse_value`). Not translated: project data, library pack text, AI tool/context texts,
  undo labels built in `state` (model/AI vocabulary; some device-editor gesture labels are
  translated), logs, `ParamInfo.description`.
- Languages (T40/T41): English (source), Español, Português (Brasil) (`locales/pt-BR.toml`;
  `Language::code()` stays the BCP 47 primary subtag `"pt"` for OS-locale matching and
  `LanguageChoice` round-tripping even though the file carries a region suffix — `build.rs`
  turns a hyphenated file stem into a valid Rust identifier for the generated static table),
  Français, Deutsch, Italiano, Русский, 日本語 (`ja`), 简体中文 (`locales/zh-CN.toml`; `code()`
  `"zh"`, matches `zh_CN`/`zh_Hans`/`zh_Hant` OS locales — only Simplified is shipped), 한국어
  (`ko`) — ten languages total. French and Portuguese use the CLDR "one at 0 or 1" plural rule
  (`Language::plural_index`), German and Italian the English/Spanish "one at 1 only" rule,
  Russian its three-form "one/few/many" cardinal rule on the last two digits (ends in 1 but not
  11 = one, ends in 2-4 but not 12-14 = few, else many; `Language::plural_forms()` returns 3 for
  Russian), and Japanese/Chinese/Korean a single CLDR cardinal form ("other" for every `n`,
  `plural_forms()` returns 1, `locales/{ja,zh-CN,ko}.toml` plural keys are one-element arrays).
  The seven European languages use a decimal comma; Japanese, Chinese and Korean use a decimal
  point like English. Script coverage: Cyrillic is already in the embedded IBM Plex Sans
  (checked by `proportional_fonts_cover_the_app_symbols`, which also scans every translation
  file for glyphs, splitting out CJK characters — ideographs, kana, hangul, `theme::is_cjk` —
  into their own check), so `ru` needs no font work; CJK cannot be embedded (binary size), so
  `Language::needs_cjk_fallback()` (true for ja/zh-CN/ko) gates `theme::fonts_for_language`/
  `find_cjk_font`, which searches common system paths (Noto Sans CJK, Source Han Sans, Droid
  Sans Fallback, WenQuanYi on Linux; PingFang/Hiragino on macOS; Microsoft YaHei/Malgun
  Gothic/Yu Gothic/MS Gothic on Windows) and appends a found face to **every** font family UI
  text draws with — `Proportional`, `Monospace` and the named `medium`/`semibold`/`mono_medium`
  families `fonts()` builds per weight (egui has no font weights) — not just `Proportional`:
  missing any of them showed as empty boxes in buttons/tabs, panel titles or the position
  counter even with a CJK font installed. No face found = a clear notice (`AppState.notices`),
  not empty boxes (verified: `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc`, the first
  `CJK_FONT_CANDIDATES` entry, is present on the reference dev machine and renders every CJK
  string correctly, menu bar to mixer strips, both when a CJK language is active and when
  another language's View > Language picker lists their native names). `theme::needs_apply`/
  `apply` re-theme a context whenever the active language's CJK need changed (not just on first
  use), so switching into/out of a CJK language mid-session picks up or drops the fallback
  font — the font is per-active-language, so another language's native name in the picker
  (e.g. 日本語 shown while using English) still boxes until that language is actually selected;
  a future pass could load the CJK fallback unconditionally now that the catalog always
  includes one. Arrange/piano-roll toolbars fold their tool chips (key dropped, then the name,
  glyph only) from the width actually left for them (`arrange::chip_width`/`arrange_fold`,
  `piano_roll::piano_fold`; pure text measurement, never by drawing a probe widget twice — that
  briefly duplicated accessibility nodes and broke `egui_kittest` queries), and the arrange
  toolbar moves Sel/Fit into a "⋯" menu when even glyph-only tools don't leave room (found via
  the German 1024 px shot: "Tempo" drawn under "Einpassen", "Sel" cut off). The master strip's
  Mono toggle shows the fixed, untranslated "M" in narrow strips instead of an abbreviated
  translated word (was "Mo", which doesn't track every language's "Mono" translation or fit a
  CJK one).
- MIDI learn (T36, pure `state::midi_learn` + `ui::midi_learn`): every kit control menu and the
  mixer/inspector `automate_item` menus get "MIDI learn" / "Cancel MIDI learn" / "Forget MIDI (CC 74)";
  learning pulses an amber ring on the control (`theme::LEARN`) with a bottom hint (Esc cancels) and
  takes the next controller the monitor reports (`learned_map`: replaces the target's mapping, takes
  the CC from another target; range = the param's / -60..+6 dB / full pan) = one step "MIDI learn CC
  74"; mapped controls show a "CC 74" badge. `tick_midi_learn` mirrors the values controllers set
  (`mirror_value`: previews + autowrite) and commits each after 400 ms rest (`commit_address`).
  File > "MIDI Mappings…" window: min/max, mode, takeover, × (one `SetMidiMap` per edit).
  `live::diff` sends map changes live (`AudioEngine::midi_map`); `AudioEvent::Ready.midi_map`.
- Explain this device (T36): editor header "Explain" (next to Ask AI) and the control menu
  "Explain this device" -> `AppState::explain_device` -> `AiChat::send_explain` ->
  `SessionAgent::run_explain(prompt::explain_request(device))`: read only (`EXPLAIN_TOOLS`; other
  tool calls are refused with an actionable message, the undo group is aborted so nothing stays);
  prompts carry an "# Explain" section.
- AI reply language: the producer answers in the UI's language by default. `AiSettings.
  reply_language: ReplyLanguage` (UI language | English | a typed-in language, `Prefs.ai`,
  settings dialog "Reply language") resolves (`resolve_name`) to `AppContext.reply_language`
  (`None` = English, no hint), rendered as an extra "Reply in: <language>" line in the request
  context (`cadenza_agent::prompt::AppContext::render`); `SESSION_SYSTEM_PROMPT` tells the model
  to use that line and keep tool names/parameters/ids/code in English regardless. Tests use the
  scripted provider (no network).
- Modulation (T33, `ui::devices::modulation` + pure `state::modulation`): a MOD strip under every
  built-in editor body (collapsed header "▸ MOD" + "+ LFO" / "+ Envelope"; open when the device has
  modulators): cards with a draggable handle (egui dnd `ModDrag`), live value bar, shape cards,
  Free/Note/Song retrigger, Sync + step combo or Rate knob, Phase; envelope Input/Key, Attack/
  Release/Gain; target chips (Alt+click removes). `ParamKnob` draws the modulation ring
  (`theme::MOD` teal, outermost; range from `modulation::extent`, live dot from
  `AppState::modulation_values`), a handle dropped on a knob adds a target (depth 0.25), dragging
  the ring sets the selected modulator's depth (200 px = 1), Alt+click on the ring removes it;
  control menu "Modulate with ▸" (toggle per modulator, New LFO / New envelope). One
  `SetModulators` per gesture; depth drags preview only the ring (heard after release).
- Device kit (redesign D0, `ui::device_kit` + pure `state::device`: `parse_value` with units/
  shorthand/note names, `format_value`, `KnobSpec`): display surface `theme::DISPLAY_BG`, 8
  `BAND_COLORS`, automation violet / GR coral / L-R teal-orange, glow strokes; `Knob` S/M/L/XL
  (uni/bipolar, automation + AI rings, floating value, 200 px drag, wheel), `param_knob`/
  `param_toggle` (read the automated value, one `SetParam` per gesture, `control_menu`:
  Automate/Ask AI about this (`device_ai::ask_about`: opens the editor's Ask AI popover
  prefilled "<param> (now <value>): ")/Reset/Copy/Paste; it also draws the 300 ms hover
  readout `floating_tip` = value + `ParamInfo.description` (`ControlInfo.tip`; knobs draw
  their own via `Knob::description`) and remembers the param touched last, `touched`),
  typed `readout` (eases 80 ms on outside changes like the knob, never while dragging /
  automated), toggle, segmented (`segmented_with`: font + disabled segments, used by the
  export dialog), grid selector, mode cards, level/GR/IO meters with peak hold;
  `widgets::checkbox` = egui's checkbox with a visible Graphite box (every dialog uses it);
  displays (pure math in `state::device_display`): `adsr` (cube-root time axis, per-stage
  `Gesture`s, D = decay + sustain, voice dots), `FreqDisplay` builder (log 20 Hz-20 kHz, ±12/±24/
  filter dB ranges, sum glow + band fills, pre/post analyzer from `spectrum_bins` (FFT, 1/6 oct,
  `AnalyzerState` 12 dB/s fall), nodes -> `FreqEvent`s, piano strip + note hint, one mesh per
  fill), `LevelHistory` (min/max columns from probe frames) + `LevelHistoryView` (input fill,
  output line, GR from the top, draggable threshold, `stopped` dims); `gallery` is rendered by
  `render_device_kit`.
- Device editors (redesign D2, `ui::device_editor` + `state::device`): every built-in opens in
  its floating window (`open_device_editor`; T37) from a mixer insert click, the track header's
  instrument name double-click, the inspector "Edit"; `open_device_in_dock` / a window's Dock
  shows it in the bottom editor dock instead (`view.device_editor`, device in
  `view.instrument.device`; the dock's pop-out button floats it again); `toggle_device_editor`
  focuses / closes; `device_editor_target` = focused window, else dock, else topmost window;
  plugins keep their windows. Shared
  header: power (`SetDeviceBypass`), name, track chip, preset ‹ › ▾ over factory + user
  presets (MCP `list_presets` / `load_preset`, one undo step; amber dot = `preset_status`
  dirty), Save (MCP `save_preset`, name prompt), IN/OUT `io_meters` from the probe
  (`IoLevels`), Mix slider when the device has `mix`, close; below 800 px the compact
  two-row header (Save in ⋯, 34 px meters, Mix in the body). The probe follows the open
  editor (`sync_editor_probe`). Bodies are functions dispatched by device id in
  `DeviceEditorUi::body` (`BodyCx`: ref, info, rect, drained probe frames, compact); the
  default `generic_body` draws every param (`param_groups`, `param_control`: knobs M,
  segmented / grid choices, toggles) in packed rows on a display panel.
  Header extras (D2b, `state::device_ab`): A | B compare per device (view state; the active
  slot is the live params, switching = one Batch "Compare B" of `SetDeviceState { params }`,
  undo/redo flips the slot back; A→B copies; A key while the editor has focus, Shift+A on the
  Poly Synth / Electric Piano whose A–K play notes). Editor keys (T29, `DeviceEditorUi::
  shortcuts`, listed in F1): B bypass; ← → select band (EQ) / pad / zone / voice, ↑ ↓ nudge
  its value (band gain or filter freq, pad layer / zone gain, voice level), elsewhere the
  control touched last else `device::main_param` (`device::nudge`: 0.5 dB, Shift 0.1; one
  step on whole numbers; else 1 % of the travel); Sampler Alt+arrows move the zone
  (`device_editor::arrows` consumes them). Undo hint
  "Ctrl+Z <step>" when the last step touched the device (`StepTouches`; both only >= 1600 px);
  pop-out: the dock's device moves to its floating window, the dock goes back to the piano roll.
  EQ (`ui::devices::eq` + pure `state::eq_editor`): `FreqDisplay` with exact `EqBands` curves,
  pre/post analyzer, 7 nodes (drag/curve-drag = one Batch "Peak 2 freq and gain" via
  `AppState::commit_params`; wheel Q/slope, double-click reset/wake, Ctrl gain only), chips,
  band readouts, Range; compact form also replaces the inspector's sliders; keys 1-7 / Del; a soloed band (Listen) dims the other bands' fills.
  Dynamic bands (T30, `eq_editor::{toggle_dynamic, live_offsets, activity}`): band row "Dynamic" toggle (+ Thr/Ratio,
  Att/Rel when room; compact: Thr/Ratio knobs), sum = live curve (band gain readings) over the static ghost, node with a
  dashed ring, 0 dB → gain range bar, live marker and a GR halo by activity (`FreqNode::dynamic`, `FreqDisplay::ghost`).
  Mid/side + match EQ (T31): band row Stereo | Mid | Side segmented (compact: a "Channel" row with Match…;
  `eq_editor::{CHANNELS, channel, set_channel, badge, uses_mid_side}`, one step "<band> channel"); M/S node badges
  (`FreqDisplay::badges`); with an awake mid/side band the display draws the mid sum (`theme::EQ_MID` = text) and the
  side sum (`theme::EQ_SIDE`, pale amber) tagged M / S (`sum2`, `sum_tags`); footer "Side" toggle = analyzer on
  (L-R)/2 (`SampleWindow::push_channel`). "Match…" (band row right / compact Channel row): Audio file… (file dialog
  `FileAction::MatchReference`) or another track -> `state::match_eq` (`AppState::start_match_eq` on a
  `cadenza-match-eq` worker, progress, cancel; `match_eq_proposal` refits live from amount / max gain;
  `apply_match_eq` = one `commit_params` step "Match EQ to <ref>"); while ready the display shows the EQ input (pre fill)
  and the level-matched reference spectrum (`FreqDisplay::reference`, `display_spectra`) and the proposal dashed in the
  accent (`FreqDisplay::proposal`); a card over the display: Amount %, Max dB, "before → after dB", Apply / Cancel.
  Collision view (T32, `state::collisions` + `ui::devices::collisions`): footer "Collisions" toggle (disabled on
  master/bus EQs) shows the Mix assistant's report (fresh one reused, else `mix_analyze` on its worker, progress chip;
  stale chip "Analyze again"): `analysis::issues::track_collisions` (-> `masking::collisions_for`: same rule as `masking`
  but per (other, octave band), `center_hz` = loudest shared third-octave, `cut_db` = `cut_gain_db`, `cut_here`) drawn as
  red columns + a heat strip (`FreqDisplay::heat`, `HeatBand`, relative to the worst band), this track's spectrum as the
  pre fill and the top 3 others as thin lines in track colours (`FreqDisplay::spectra`, legend), the strongest pair
  labelled ("Pad × Keys 177–354 Hz"); strip hover = both levels per track, click (or the label) = `suggest_collision_cut`
  (`cut_changes`: near / free / least-used peak, Q 1.4, half the cut when the rule says the other track should move)
  dashed as the proposal + card Apply (one step "Cut '<own>' under '<other>' at <hz>") / Cancel. Offline only (no live
  probe version).
  Compressor (`ui::devices::compressor` + pure `state::compressor_editor`): level history (key trace from
  the key track's meter, threshold tag drag), transfer curve (dot = threshold, above the knee = ratio, wheel = knee,
  double-click -24/4:1), GR meter, auto makeup (GR at -18 dBFS) chip, key picker (`SetSidechain`); compact in the inspector (with "Listen" to the key, T29).
  Key trace = the compressor's `KEY_LEVEL_DB` reading; footer "Listen to key" (headphones `device_kit::glyph_toggle`).
  KEY FILTER strip under the history (dock, compact): `FreqDisplay` of the key filter, HP/LP x-only nodes
  (`compressor_editor::{KeyFilter, drag_key, key_off}`: dragged to the end = off, double-click = off).
  Listen modes (`AppState::set_listen/listen_mode`, `Request::Listen`) are view-only, follow the probed editor device and
  are cleared when its editor closes. EQ: "Listen" on the selected band (dock + compact) or Alt+click a node
  (`eq_editor::toggle_listen`; the solo follows the selection, `listen_follow`; listened chip ringed in SOLO yellow).
  AI on the device (D2c, `ui::device_ai` + pure `state::ai_changes`): header "Ask AI" (before pop-out) opens a
  prompt popover (Enter sends, per-device suggestion chips) -> `ask_ai_about_device` = `send_ai("<text> (<Device> on
  '<Track>')")` + chat opened; `AppContext.open_device` names the editor's device. Every request snapshots built-in
  params; when it ends `AiChanges { label, request, depth, devices: {param, before, after} }` (view state): `ParamKnob`
  draws the blue ring + delta tag ("+3.0 dB") by itself, toggles/readouts (`ReadoutState::Ai`)/EQ nodes (`FreqNode::ai`)
  turn blue, EQ draws the before response dashed; card "AI changed N params" with Keep, Undo (only while it is the
  last step) and Hold to hear before (`hear_before`: live params only, never a command). Cleared by Keep, undoing the
  step, a new request; a commit / later step on a changed param drops it.
  Limiter (`ui::devices::limiter` + pure `state::limiter_editor`): waveform history (`LevelHistoryView::waveform`, ceiling line on both halves, drag = input gain, wheel = release), IN/GR/OUT meters, LOUDNESS from `state::loudness_meter::LiveLoudness` (UI thread, output frames: short-term per 100 ms, `LoudnessMeter::integrated_since` reset (R), true peak, 30 s trace, view-only target chips); compact in the inspector. Footer / compact row: Oversampling `1x | 2x | 4x` + "5.7 ms latency" (`latency_text`).
  Reverb (`ui::devices::reverb` + pure `state::reverb_editor`): a custom time/dB tail picture (early reflections, lows/highs decay curves, highs fall faster with damping) with 3 hand-drawn draggable nodes (pre-delay/decay ⇔, damping ⇕; wheel over the tail = decay, double-click resets), a Space box (corner drag = size, right edge = width, wheel = size), a `FreqDisplay` low-cut strip and a wet-output `level_meter`; compact in the inspector. "❄ Freeze" (`param_glyph_toggle`, the stored `freeze` param) in the footer / compact row; frozen = tinted tail + "Frozen · tail held" badge. Duck / Duck rel. knobs (coral), a duck GR bar beside the wet meter (probe reading) + the shared key picker (`compressor::key_picker_with`) in the footer.
  Delay (`ui::devices::delay` + pure `state::delay_editor`): echo taps on a real time axis (`n * echo_ms`, `response::delay_time_seconds` shared with the processor) with a true beat/ms grid, alternating L/R when ping-pong is on, darker per repeat from the feedback filter; the first echo's drag snaps to `Sync` (`TimeChange::Sync`/`Free`), `Sync | Free` + the 12-step note `grid_selector`, `⇄ Ping-pong`, a `FreqDisplay` feedback-filter strip (two x-only nodes); S/P keys toggle sync/ping-pong while focused; compact in the inspector. Taps 2-4 (`DelValues::extra_taps`): bars at their times with faint feedback repeats and a handle (drag ⇔ = shared spread, ⇕ = level, wheel = pan, double-click resets; one step "Delay tap"), `Taps 1|2|3|4` row, Tap spread / Taps knobs.
  Chorus / Saturator / Gain (`ui::devices::{chorus,saturator,gain}` + pure `state::{chorus,saturator,gain}_editor`; shared `ui::devices::{commit_changes, KnobDef, knob_grid, compact_scroll, footer}`, kit `Knob::wheel_step` for 0.5 dB wheels): chorus = both voices' LFO delay scrolling at the real rate (UI phase, repaints only while visible and playing/audio passes, else frozen + 50 % dim; drag ⇕ depth ⇔ rate = one step) + width fan; saturator = mode cards (keys 1/2/3), `saturator_shape` curve scaled to fit (drag ⇕ drive in 0.5 dB, double-click 0, 150 ms morph), test-sine or probe before/after scope, harmonics by DFT of the curve; gain = goniometer of the probe output (⇔ pan ⇕ width, M mono), correlation meter, XL gain, Ø toggles, 900 px centred; compact forms in the inspector.
  Poly Synth (`ui::devices::poly_synth` + pure `state::poly_synth_editor`): OSC (5 wave cards incl. Wavetable: per wavetable osc a panel with table picker, waterfall of frames (current violet, modulation extent/live frame teal; drag ⇕ / wheel = position, Shift fine, double-click resets, a MOD handle dropped on it targets position) + Position knob; one-row docks put the panels in a column right of the cards, wrapped/stacked/compact under them; right-click copy to the other osc; semi/cents readouts dragged ⇕; unison 1-4) → FILTER (`FreqDisplay` with `poly_synth_filter_response_db`, node ⇔ cutoff ⇕ resonance = one Batch, wheel env amount, dashed env peak) → AMP/FILTER ENVELOPE (kit `adsr`, D = one Batch) → OUTPUT (300-point `scope_window` of the probe, flat when idle, held notes); on-screen keyboard via `preview_note_on/off`, A-K/Z-X only while the editor has focus (`SynthKeys`, released on key up/focus loss/close); wraps into two rows below 1000 px, stacks below 560; compact in the inspector.
  Electric Piano (`ui::devices::electric_piano` + pure `state::electric_piano_editor`): tone bars of a held C4 (`partials`: brightness tilts, bell lifts 7-9 pale) over a Mellow ↔ Bright slider bound to brightness, face card (L/XL Brightness/Bell/Decay, S Velocity/Release/Gain), tremolo picture (one note over 3 s from the DSP's envelope, L up / R down × the auto-pan, LFO lines); the keyboard (`ui::devices::keyboard`: `SynthKeys`, `keyboard_row`) is shared with the Poly Synth; compact in the inspector.
  Drum Machine (`ui::devices::drum_machine` + pure `state::drum_machine_editor`): 7 voice strips in `BAND_COLORS` (pad click = audition via `preview_note_on/off` + select, Ctrl/Cmd-click selects only, double-click resets the voice; envelope from the voice's params, 2 summaries, level bar drag ⇔ / wheel = one step), selected voice (kick waveform with its real pitch sweep, dashed amp envelope, L knobs), KIT tone/gain, ← → voices; strips flash 150 ms on the DSP trigger counters (`Flashes` over `probe().readings()`); <1100 px two rows, <700 px compact; compact in the inspector.
- Tests: state unit tests, `tests/ui.rs` drives the whole window headless with `egui_kittest`;
  `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --test ui -- --ignored` renders PNGs (wgpu),
  including automation lanes, the audio editor, an empty project, (`render_sampler_screens`)
  the sampler editors, (`render_sampler_editor_screens`) the Sampler (12-zone felt piano: dock, filter
  envelope, 1280, 540 stacked, compact), (`render_device_editor_screens`) the device editor (reverb, compact
  compressor, poly synth, drum sampler), (`render_project_screens`) the start screen, export dialog/progress/
  result, Locate missing files and the recovery prompt, and (`render_recording_screens`) armed
  tracks with take lanes, a recording in progress (fed by the engine's virtual device) and the
  audio settings dialog, and (`render_phase17_screens`) time signature changes + the signature
  lane, controller lanes (mod wheel, pitch bend), an automatic crossfade and a folder (open,
  folded), (`render_mix_screens`) the Mix tab (empty, analyzing, report, issues, pass running, review) and the Files tab glyphs, (`render_mid_side_match_screens`) an EQ side band + match preview, and (`render_library_screens`) the Library tab (list, filters, preview, pattern
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
  live in the project (`Project.armed`, saved; phase 19a): the ● button is one undo step "Arm
  'Vox'" (`SetArmedTracks`, plus In 1 for an audio track without input), `prune_record` syncs
  the player mirror `RecordState.armed` after every edit/undo/open (`live::diff` ignores
  arms; kept across device reopen); audio input monitoring already lives in `TrackInput.monitor`;
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
