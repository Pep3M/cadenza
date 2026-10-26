# cadenza-notes — contract

Moved from CLAUDE.md (kept in sync with the code: update it when the contract changes).
Scope: (NE-0..NE-9l, plan closed; plan `docs/design/note-editing-plan.md`, research `note-editing-research.md`)

Pure, offline. Times in seconds of the analyzed audio, pitches fractional MIDI. `Note { onset,
offset, center }`; `NoteDetector { name, detect(samples, rate) }`, registry `detector(id)` /
`DETECTORS`. `yin::track` = FFT-autocorrelation YIN per 5 ms frame (pitch, aperiodicity, rms).
`eval`: `TruthNote { onset, offset, nominal, center (sung, over the core without vibrato),
core_start, core_end }` (a legato boundary = middle of the glide; cores exclude glides,
scoops, falls, 20 ms attack, 30 ms release); `synth::corpus(rate)` (20 deterministic cases:
steady, legato glides, scoops/falls, vibrato, drift, repeated + consonants, grace notes,
melisma, octave leaps, breaths, expressive, noise SNR 20/10, reverb); `labels` (`<name>.json`
next to the audio: onset/offset/pitch name or MIDI, optional cores); `metrics` (mir_eval
COn/COnP/COnPOff with maximum matching, center error in cents of the region covering half the
core, mixed regions = a region over two cores of different pitch, splits, merges, octave
errors, voicing overlap); `evaluate(detector, cases) -> Report`. `baseline` (YIN + new note on
a > 50 cent frame jump, mean center) is the bar: exact on steady notes; on the corpus 41.8 % mixed regions, COnPOff
68.9 %, 211 c p95 center error (66 c median on long glides). The crate and realfft/rustfft build with opt-level 3 in dev (root `Cargo.toml`).
`CADENZA_NOTES_CORPUS` = folder of labeled recordings for `cadenza notes eval`.
NE-1 frame analysis: `PitchTracker { name, track -> Vec<PitchFrame { time, pitch, voicing }> }`
(`tracker(id)`: `pyin`, `yin`). `yin::Cmnd` = the shared difference function (FFT
autocorrelation, 40 ms window, 5 ms hop). `pyin`: Beta(2, 8) threshold prior (mean 0.2, tuned on
the corpus; the paper's 0.1 lost voicing in noise) -> per-frame period candidates with
probabilities; Viterbi over candidates + unvoiced (move cost 1 nat/semitone capped at 8,
switch 4.6, unvoiced bias 1 in frames with candidates, silence 50 dB below the loudest with a
10 dB knee). `features::spectral_frames` (23 ms Hann: level dBFS, log-magnitude flux, energy
share above 4 kHz) on the same grid. `analysis::analyze(samples, rate, opts, progress) ->
Option<Analysis { version, sample_rate, hop, duration, frames: [AnalysisFrame { time, pitch,
voicing, aperiodicity, level_db, flux, hf_ratio, candidates (top 3, decoded first) }] }>` (None
= cancelled); `par::map_frames` splits long takes into one chunk per core (<= 8, from 4096
frames; chunked == single-threaded, tested). `Cache` (`<dir>/<sha256 of samples + rate +
version + options>.cdzna`, little-endian f32 binary, atomic writes, stale versions ignored;
the CLI uses `paths::cache_dir()/analysis`). Frame metrics (`eval::frames`, truth on a 5 ms
grid from the synth curve or from labels): raw pitch/chroma, voicing recall/false alarm,
octave/gross rates; `evaluate_pitch`. Floors in `tests/pitch.rs` (pYIN: raw pitch >= 98 %, 0
octave errors, gross <= 0.2 %, false alarm <= 8 %). A 3-minute 44.1 kHz take analyzes in ~0.3 s.
SwiftF0 is not wired: tract 0.23.9 cannot type its graph (see the plan, NE-1). pYIN passes over
a dip with aperiodicity > 0.1 for a later one 4x deeper (`deeper_ratio`/`deeper_above`: the
octave-up dip of a strong second harmonic); the YIN window is ~46 ms at every rate.
NE-2 `segment::segment(&Analysis, &SegmentOptions, splits /* forced boundaries, s */) ->
Segmentation { notes: [SegmentedNote { onset, offset, core_start, core_end, center, drift (c/s),
vibrato: Option<NoteVibrato { rate_hz, depth_cents }>, confidence, flags: [NoteFlag::
MaybeTwoNotes { at, cents } | ShortCore | WeakVoicing] (serde tag "flag") }], sibilants: [{start,
end}] }`; `Segmenter` = detector `segment`. Per voiced run (short gaps bridged without an attack,
forced splits cut, reverb tails cut: 12 dB fall within 100 ms not back within 8 dB for 60 ms):
`vibrato::detect` (sinusoid 4-8.5 Hz on a detrended ±0.25 s window; rejected for staircases,
too few zero crossings, one-way motion; regions >= 0.35 s) + `remove` (mean over one period) ->
`partition` (exact DP: cores = lines |slope| <= 150 c/s priced by squared error + `note_cost`
10k / `sensitivity`; transition frames 300 when moving, 4000 when still) -> `notes_of_run`
(merge cores < 35 c apart across <= 60 ms glitches; absorb short edge cores: fall bottoms,
quiet scoop starts (median level), unseen vibrato swings; legato boundary = glide crossing
halfway between centers; centers from cores only; `self_check` -> confidence / MaybeTwoNotes).
Synthetic corpus (22.05 kHz): COnPOff 99.6 %, 0 mixed/split/merge/octave, center median 0.4 c,
p95 3 c; 16/44.1/48 kHz >= 97.2 %. Floors: `tests/segment.rs`; pYIN floors in `tests/pitch.rs`
are raw pitch >= 97.5 % (glide-edge frames since the corpus melodies stopped repeating notes).
A core < 80 ms next to a note an octave / twelfth / two octaves away is folded into it
(`harmonic_glitch`: the tracker locking onto a harmonic in attacks or beating unisons).
NE-3 `midi::to_midi(&Segmentation, &Analysis, &TempoMap, origin /* song s at analysis 0 */,
&MidiOptions { quantize (beats), quantize_strength, key: KeySnap::Off|Auto|Scale, bends,
bend_range, min_length, merge_repeats, octave_shift, range, velocity, region }) ->
MidiConversion { notes: [MidiNote { start, length (song beats), pitch, velocity, cents }],
bends: [BendPoint { beat, value -1..1 }], key, dropped }`; `MidiOptions::hum()` (1/16, key
inferred with `Scale::infer` (cadenza-theory, Krumhansl; the library importer uses it too),
repeats merged, < 80 ms dropped). MCP `audio_to_midi` (`tools::notes`: `analyze_clip(project,
clip, sensitivity, cache_dir)` -> `ClipNotes`, `midi_options(args)`, `convert`,
`conversion_command` -> ONE Batch "Convert '<clip>' to MIDI" (clip "<clip> (MIDI)" + pitch-bend
lane on `track_id` or a new instrument track below: bass/keys/lead by median pitch); the app's
dialog and hum recording reuse them. Round trip MIDI -> render -> audio_to_midi:
`mcp/tests/audio_to_midi.rs` (COnP F >= 0.95, hum mode exact).
NE-4 resynthesis (offline, pure Rust): `edit::{NoteEdit { pitch, drift, modulation (shares kept,
-1..2), formant_cents, gain_db, time: Option<TimeEdit> }, Edits { notes (by segmentation index),
sibilant_gain_db }, validate, curves, time_map, correct_pitch(&Correction { key, center, drift,
threshold_cents })}`; core shift = `(target - center) + (a_drift - 1) drift + (a_mod - 1) mod`
(drift = 0.2 s double box), transitions blend between connected cores, `marks::pitched` frames
(voiced + up to 3 edge frames still holding the period) take the next core's value.
`marks::pitch_marks`: cycle time from the fundamental's phase (2-period Hann DFT, absolute, no
drift), mark on the pulse (derivative-energy component) by its offset, weak pulses take the ±5
cycle median offset; 5 ms marks elsewhere, first 0, last len-1. `psola::psola` (sin²/cos² grains
between neighbour marks = exact null render; formant = grain read speed; Catmull-Rom placement;
time map picks the source mark; gain last). `resynth::{render (unchanged edits return the input;
reshaped notes leveled back to their core RMS), verify (re-analyze + re-segment, center at the
mapped core), render_verified (<= 2 extra passes, integrating the error vs the original target,
tolerance 2 c)}`. `tests/resynth.rs`: corpus moves 0..±3 st all within ±5 c (22.05 + 44.1 kHz),
±12 st p95 <= 3 c on clean cases, null < -80 dB, no clicks, level ±0.5 dB, LPC envelope kept /
formant ×1.18 for +300 c, time edits, stereo, sibilant gain, vibrato flattening, noise.
NE-5: `cadenza-model::audio_notes` (`NoteEdits { notes: [AudioNote { id, start, end, core_start,
core_end (file seconds), center, pitch?, drift, modulation, formant_cents, gain_db, time?,
confidence, flags }], sibilants, sibilant_gain_db, sensitivity, user_splits, user_merges }`,
`validate`, `summary` -> `ClipSummary.audio_notes { count, edited, avg_error_cents, flagged }`);
`clip::{detect (re-detection keeps ids + edits of matching notes, splits, merges), split,
merge_with_next, to_segmentation, to_edits}`. Renders are derived data:
`engine::note_render::{render_path (sha256 of version + file path/size/mtime + edits JSON, in
$CADENZA_NOTE_RENDERS or <cache>/note-renders), ready, render (closed loop), pending}`; compile
plays the render, offline mode renders a missing one, realtime plays as recorded with
`PENDING_WARNING`; the app renders missing ones on a worker (`state::note_renders`) and
reloads; audio editor reverse/stretch/transpose drop the notes.
NE-9b local re-renders: `psola::{plan (grain layout, cheap), psola_range (any window, bit-identical
to the whole render), changed_span}`; identity renders resync on the source grid at the third mark
of each unpitched run. `resynth::{verified_with -> Verified { render, residuals, effective },
rerender_verified(.., Previous { channels, asked, effective }, edits) -> Option}` (re-render +
closed loop only around the changed notes, spliced; `None` = whole render: time edits, other
sibilant gain, > 60 % of the take). `note_render` v2 keeps `<render>.json` (asked + effective
edits) and `<file key>.renders` (last 8): renders start from the closest earlier one
(`NoteRender.local`; one note on a 20 s take 1.7 s -> 30-70 ms), and the player plays it while
the new one is pending.
NE-9d hearing while dragging: note editor Main drag -> `AppState::audition_notes` (stopped transport,
`NoteEditorView.audition`, toolbar "♪ Hear") -> `state::note_audition` worker (take + analysis + marks
per file, newest request wins) -> `resynth::audition` (open-loop window render) -> engine
`Player::play_snippet` (`snippet::Snippet`, handover + garbage rings, 5 ms fades, monitoring only).
NE-9e MIDI-guided Pitch Correct: `Device.midi_input` -> engine `graph::DeviceMidi` (the source track's
clips scheduled for the insert, `QuantumMidi::fill`; live edits of a feeding track recompile) ->
`pitch_correct` `midi` param (0 ignore / 1 held note else scale / 2 only while held, `nearest_held` in the
singer's octave); MCP `set_midi_input`; editor "MIDI: <track>" menu; `cadenza_dsp::effect_takes_midi`.
NE-9f pitched percussion: `hits::detect -> HitTrack { analysis, segmentation, unpitched }` (808s, toms: slow-level
rises or steps between steady pitches start hits, YIN 28-500 Hz on a ~4 kHz copy after a 25 ms settle, pitch =
median of the first 150 ms, curve kept for bends) -> `to_midi`; MCP `audio_to_midi { algorithm: pitched_percussive }`
(`ClipNotes.hits`, `detect_hits`, new track "808 Clean"), app "Pitched hits", CLI `--algorithm pitched`.
NE-9g MPE export: per-note curves `PolyNote.bend` (`poly::note_bend` from the contour; key k peaks at bin 3(k-21)+1;
cache v2 stores the contour as bytes) and `MidiNote.curve` (beats, semitones; `to_midi` and `poly_to_midi` fill it)
-> `cadenza_midi_io::export_mpe` (MCM lower zone, 15 members, bend range 48, LRU channel per note, `shared` count);
MCP `audio_to_midi { mpe_path }` (`write_mpe`), CLI `notes to-midi --mpe`, app Convert to MIDI "Export MPE…".
NE-9h per-note bends: `Note.bend: Vec<NoteBendPoint { tick (from the note start), semitones }>` (held steps, optional
in JSON, `NotePatch.bend`, points past the end kept unplayed; `bend_at/split_bend/scaled_bend`) -> `MidiEvent::NoteBend`
(per-key bend in `Expression`: poly_synth/electric_piano/sampler; sandbox `EV_NOTE_BEND`; CLAP tuning expression) from
`MidiSchedule` (chased); piano roll draws it + "Straighten", Bend tool (G) draws it freehand (NE-9i, `piano::draw_bend`); MCP `bend` on notes, `set_note_bend`, `export_midi { mpe }`.
NE-9j MPE import: `midi_io::mpe_import` (MCM zones + RPN 0 ranges -> `ParsedMidiFile.mpe`; member-channel bends become `ParsedNote.bend`,
notes move to the zone's master channel = one instrument, member events/config CCs leave the lanes) for every MIDI import.
NE-9k per-note pressure/timbre: `Note.pressure/timbre` -> `MidiEvent::NoteExpression { kind: NoteExpressionKind }` (`Expression::
filter_semitones`: poly_synth/sampler filter; sandbox EV_NOTE_PRESSURE/TIMBRE; CLAP pressure/brightness expressions, MIDI 1.0 poly
pressure) from `MidiSchedule` (`NoteSpan.curves`, chased); MPE export/import as member channel pressure + CC74; MCP `pressure`/`timbre`
on notes, `set_note_expression`; note popover Clear.
NE-9l: piano roll event lanes `EventLane::Note(NoteExpression)` draw each note's pressure/timbre over its span; drag = the note there
(`piano::expression_target`: selected, else the highest) via `draw_expression`, right-click clears, one step. The NE plan is closed
(see "Closing" in the plan).
NE-7a (from an expert's session, `docs/design/note-editing-expert-session.md`): `refine`
(`SegmentOptions::refine`, default on): holds on the tracked curve (< 1000 c/s; the
vibrato-free curve only where vibrato is, it straightens runs), never across a boundary,
joined under max(60 c, 2.5 x depth); new forced boundaries between holds of different pitch
and at re-articulations (>= 100 c in 20 ms + 5 dB dip), notes >= 80 ms, edge holds >= 60 ms,
then one more partition; landing notes after a fall kept (`landing_hold`); blips (< 120 ms,
> 24 st from the median, pure tone: metronome bleed) dropped; region `MaybeTwoNotes` flags.
`tuning::{fit_scale (modes + harmonic minor x offset -50..50 c, >= 6 notes), targets (reason
in_scale|chromatic|joins_next|scoop_landing, runner_up, ambiguous), auto_targets}`;
`Correction.targets`. Expert labels `tests/data/expert_take.json` (`tests/tuning.rs` 51/52;
`tests/expert.rs` with `CADENZA_NOTES_EXPERT_TAKE`, COnPOff >= 83 %).
NE-7b (AI tuning engineer): MCP `tools::tuning` (`detect_audio_notes`, `analyze_vocal`,
`get_audio_notes`, `tune_audio_notes` (+ `overrides`), `edit_audio_notes`, `split_audio_note`,
`merge_audio_notes`; notes by number in time order; key auto | nearest | scale,
`offset_cents`, `reference_clip_id` = MIDI melody -> `TargetReason::Reference`); shared pure
builders `plan_targets`, `correction_patches` (the app's Correct Pitch), `pitch_patch`,
`analyze_file`. Agent "# Tuning engineer" section, `SessionAgent::run_tuning_pass(&TuningBrief)`
= one undo step `TUNING_UNDO_LABEL` ("AI tuning pass"); app "Tune with AI" (note editor macros,
audio clip menu, `AppState::tune_with_ai`); CLI `cadenza tune`.
NE-8: `poly` = Basic Pitch in plain Rust (weights `models/basic_pitch.bin` from the ONNX via
`tools/basic-pitch/export.py`, Apache-2.0, `include_bytes!`; == onnxruntime within 2e-4;
`transcribe(samples, rate, &PolyOptions, progress) -> (Activations, Vec<PolyNote { onset,
offset, pitch, amplitude }>)`, published note creation (overtone filter optional, off),
`polyphony` (ghosts excluded); `transcribe_cached` keeps the note/onset activations in the
analysis cache as `<sha256>.cdzbp`, used by MCP `detect_poly` (`ClipNotes.cache_dir`) and the app);
`drums::detect` (per-band onsets -> `DrumHit { time, kind: DrumKind (GM note), velocity }`);
`midi::{poly_to_midi, drums_to_midi}`. MCP `audio_to_midi { algorithm: auto|mono|poly|
percussive, drum_note }` (`notes::{detect_for, detect_poly, detect_drums}`, `ClipNotes::resolve`:
percussive >= 1 hit/s and < 30 % pitched, poly > 20 % chord time, else mono). App: Convert to
MIDI "Detection" row, note editor polyphonic view (chords as the read-only picture of a polyphonic
clip, `PolyLayers`, Convert to MIDI in the side column; "Chords" overlay chip otherwise); CLI `notes to-midi
--algorithm --drum-note`.
NE-9a live tuner: built-in effect `pitch_correct` (cadenza-dsp, Effect): YIN on a ~12 kHz
mono copy every 4 ms (40 ms window, full-rate refinement) -> nearest note of `key` + `scale`
(12 scales, `SCALE_MASKS`, against `reference` A4 Hz) -> shift = `amount` x (target - anchor)
+ `transpose`, followed with time constant `retune` ms; `humanize` = anchor averaged on held
notes (vibrato kept); streaming TD-PSOLA (cos²/sin² heads between synthesis marks, ratio 1 =
exact delay, formants kept); latency 20 ms (half the window: estimate centered on the audio,
p90 38 -> 8 c on a real take). Readings `readings::PITCH_DETECTED/OUTPUT/TARGET`; presets
Natural / Modern Pop / Hard Tune / Gentle; editor `ui::devices::pitch_correct` (live sung vs
corrected pitch, key strip, scale grid, knobs; compact in the inspector).
