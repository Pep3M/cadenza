# Note editing: plan (audio tuning, audio-to-MIDI)

Status: plan (2026-10-24). **NE-0, NE-1 and NE-2 shipped** (2026-10-24, branch `feat/note-editing`): see the end of their sections. Research and sources:
`docs/design/note-editing-research.md` (referred to as "research §n").

Separate roadmap from the main phases and from SL-n: steps are **NE-0 … NE-9**.

## Outcome

A polished, Melodyne-class note editor for audio clips — detect notes, retune / retime /
reshape them with natural results, and capture them as MIDI — built as one engine that later
serves hum-to-MIDI, audio-to-MIDI (incl. chords), automatic tuning and an AI tuning engineer.

Quality bar, in priority order:

1. **Segmentation is right** (research §5): a note region never mixes two real notes; its
   center comes from the stable core; doubtful regions are flagged and fixed by the
   analysis itself before the user sees them.
2. **Corrected notes are in tune, measured**: after a render every corrected note is within
   ±5 cents of its target (closed loop), not just "the blob was moved".
3. Natural sound: formants kept, sibilants/breaths untouched, attacks not smeared.
4. Everything is undoable `Command`s, reachable by the UI and the AI alike.

## Principles

- Clean-room: patents and papers for ideas, no code from proprietary products. No polyphonic
  per-note editing before legal review or 2029-06-15 (research §2). Polyphonic detection ->
  MIDI is in scope.
- Pure Rust, on by default: no dependency on the optional `stretch` feature for core
  functionality. ML models only through `tract` (pure-Rust ONNX), weights with permissive
  licences (SwiftF0 MIT, Basic Pitch Apache-2.0), shipped compressed in the binary or
  downloaded never (no network). The classic DSP path always works without models.
- Analysis is offline and cached; never on the audio thread.
- Note times inside a clip are **seconds of the source file** (like `offset_seconds` and
  fades: they describe the recording and must not move with tempo changes); AI tools speak
  beats and note names and convert at the edge.
- Measured, not eyeballed: every detection change runs the evaluation harness (NE-0) and
  must not regress its scores.

## New crate

`cadenza-notes` (depends on core, theory; `realfft`, `tract-onnx` behind an on-by-default
`models` feature): frame analysis, segmentation, decomposition, correction math, resynthesis,
MIDI conversion, evaluation. Pure, allocates, `thiserror`. `cadenza-model` gets the typed edit
state; engine/mcp/app/cli use the crate.

---

## NE-0 — Ground truth and evaluation harness

Before any detector: a way to know it is right.

- Synthetic corpus generator (`cadenza-notes::eval::synth`): renders monophonic phrases from a
  note list with known truth — glides of varying length, scoops/falls, vibrato (rate, depth,
  onset delay), drift, legato vs detached, repeated same-pitch notes, grace notes, melismas,
  octave leaps, breaths/sibilant noise bursts, reverb tail, background noise at several SNRs;
  timbres: additive voice-like source through formant filters, plus the built-in synths.
- Real set: a small hand-labeled folder of user recordings (not committed if not ours to
  ship; path from `CADENZA_NOTES_CORPUS`), labels in a simple JSON (onset, offset, pitch).
- Metrics: COnPOff / COnP / COn F-measure (mir_eval rules: ±50 ms, ±50 cents, offset 20 %),
  **center error in cents**, **mixed-region rate** (regions whose core spans two truth notes —
  the user's Melodyne pain point), split/merge error counts, octave error rate, voicing
  precision/recall.
- `cadenza notes eval [--corpus dir] [--json]`: prints the table; a CI test runs the synthetic
  set with thresholds (fast subset) and fails on regressions.

Acceptance: the harness scores a trivial baseline (YIN + "new note on > 50 cent jump") and
shows its mixed-region problem on glides — the bar later phases must beat.

**Shipped (NE-0).** Crate `cadenza-notes`: `yin` (FFT YIN), `baseline`, `eval::{synth,
labels, metrics}`, `cadenza notes eval [--detector] [--corpus dir] [--case] [--json]` and
`cadenza notes corpus <dir>` (WAV + labels). 20 synthetic cases, 246 notes; the whole run
takes ~1.4 s (release). Baseline on the synthetic corpus (22.05 kHz):

| | COnPOff F | COnP F | center median / p95 | over 10 c | mixed | split | merge |
|---|---|---|---|---|---|---|---|
| total | 61.1 % | 67.4 % | 3.3 c / 124 c | 41.7 % | 15.6 % | 1.2 % | 19.6 % |
| legato_glide_long | 25.0 % | 25.0 % | 96.5 c / 252 c | 83.3 % | 50.0 % | 0 % | 50.0 % |
| scoops_falls | 50.0 % | 50.0 % | 48.9 c / 66 c | 90.0 % | 0 % | 0 % | 0 % |
| steady / drift / repeated / breaths / octaves | 100 % | 100 % | < 1 c / ≤ 2 c | 0 % | 0 % | 0 % | 0 % |

It reproduces the Melodyne complaint exactly: across glides one region swallows two notes and
its center lands between them; scoops drag the mean flat. Tests: the truth scores perfectly
(`ground_truth_scores_perfectly_on_the_whole_corpus`), the baseline keeps those properties,
the CLI round trip (corpus written to disk = same scores). The real labeled set is still to
be collected (`CADENZA_NOTES_CORPUS`).

## NE-1 — Frame analysis (mono)

- Resample to an analysis rate (16 kHz for f0 models, 44.1/48 kHz kept for onsets/sibilants).
- **pYIN** in Rust: YIN difference function, threshold distribution -> f0 candidates with
  probabilities, HMM over candidates + unvoiced states (Viterbi), octave-jump penalty and
  harmonic-consistency check (research §4).
- **SwiftF0** via tract (feature `models`): f0 + confidence per 16 ms; fused with pYIN
  candidates in the same Viterbi (model as a strong prior, pYIN for fine resolution).
- Per frame (~5 ms grid after interpolation): f0 (cents), voicing prob, aperiodicity, RMS /
  short-term loudness, spectral flux, HF energy (sibilants).
- Cache: `.cadenza/analysis/<sha256 of file>-<analysis version>.bin` (compact, versioned,
  safe to delete; project-less: user cache). Background worker with progress + cancel.
- CLI `cadenza notes analyze <file> [--json]`; app: analysis starts when a clip opens in the
  note editor (or on demand), never blocks the UI.

Acceptance: voicing F1 and raw pitch accuracy on the corpus at or above published pYIN
levels; octave errors < 1 % of voiced frames on the synthetic set; a 3-minute vocal analyzed
in < 3 s on one core (models on).

**Shipped (NE-1).** `pyin` (threshold-distribution candidates + Viterbi over candidates and an
unvoiced state, O(frames x candidates²)), `features` (level, spectral flux, HF share on a 23 ms
window), `analysis::analyze` -> `Analysis` (5 ms frames: pitch, voicing, aperiodicity, level,
flux, HF share, top-3 candidates), frame-parallel over cores, progress + cancel, on-disk
`Cache` keyed by content. CLI `cadenza notes analyze <file> [--json] [--no-cache]` and
`cadenza notes eval-pitch [--tracker pyin|yin]`. Frame scores on the synthetic corpus:

| | raw pitch | voicing recall | false alarm | octave | gross | p95 |
|---|---|---|---|---|---|---|
| YIN | 97.7 % | 99.0 % | 9.1 % | 0.6 % | 0.7 % | 10 c |
| **pYIN** | **98.5 %** | 98.7 % | **7.5 %** | **0 %** | **0.1 %** | **8 c** |
| YIN, noise SNR 10 dB | 73.8 % | 98.0 % | | 11.1 % | 13.6 % | 2400 c |
| **pYIN, noise SNR 10 dB** | **95.1 %** | 95.2 % | | **0 %** | **0.1 %** | **10 c** |

The paper's Beta mean 0.1 kept octave errors at 0 but lost voicing (93.7 % raw pitch, 32 % at
SNR 10); mean 0.2 plus a small bias against "unvoiced" in frames with a period candidate gave
the table above (sweep over beta, bias and switch cost). Remaining misses are window-bound:
fast melisma glides (the 40 ms window averages them) and frames just after a note ends
(false alarms). Speed: a 3-minute 44.1 kHz take in 0.29 s on 16 cores (2.2 s on one), a cache
hit in 0.06 s.

**SwiftF0 deferred.** Its ONNX (MIT, 135 kB, raw 16 kHz audio + fmin/fmax in, STFT inside the
graph) does not load in tract 0.23.9 ("Impossible to unify F32 with F64" at MatMul node
`v128`), and tract adds minutes of build time. pYIN already meets the NE-1 bar, so the model
waits until real recordings (bleed, breathy voices) show where pYIN fails; options then: a
hand port of the 14k-parameter network (weights read from the ONNX), or a fixed tract. The
`PitchTracker` trait and the candidate lattice are ready to take its estimates.

## NE-2 — Note segmentation and pitch decomposition (the core)

Implements research §5 and §6:

- Boundary evidence per frame: onset strength, consonant/aperiodicity, voicing changes,
  energy dips, pitch moves that settle.
- **Exact DP segmentation** (PELT-style pruning): segment = transition-in + stable core +
  transition-out, Huber cost of the core about its own center, vibrato-aware term,
  evidence-weighted boundary costs; minimum plateau rules (scoops/glides < ~120 ms without a
  plateau are transitions; plateaus ≥ ~60–80 ms are notes; vibrato never splits; repeated
  notes split only on onset evidence).
- Boundary placement at the glide's pitch midpoint or at the onset; transition zones stored
  explicitly and excluded from centers.
- Center = weighted mode of the core over whole vibrato cycles.
- Per-note **confidence** + self-check (bimodality BIC, edge drift, octave flags) and
  automatic re-split/merge proposals applied when they lower the global cost; remaining
  doubts kept as `flags` for UI/AI.
- Decomposition: drift (low-pass ~2–3 Hz) + modulation (vibrato rate/depth measured) per note.
- Sibilant/unvoiced segments as their own objects attached to the next/previous note
  (Melodyne Melodic behaviour: they never get pitch-shifted).
- Parameters are tunable but have tested defaults; a "sensitivity" knob maps to the boundary
  penalty for the user.
- Re-segmenting a range after a manual split/merge is local and instant (< 50 ms).

Acceptance (synthetic set): mixed-region rate < 1 %, COnPOff F ≥ 0.90, median center error
< 3 cents, 95th percentile < 8 cents; on the real set: clearly better than the NE-0
baseline, every remaining mixed region flagged with low confidence.

**Shipped (NE-2).** `segment::segment(&Analysis, &SegmentOptions, splits) -> Segmentation {
notes: [SegmentedNote { onset, offset, core_start, core_end, center, drift (c/s), vibrato:
Option<{rate_hz, depth_cents}>, confidence, flags: [MaybeTwoNotes {at, cents} | ShortCore |
WeakVoicing] }], sibilants }`; `Segmenter` is the `segment` detector. CLI `cadenza notes
segment <file> [--sensitivity] [--split <s>]... [--json]`. Pipeline per voiced run (gaps
≤ 25 ms with continuous pitch and no attack are bridged; forced splits cut runs; reverb tails
are cut out first):

1. `vibrato::detect`: per frame, a detrended ±0.25 s window is fitted by a sinusoid at
   4–8.5 Hz; vibrato when it explains ≥ 60 % of the residual, depth 12–200 c, the residual
   crosses zero ≥ 0.75 x as often as the sine, the sine fits at least twice better than a
   staircase of up to three notes, and the window both rises and falls by the minimum depth
   (a run of rising notes is not vibrato). Regions ≥ 0.35 s are replaced by their mean over
   one period: vibrato never splits a note and centers are its mean.
2. `partition`: exact DP over core / transition frames; a core is a line with |slope| ≤ 150
   c/s, priced by its squared error plus `note_cost` (10 000 c², / sensitivity); a transition
   frame costs 300 where the pitch moves (> 300 c/s over 30 ms) and 4 000 where it holds, so
   plateaus are notes and glides are not.
3. Cores closer than 35 c across a glitch ≤ 60 ms merge; a short (< 100 ms) core at a run
   edge joined by a glide (or itself at the slope limit) is absorbed: the bottom of a fall
   (≥ 10 dB quieter, or going down and ≥ 3 dB quieter), the start of a scoop (median level ≥
   12 dB lower; a grace note reaches full level), or the swing of a vibrato the analysis
   cannot see in the run's last half window.
4. Boundaries between legato notes sit where the glide crosses halfway between the two
   centers; centers come from the cores only; `self_check` prices the best split of each core
   (confidence, `MaybeTwoNotes` when it would gain over half a note's price and the parts are
   > 50 c apart).
5. Reverb tails: a level fall of 12 dB within 100 ms that does not come back within 8 dB for
   ≥ 60 ms is cut from the run (the dry offset is the note's end).

The pYIN stage gained a rule found on noisy vowels with a strong second harmonic: a dip below
the threshold whose aperiodicity is above 0.1 is passed over for a later one 4 x deeper (an
octave-up dip at half the period is shallow, a true period's subharmonics are not), and the
YIN window is ~46 ms at every rate (a power of two made it 64 ms at 16 kHz).

Synthetic corpus (22.05 kHz, 246 notes in 20 cases):

| | COnPOff | mixed | split | merge | octave | center median | p95 |
|---|---|---|---|---|---|---|---|
| baseline (NE-0) | 68.9 % | 41.8 % | 15.9 % | 15.9 % | 0 % | 2.9 c | 211 c |
| **segment** | **99.6 %** | **0 %** | **0 %** | **0 %** | **0 %** | **0.4 c** | **3 c** |

Every case is exact but `reverb` (one onset off). At 16 / 44.1 / 48 kHz: COnPOff 97.6 / 97.2 /
98.4 %, no mixed region but one at 16 kHz (0.8 %); the misses are short fragments at the
edges of notes in noise (falls into consonants and scoops whose level the noise floor hides)
and one octave-wrong note at 16 kHz. Floors in `tests/segment.rs`. Speed: 21 minutes of audio
segmented in ~2 s (cache hit included). Frame scores after the corpus melodies stopped
repeating notes: pYIN raw pitch 97.9 %, octave 0 %, gross 0.1 %, false alarm 7.3 % (YIN
98.2 % / 0.2 % / 0.2 % / 9.1 %; its extra frames are glide edges, some at the wrong octave).

Not done in NE-2, moved on: per-note drift and modulation *curves* (the decomposition the
editor draws, NE-5; the notes carry drift slope and vibrato rate/depth), local re-segmentation
of a range after a manual edit (a whole 3-minute take re-segments in ~0.3 s, so NE-5 can start
with that), Huber costs and BIC self-checks (the squared-error DP plus the split self-check
reached the targets), very wet reverb (tuned at 30 % wet) and the real labeled set.

## NE-3 — Audio-to-MIDI (mono) and hum-to-MIDI

- `notes::to_midi(notes, &MidiOptions { bends, bend_range, velocity_curve, quantize,
  min_length, merge_repeats, key_snap, octave_shift })` -> notes in beats + bend points;
  builds one `Batch` (`AddClip` + `SetControllerLane pitch_bend`) like MIDI import.
- App: audio clip menu "Convert to MIDI…" (dialog with the options and a live preview count),
  creates an instrument track below (default instrument by register: bass, keys, lead) or
  writes into a chosen one; result opens in the piano roll. Drag an audio clip onto an
  instrument track = convert.
- Hum-to-MIDI: record-to-MIDI mode on an instrument track: records audio internally (existing
  recording path), converts with "hum" defaults (stronger smoothing, quantize to grid + key,
  fold to the instrument's range), lands as one undo step; the take audio is kept in `Audio/`.
- MCP `audio_to_midi { clip_id, track_id?, quantize, bends, key_snap, ... }`.

Acceptance: round trip "MIDI -> synth render -> audio_to_midi" recovers the notes (COnP F ≥
0.95 on clean synth material); a hummed melody lands quantized and in key.

## NE-4 — Resynthesis engine (pitch, formant, time, sibilants)

- Pitch-mark (epoch) detection on voiced cores; **PSOLA** with the continuous ratio curve
  `r(t)` from the edit state (research §6–7); unvoiced/sibilant objects pass through with
  their own gain (sibilant balance); crossfades at object borders.
- Formant: spectral envelope (true envelope / cepstral) warp on a vocoder path, used when a
  formant edit is set or when the shift exceeds what PSOLA does cleanly (switch decided per
  note by size of shift + periodicity).
- Time: piecewise time map (moves, stretches, attack speed) applied by the same overlap-add
  (grain repetition/skipping); attacks kept at ratio 1 unless attack speed is edited.
- Amplitude, per-note fades, leveling: gain curve.
- **Closed loop**: render -> NE-1/NE-2 on the result -> residual cents per note; a test asserts
  ±5 cents for every corrected note of the corpus; residuals feed back once (re-render with a
  corrected ratio) when needed.
- Listening comparisons against WORLD-style resynthesis and Signalsmith (when built) decide
  the defaults; the decision is written here.

Acceptance: corrected notes within ±5 cents (closed loop) on the corpus; null test: an edit
state with no changes renders bit-close to the original (residual < −80 dB); no clicks at
object borders (transient detector on the output).

## NE-5 — Project model, commands and playback

- `cadenza-model`: `ClipContent::Audio` gains `notes: Option<Box<NoteEdits>>` (optional in
  JSON, `format_version` stays 1):
  `NoteEdits { analysis: AnalysisRef { file_hash, version, algorithm: melodic|percussive|poly },
  notes: Vec<AudioNote { id: AudioNoteId, start, end, core_start, core_end /* seconds of the
  file */, center_cents (detected), pitch_cents: Option<f32> /* new target */, drift_pct,
  mod_pct, formant_cents, gain_db, fade_in, fade_out, time: Option<TimeEdit> }>,
  user_splits: Vec<f64> /* manual separations survive re-analysis */,
  render: Option<NoteRender { file, source /* fingerprint, like Freeze */ }> }`.
  Validation with actionable errors (ordering, ranges: pitch ±48 st, pct −100..200, …).
- Commands: `SetAudioNotes { clip, notes: Option<NoteEdits> }` (replace; detection results come
  in through it), `UpdateAudioNotes { clip, patches }` (per-note patches, the gesture command),
  `SplitAudioNote` / `MergeAudioNotes` (carry new ids). Builders in `model::audio_notes`.
- Engine: an edited clip plays its **rendered copy** (worker render, like freeze/bounce and the
  audio editor's processed copies, into `.cadenza/notes/`); a stale fingerprint plays the
  original with a warning until the re-render lands (debounced, background, per clip, only the
  changed range re-rendered and spliced). Export/freeze/bounce wait for pending renders.
- Summary: clips with notes show `notes: {count, edited, avg_error_cents, flagged}`.

Acceptance: undo/redo of every edit, save/load round trip, relative paths, `collect_files`
copies renders, offline render == realtime playback of the rendered copy, `no_alloc` untouched.

## NE-6 — Note editor UI

In the audio editor dock (`view.audio_editor`), a "Notes" mode beside the waveform mode
(double-click an audio clip; Graphite look, shares the piano roll's key column/grid helpers):

- Blobs drawn by amplitude envelope on a pitch grid, pitch curve on top, center line,
  transition zones shaded, sibilants as hatched blocks, flagged notes outlined amber with the
  reason in a tooltip ("may contain two notes — split?" + one-click fix).
- Tools (Melodyne parity, research §3): Main (move pitch ⇕ snapped to scale / Alt free, time
  ⇔, edges), Pitch modulation, Pitch drift, Formant, Amplitude, Time / attack speed, Note
  separation (double-click split/merge; separation drag), Sibilant balance. Keys like the
  piano roll; selection, rubber band, Ctrl+A.
- Macros panel: Correct Pitch (center %, drift %; only notes off by > n cents option),
  Quantize Time (%), Leveling (%); scale picker with detected key ("A minor, 92 %").
- Inspector Clip tab: note editing on/off, algorithm, sensitivity, re-detect.
- Audition: playing the clip plays the edited render; editing a note previews that note
  instantly from a quick local render (PSOLA is cheap enough), full render after release.
- Arrange: edited audio clips get a "♪" badge; piano-roll ghost of the detected notes when
  an audio clip and a MIDI clip are compared.
- i18n strings in all 10 locales; screenshots in `tests/ui.rs` (`render_note_editor_screens`).

Acceptance: a pitched vocal take can be fully tuned with the macros + a few manual touches;
flagged notes are rare (< 2 % on real material) and each has a one-click fix.

## NE-7 — AI tuning engineer

- MCP (`tools::notes`): `detect_notes { clip_id, algorithm?, sensitivity? }`,
  `get_notes { clip_id, range }` (beats + note names + cents error + confidence + flags,
  token budgeted), `tuning_report { clip_id | track_id, key?, reference_clip_id? }` (per note
  error vs scale or vs a MIDI reference, drift/vibrato stats, suspicious segmentations,
  summary numbers), `correct_pitch { clip_id, notes?, center_pct, drift_pct, mod_pct?,
  threshold_cents?, key? }`, `edit_audio_notes { clip_id, patches }`,
  `split_audio_note` / `merge_audio_notes`, `quantize_audio_timing`, `audio_to_midi` (NE-3).
- Agent prompt section "Tuning engineer": analyze -> fix segmentation flags -> correct with
  musical restraint (keep intentional slides, blue notes, vibrato) -> re-analyze -> report
  before -> after cents; one undo step per request; app: "Tune with AI" on audio clips /
  the note editor; CLI `cadenza tune <project> --track ...`.
- Reference-aware tuning: when a MIDI melody or the chord track exists, targets come from it.

Acceptance: scripted-provider tests; on the corpus the AI pass reaches the same residuals as
the macros without flattening intentional expression (vibrato depth kept within 10 %).

## NE-8 — Polyphonic and percussive detection -> MIDI

- **Basic Pitch** via tract (Apache-2.0 weights): HCQT front end in Rust, note creation
  post-processing (research §8), bends from the contour output (single-voice passages only).
- Optional piano model (Onsets and Frames class, Apache-2.0) when a clip is tagged piano.
- Percussive: onset detection -> events; drum mapping by spectral class (kick/snare/hats/
  toms), or one chosen note; "Percussive pitched" (808, toms): pitch per event.
- Polyphonic notes **display read-only** in the note editor (chords visible, convertible to
  MIDI, usable as a tuning reference), no per-note editing (legal gate).
- App "Convert to MIDI" picks the algorithm (auto: mono / poly / percussive classifier).

Acceptance: chords from a guitar/keys recording land as MIDI chords (note F1 near the paper's
numbers for the material); drum loops -> drum pattern clip playable by `drum_machine`.

## NE-9 — Later

- Realtime rendering of edits (no render step; streaming PSOLA per clip in the engine with
  lookahead, `no_alloc`).
- Live tuner device (Auto-Tune-style retune speed + humanize; prior art expired) reusing the
  NE-1 detector in a realtime form.
- Learned boundary model trained on our corpus if DP + evidence plateaus.
- Polyphonic per-note editing after legal review / patent expiry.
- MPE export of polyphonic bends.

## Order and dependencies

NE-0 -> NE-1 -> NE-2 -> (NE-3 ∥ NE-4) -> NE-5 -> NE-6 -> NE-7; NE-8 after NE-3 (needs only
NE-1 infra + tract). First user-visible value: NE-3 (audio/hum-to-MIDI); first tuning: NE-6.

## Risks

| Risk | Mitigation |
|---|---|
| Segmentation on expressive singing (melisma, fry, breathy) | evidence fusion + confidence flags + local re-segmentation; corpus grows with each failure the user reports |
| PSOLA artifacts on big shifts / breathy voices | per-note switch to the envelope vocoder path; closed-loop + listening tests |
| Model size / CPU | SwiftF0 ~14k params, Basic Pitch ~17k: tiny; DSP fallback always works |
| Patent exposure | polyphonic editing gated; DNA's combination not reproduced; legal review before NE-9 poly |
| Render latency while editing | local quick render for the touched note; background full render; splice by range |
