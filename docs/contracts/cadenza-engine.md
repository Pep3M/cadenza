# cadenza-engine — contract

Moved from CLAUDE.md (kept in sync with the code: update it when the contract changes).

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
MCP preset tools: `list_presets { category, query }` (the tool, `list_presets_tool`, drops descriptions
from long unfiltered lists)/`load_preset` (Batch with `SetModulators` for built-ins; reports `modulators`)/
`save_preset` (user presets keep modulators; `DeviceSpec.preset` starts a built-in on a factory preset in
create_track/insert_device, `convert::find_factory_preset` suggests close names; `get_device_info`
groups names per category) (user presets in
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
versioned) + the bundled kit samples (`bundled_sample_dir`: `$CADENZA_BUNDLED_SAMPLES`, next to
the exe `samples/`, `../share/cadenza/samples`, `../Resources/samples`, else repo `assets/samples`;
must hold `CREDITS.md`; copied once per process, `.part` + rename), `list_factory_samples`, `preset_zones`, `default_content(id)` (a new sampler starts
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
MCP sample tools (`tools::samples`): `list_factory_samples { query, pad, limit (40, max 400) }` (+ `total`), `load_sample` (drum pad by name/
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
  If the audio thread cannot be reached (its command queue is full) or never hands the tap back
  within `timeout`, the recording is abandoned (`self.recording` cleared, the writer's
  `JoinHandle` detached rather than joined when its tap never came back) so the player stays
  usable: a later `start_recording` is not refused as "already recording".
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
