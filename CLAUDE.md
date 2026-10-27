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
| `cadenza-notes` | note detection on audio (roadmap `docs/design/note-editing-plan.md`, NE-n): frame analysis (`analysis`: pYIN pitch track + level/flux/sibilance features, on-disk `Cache`), FFT YIN, note segmentation (`segment`, `vibrato`), notes -> MIDI (`midi`), resynthesis of note edits (`edit`, `marks`, `psola`, `timemap`, `resynth`: PSOLA + closed loop), `NoteDetector` + naive `baseline`, evaluation harness `eval` (synthetic corpus with exact truth, labeled recordings, note and frame metrics), stored clip notes <-> detection (`clip`) | core, model, theory |
| `cadenza-midi-io` | SMF import (-> undoable `Batch`; MPE files: per-note bends, `mpe_import`) / export (+ MPE: `export_mpe`), hardware MIDI input (midir -> rtrb), recorder | core, model, theory, dsp |
| `cadenza-engine` | compiles a `Project` into a render graph; offline render/export (mixdown + stems jobs with progress/cancel), mix analysis (`analyze_mix`, `analyze_mix_detailed`), realtime playback (cpal) + live MIDI monitoring, audio input + input monitoring, recording (WAV takes, MIDI), metronome + count-in, rendered copies of audio clips with note edits (`note_render`) | core, model, dsp, audio-io, midi-io, analysis, notes |
| `cadenza-mcp` | MCP server (rmcp, stdio) exposing the command API as AI tools | model, theory, engine, dsp, audio-io, midi-io |
| `cadenza-plugin-host` | CLAP (clack) + VST3 hosting, out-of-process scanner, catalog, per-plugin sandbox process (Unix: shm + futex / macOS `os_sync`), plugin editor windows (X11) | dsp |
| `cadenza-agent` | embedded AI producer: provider-agnostic `LlmProvider` tool loop (OpenRouter + generic OpenAI-compatible), budget cap, JSONL transcript; `Agent` (compose from a brief) and `SessionAgent` (in-app chat on a shared session) | model, mcp |
| `cadenza-cli` | `cadenza` binary: `render`, `export` (mixdown/stems), `collect`, `relink`, `pack`, `unpack`, `analyze`, `play` (`--midi-in --monitor`), `monitor`, `midi-ports`, `import-midi`, `export-midi`, `mcp`, `scan-plugins`, `list-plugins`, `compose`, `chat` (one AI request on a project), `mix` (AI sound-engineer pass: `--target-lufs`, `--reference`, `--notes`, `--dry-run`, `-o`; analysis before/after + changed numbers), `library list|validate|import` (style library packs; MIDI -> pattern), `notes analyze|segment|to-midi|tune|eval|eval-pitch|corpus` (pitch/feature analysis of a file, cached; notes of a file with forced splits; a recording to a .mid (`--hum`, `--bends`, `--key`, `--quantize`, `--algorithm auto|mono|poly|percussive|pitched`, `--mpe`, `--drum-note`); pitch correction rendered to a WAV with before -> after cents per note (`--key` (or `auto`: fitted scale + offset, reason per note), `--center`, `--vibrato`, `--transpose`, `--formant`, `--sibilants`); score a note detector or a pitch tracker; write the synthetic corpus), `tune` (AI tuning engineer on an audio clip: `--clip|--track`, `--key`, `--reference` MIDI clip, `--notes`, `--dry-run`, `-o`), `gui` (starts `cadenza-app`) | all |
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
ubuntu-22.04), Windows zip (no sandbox binary) + `-setup.exe` (Inno Setup, `packaging/windows/cadenza.iss`,
`iscc` on the runner, static CRT via `RUSTFLAGS`; fixed `AppId` = in-place upgrades; per-user by
default in `%LOCALAPPDATA%\Programs\Cadenza` like `install.ps1`, or all users; Start menu/desktop
shortcuts, `.cdz`/`.cdzpack` associations, optional PATH entry; wizard in the app's 10 languages,
Simplified Chinese `.isl` downloaded by the job), macOS universal `Cadenza.app` (lipo, ad-hoc
signed) in a dmg; each ships `cadenza`, `cadenza-app`, `cadenza-plugin-scanner` (+
`cadenza-plugin-sandbox` on Unix) side by side, plus LICENSE/README and the bundled drum samples
(`assets/samples` -> `samples/`, AppImage `usr/share/cadenza/samples`, mac `Contents/Resources/samples`). Tags publish a GitHub Release
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

The detailed contract of each crate lives in `docs/contracts/<crate>.md`. **Read the file of
every crate you are about to change (and of the crates whose API you call) before editing**,
and update it in the same change when the contract moves (new types, commands, tools, params,
UI behavior). Keep this file to cross-crate rules and the core model; per-feature detail goes
to the contract file.

| Contract | Covers |
|---|---|
| `docs/contracts/cadenza-model.md` | sample zones, clip fades, time signatures/`Meter`, controller lanes, folders, linked clips, takes, project files/portability (the core types are below) |
| `docs/contracts/cadenza-library.md` | style packs, validation budgets, `realize`, search, MCP library tools, MIDI -> pattern import |
| `docs/contracts/cadenza-analysis.md` | spectrum/detail, roles, masking, targets, issues, `MixMove`, reference compare, match EQ fit |
| `docs/contracts/cadenza-notes.md` | NE-0..NE-9l: pYIN analysis + cache, segmentation, notes -> MIDI, PSOLA resynthesis, audio note edits, poly/drums/hits, MPE, tuning |
| `docs/contracts/cadenza-dsp.md` | `Processor` trait, presets/kits, sample instruments, wavetables, expression, readings, built-in device params |
| `docs/contracts/cadenza-engine.md` | render/export API, routing + PDC, plugin hosting/sandbox/editors, freeze, MCP tool families, metering/probe, live param path, loop/audition, recording |
| `docs/contracts/cadenza-app.md` | `AppState`/`live::diff`, panels + layout, every view and editor, device editors, i18n, AI chat, tests/screenshots |

### cadenza-model (core)

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
	midi_input: Option<TrackId> /* effects only: an instrument track's MIDI notes (NE-9e), optional */,
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
	fade_in: Option<ClipFade>, fade_out: Option<ClipFade>, notes: Option<Box<NoteEdits>> /* NE-5, model::audio_notes */ } }
pub struct ClipFade { seconds: f64, curve: FadeCurve /* linear (default) | equal_power | exponential | s_curve */ }
pub struct Note { id: NoteId, pitch: u8, velocity: f32 /* 0..1 */, start: Tick /* relative to clip */, length: Tick,
	bend: Vec<NoteBendPoint { tick /* from the note start */, semitones }> /* per-note bend (NE-9h), optional */,
	pressure, timbre: Vec<NoteValuePoint { tick, value /* 0..1 */ }> /* per-note expression (NE-9k), rest 0 / 0.5, optional */ }
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
SetSidechain { track, device, source: Option<SidechainSource> }, SetMidiInput { track, device, source: Option<TrackId> } (effects only, instrument-track source; removing the source track disconnects it), SetOutputRoutes { track, routes }
(instrument only), SetClipFades { clip, fade_in, fade_out } (replaces both; audio clips only),
UpdateAudioClip { clip, file?, offset_seconds?, gain_db? } (audio clips; swaps in processed copies),
SetAudioNotes { clip, notes: Option<Box<NoteEdits>> } (replace the detected notes + edits of an audio clip),
UpdateAudioNotes { clip, patches: [AudioNotePatch { id, pitch?? , drift?, modulation?, formant_cents?, gain_db?, time?? }], sibilant_gain_db? },
Batch { label, commands }.
`ParamAddress` also names send levels: `track/{id}/send/{send_id}` (dB; SetParam, automation);
removing a send (or the bus it feeds) drops its lanes. `model::automation_value(points, tick,
tempo)` evaluates a lane in seconds, exactly like the engine (which interpolates in samples).

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
