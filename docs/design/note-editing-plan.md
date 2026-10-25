# Note editing: plan (audio tuning, audio-to-MIDI)

Status: plan (2026-10-24). **NE-0 to NE-8 shipped** (2026-10-24/25, branch `feat/note-editing`), **NE-9a (live pitch correction), NE-9b (local re-renders), NE-9c (poly cache), NE-9d (hear while dragging) and NE-9e (MIDI-guided Pitch Correct) shipped** 2026-10-25: see the end of their sections. Research and sources:
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

**Shipped (NE-3).** `cadenza_notes::midi::to_midi(&Segmentation, &Analysis, &TempoMap, origin,
&MidiOptions { quantize, quantize_strength, key: Off | Auto | Scale, bends, bend_range, min_length,
merge_repeats, octave_shift, range, velocity, region })` -> `MidiConversion { notes (song beats,
pitch, velocity from the core's 90th-percentile level, sung cents), bends (-1..1, a point when the
curve moves 3 c, reset after a detached note; mapped onto quantized notes), key, dropped }`;
`MidiOptions::hum()` = 1/16 grid, inferred key (Krumhansl, now `Scale::infer` in
cadenza-theory, shared with the library importer), repeats < 50 ms apart merged, notes < 80 ms
dropped. Mono line: a note ends where the next starts. MCP `audio_to_midi { clip_id, mode:
audio|hum, track_id?, quantize_beats, quantize_strength, key, bends, bend_range,
min_length_seconds, merge_repeats, octave_shift, lowest/highest, velocity, sensitivity }`
(`tools::notes`: `analyze_clip` decodes the clip's file and analyzes it through the content
cache, `convert` keeps what the clip plays, `conversion_command` = ONE Batch "Convert '<clip>' to
MIDI": a MIDI clip "<clip> (MIDI)" over the audio clip's span with an optional pitch-bend lane,
on the given instrument track or on a new one below the audio track (median pitch < C3 =
poly_synth "Sub Bass", < C4 = electric_piano, else poly_synth "Night Drive Lead"; colour and
folder copied)). CLI `cadenza notes to-midi <file> [-o out.mid] [--tempo] [--hum] [--quantize]
[--key] [--bends] [--octave-shift] [--lowest --highest] [--sensitivity] [--min-length]` runs the
same tool on a scratch session and exports the clip. App: audio clip menu "Convert to MIDI…"
(dialog: Recording | Humming, quantize grid + strength, key Off / Detect / 24 keys, pitch bend
+ range, octave, sensitivity (re-segments on a worker), write to a new track or an instrument
track; live preview picture and "n notes · key · range"; Convert = one undo step, then the
piano roll opens on the clip); dragging an audio clip onto an instrument track opens the dialog
aimed at it. Hum to MIDI: instrument track menu "Hum to MIDI when recording" (arms the track,
"H" toggle on the header; session state): the track records input 1 as audio and, when the
recording ends, its last pass is converted with the hum settings inside the same "Record" undo
step (the WAV stays in `Audio/`); the AI's recording context says so.

Segmentation gained a rule found by the round trip: a core shorter than 80 ms right next to a
note an octave, a twelfth or two octaves away is the tracker locking onto a harmonic (attacks,
beating unison oscillators) and is folded into that note (drift is measured over the core
ranges only). Round trip (`mcp/tests/audio_to_midi.rs`: 16-note line on the poly synth, engine
render at 44.1 kHz, import, convert): COnP F ≥ 0.95 in audio mode (all 16 notes), hum mode gives
the written notes back exactly (pitches, starts and lengths on the grid) and infers A minor.

Not done: folding hummed notes into "the instrument's range" (instruments declare none; the
lowest/highest option does it by hand), a per-track hum input picker (input 1), live monitoring
of the hummed voice, and converting every loop pass of a hum take (the active one is used).

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

**Shipped (NE-4).** Pure Rust in `cadenza-notes`, no new dependency:

- `edit`: `NoteEdit { pitch (new center), drift, modulation (shares kept, -1..2), formant_cents,
  gain_db, time: Option<TimeEdit { onset, offset }> }`, `Edits { notes (by index, missing =
  unchanged), sibilant_gain_db }` with `validate` (actionable errors naming the note; shifts ≤ 24
  st, formant ±1200 c). `curves` -> per-frame shift (semitones), formant (cents), gain: a core
  frame shifts by `(target − center) + (a_drift − 1)·drift + (a_mod − 1)·mod`, drift = 0.2 s
  double box of the deviation (removes vibrato), mod = the rest; transitions between connected
  notes blend smoothly (smoothstep) from one core's value to the next, attacks/releases take
  their core's value, and so do the pitched frames touching a note (`marks::pitched`: voiced
  frames plus up to 3 frames at each end that still hold a period within a semitone and are
  < 30 dB quieter: a short note's onset and decay outlast the voiced frames and otherwise kept
  its old pitch). `time_map`: anchors per note, a moved/stretched note keeps its attack (≤ 80
  ms) at speed 1, unedited notes stay put so the material between them stretches; crossing
  edits are refused. `correct_pitch(seg, &Correction { key, center, drift, threshold_cents })`
  = the Correct Pitch macro (nearest scale tone or semitone).
- `marks`: one mark per period on pitched stretches, 5 ms marks elsewhere, first at 0 and last
  at the end. The time of each cycle comes from the fundamental's phase (two-period Hann DFT at
  the analysis period: absolute and sub-sample, so nothing drifts; narrow band, so noise barely
  moves it) and the mark sits on the cycle's pulse — the period-rate component of the
  derivative's energy — through its offset from the fundamental's peak (pulses weaker than
  0.5 take the median offset of ±5 cycles). Both lessons were measured: peak picking on the raw
  waveform jumps between look-alike peaks (spacing 0.8–1.2 P, notes 8 dB down after a move),
  and grains centered off the pulse interpolate the spectrum between harmonics out of phase
  (−7 dB on some notes).
- `psola`: TD-PSOLA. A grain spans previous mark -> next mark with sin² / cos² slopes, so grains
  laid back at their marks sum to the input exactly (null render); output grains are one source
  period / ratio apart, taken from the source mark nearest the time map's source position
  (repeats/skips for time edits), read `2^(formant/1200)` times faster for formant moves (grain
  resampling: the envelope moves, the spacing does not; no vocoder path was needed: +300 c
  measures ×1.18 against ×1.189 asked), fractional placement by Catmull-Rom, gain curve last.
  Unvoiced marks keep ratio 1. Stereo: every channel gets the grains of the mono mix's marks.
- `resynth`: `render(channels, analysis, seg, edits) -> Render { channels, map }` (an unchanged
  edit state returns the input; reshaped notes are brought back to their own core level, ±6
  dB, because re-spaced grains sample the spectrum between harmonics); `verify` re-analyzes and
  re-segments the render and measures each note at its mapped core (the render's note covering
  half of it, else the vibrato-free mean over the core: a move can make a note look like its
  neighbour's fall); `render_verified` = up to 2 more passes taking the error against the
  original target out of each note further than 2 c (an integrating loop), keeping the best.
- CLI `cadenza notes tune <file> [-o] [--key] [--center %] [--drift %] [--vibrato %]
  [--threshold c] [--transpose st] [--formant c] [--sibilants dB] [--sensitivity] [--no-verify]`
  writes "<name> tuned.wav" and lists every note sung -> target -> after.

Measured (`tests/resynth.rs`, synthetic corpus; 22.05 kHz unless said): moves of 0..±3 st on
every note (closed loop) median 0.3 c, p95 2.0 c, worst 4.5 c (SNR 10 dB noise), every note
within ±5 c, also at 44.1 kHz; the first pass alone reached 9 c. Uniform shifts over the corpus
(p95 / worst on clean takes): −12 st 2.8 c / 7 c, −7 st 2.0 / 5.7, +7 st 1.8 / 3.5, +12 st
1.6 / 5.5 (−12 on the lowest voices goes under the tracker's 50 Hz floor: unmeasurable).
Null render < −80 dB through PSOLA itself; no new clicks at ±7/−3.5 st alternating steps (one-
off second-difference bursts, the detector checked on an injected click); level after a move
within 0.5 dB; +4 st keeps the LPC envelope (1.0 dB mean distance vs 5.3 dB for resampling);
modulation 0 removes the vibrato (depth 413 c -> 0) leaving the notes in place; moved/stretched
notes land within 20/30 ms of the asked onset/offset with their neighbours untouched; sibilant
gain −12 dB moves only the sibilants; a +7 st move of the SNR 10 dB take keeps ≥ 75 % of its
voiced frames (peak-picked or per-cycle pulse marks kept 30 %). Speed: the whole corpus at 44.1 kHz
(158 s) goes through analysis + segmentation + the closed loop in about 11 s (~15x realtime);
PSOLA itself is a small part of it.

Decision: PSOLA with formant by grain resampling is the default and only engine; no
listening comparison against WORLD or Signalsmith was made (the corpus is synthetic and nobody
has listened yet; samples via `cadenza notes tune` on `cadenza notes corpus`). Not done:
attack-speed edits (the attack is kept at speed 1), per-note fades, a vocoder path for very
breathy voices and big downward shifts (grains spread apart below −12 st), and any real-voice
listening test.

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

**Shipped (NE-5).** `cadenza-model::audio_notes`: `ClipContent::Audio.notes: Option<Box<NoteEdits
{ notes: [AudioNote { id: AudioNoteId, start, end, core_start, core_end (seconds of the file),
center (detected, fractional MIDI), pitch: Option (new center), drift, modulation (shares kept,
-1..2, default 1), formant_cents, gain_db, time: Option<NoteSpan>, confidence, flags:
[MaybeTwoNotes { at, cents } | ShortCore | WeakVoicing] }], sibilants: [NoteSpan],
sibilant_gain_db, sensitivity, user_splits, user_merges }>>` (every field optional in JSON,
defaults omitted, format_version 1; `validate`: time order without overlaps, core inside the
region, unique ids, ranges, ≤ 24 st from the sung pitch, ≤ 20 000 notes; errors name the note).
Commands `SetAudioNotes { clip, notes: Option }` (replace; "Detect notes" / "Remove note
edits") and `UpdateAudioNotes { clip, patches: [AudioNotePatch { id, pitch (double option),
drift, modulation, formant_cents, gain_db, time (double option) }], sibilant_gain_db? }` ("Edit n
audio notes"; inverse patches restore exactly). Summary: `audio_notes { count, edited,
avg_error_cents, flagged }`. Split/merge are not commands: `cadenza_notes::clip::{split,
merge_with_next}` record the user's separation / joined boundary and re-detect (`clip::detect`:
segmentation with `user_splits`, `user_merges` re-joined with the center measured over the
joined core; notes whose core lies mostly over a previous note's core keep its id and edits,
the rest get new ids from the caller), the result goes in with one `SetAudioNotes`; `to_segmentation`
/ `to_edits` feed the resynthesis. `cadenza-notes` now depends on `cadenza-model`.

Renders are derived data, not project content (a deliberate change from the plan's
`NoteRender { file, source }` + `.cadenza/notes/` + `collect_files`): `engine::note_render`
keys a 32-bit float WAV by sha256 (render version, file path, size, modification time, the
`NoteEdits` JSON) in `$CADENZA_NOTE_RENDERS` or `<user cache>/note-renders/`. So undo/redo never
writes the history to point at a render, undoing an edit finds its earlier render again, a
project folder stays small and portable, and another machine renders anew; there is nothing for
`collect_files` to copy. `render(file, edits)` = decode, analysis through the shared cache, the
closed loop (`render_verified`), atomic write. Engine compile: a clip whose edits change the
audio plays its render; offline renders/exports/freezes/bounces (ProcessMode::Offline) make a
missing one on the spot (so they always hear the edits); the realtime player plays the clip as
recorded with the warning `PENDING_WARNING`. `pending(project)` lists what is missing. App
(`state::note_renders`): after every change the missing renders are made one at a time on a
worker, each landing schedules a reload; the pending warning is kept out of the notices, a
failure is one notice and is not retried until the edits change. The audio editor's reverse /
stretch / transpose (new file with other times or pitches) drop the clip's notes in the same
undo step; normalize and gain keep them. Tests: model `audio_notes_are_set_edited_undone_and_saved`
(round trip of both commands, JSON with only the non-default fields, errors), notes
`clip::tests` (re-detection keeps ids/edits/splits/merges), engine `tests/note_render.rs`
(pending warning live, offline render makes it, offline == a plain clip of the render file
bit for bit == realtime playback, every note +2 st within 5 c, unedited = the file itself),
app `edited_clips_render_in_the_background_and_reload`; `no_alloc` unchanged (nothing new on the
audio thread). Not done: re-rendering only the changed range (a whole take renders: seconds),
per-note fades, a UI (NE-6), AI tools (NE-7).

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

**Shipped (NE-6).** The audio editor's toolbar has a Waveform | Notes switch
(`AudioEditorView.mode`); Notes mode = `ui::note_editor` over the pure `state::note_editor`.
Opening it (switch, audio clip menu "Edit notes…", inspector Clip tab "Edit notes") loads the
file's analysis on a worker (`NoteAnalyses`, per file, through the shared analysis cache) and,
when the clip has no notes yet, detects them as one undo step "Detect notes". Picture: key
column + semitone rows (scale rows when a key is chosen), notes as blobs whose thickness is the
level over time, center line, name + cents when there is room, the sung place outlined when a
note was retuned or moved, the resulting pitch curve (`edited_curve`: analysis pitch +
`edit::curves` shift, cached per edits) over everything, sibilants hatched along the bottom,
doubtful notes outlined amber with a ✂ at the suspected boundary (click = split there; tooltip
says why), the clip region with the outside dimmed, playhead. Tools (chips + keys 1–6): Main
(drag ⇕ retunes, snapped to the key's tones unless Alt or Snap is off; ⇔ moves in time, refused
with the reason when it would cross a neighbour; double-click splits, on a boundary merges),
Vibrato / Drift (shares), Formant (cents), Level (dB) (drag ⇕, double-click resets), Split
(click splits, double-click a boundary merges). Selection: click, Shift/Ctrl toggles, rubber
band, Ctrl+A, Esc; ↑ ↓ retune (Shift: octave), Del resets to as sung; right-click menu: Correct
pitch, Reset to as sung, Merge with the next note, Select all. Wheel scrolls the pitch range,
Alt+wheel zooms it, Shift+wheel / Ctrl+wheel scroll / zoom time. Toolbar: Snap, key
(Chromatic, "<key> (detected)" from `Scale::infer` over the notes, 24 keys), "n notes · k
edited · avg x c off", "rendering…" while the background render runs, Macros toggle. Macros
column (selection, else all notes): Correct pitch (center %, drift %, leave within n cents),
Quantize time (grid 1/4–1/32 of the song, strength; notes that would cross stay), Leveling
(evenness %: gain toward the mean core level, ±12 dB), Sibilants (dB, committed on release),
Detection (sensitivity, Detect again keeping edits/splits/merges, Remove notes). Every gesture
is one labelled step ("Retune notes", "Move notes", "Edit vibrato", "Correct pitch", "Quantize
note timing", "Level notes", "Split note", "Merge notes", "Detect notes again"...). Arrange:
clips with edited notes show ♪ before the name. F1 lists the note editor keys. Strings in all
10 languages. Tests: `state::note_editor` (snap/free retune, macros, time moves never cross,
tools + reset, hit tests, detect/split/merge/remove/undo through `AppState`), `tests/ui.rs`
`note_editor_detects_and_corrects_through_the_ui` (detection lands, the Correct pitch button
retunes all 8 notes to semitones, back to Waveform) and `render_note_editor_screens`
(detected, corrected in the detected key with one vibrato flattened, no macros column).

Not done: an instant local preview of the note being dragged (the edit is heard after the
background render, a fraction of a second for a phrase, seconds for a long take), attack-speed
and fade tools, a piano-roll ghost of the detected notes, an algorithm choice (mono only until
NE-8), and any measurement on real vocals (the < 2 % flagged target is unverified).

## NE-7 — AI tuning engineer

Split in two after an expert's session on a real take
([note-editing-expert-session.md](note-editing-expert-session.md)): the rules the expert
follows go into deterministic code first (NE-7a), the AI works on top of them (NE-7b).

**Shipped (NE-7a).** `cadenza_notes::refine` (second look at a segmentation: notes left
inside transitions — passing tones sung as slow-downs, re-articulations, landings — become
forced boundaries for one more partition; non-vocal blips dropped; notes holding another
pitch outside their core flagged) and `cadenza_notes::tuning` (`fit_scale`: every mode +
harmonic minor fitted jointly with the singer's offset; `targets`: per-note tone with a
reason — in scale, chromatic kept, joins next, scoop landing — a runner-up and an ambiguity
flag). On the expert's take: segmentation COnPOff 70.1 % -> 83.7 %, mixed regions 20 % ->
11.8 %, 0 false cuts; targets equal to the expert's on 51/52 notes (nearest semitone 33/52).
Synthetic corpus unchanged or better at every rate. The note editor's key defaults to the
detected fit ("E phrygian +33 c (detected)", Correct Pitch uses the targets; < 6 notes =
nearest semitone); `cadenza notes tune --key auto`. Labels in
`crates/notes/tests/data/expert_take.json` (`tests/tuning.rs`; `tests/expert.rs` needs
`CADENZA_NOTES_EXPERT_TAKE`, the recording is not in the repository).

**Shipped (NE-7b).** MCP `tools::tuning` (shared with the app's note editor):
`detect_audio_notes`, `analyze_vocal` (fit + alternatives, sung/now error, the decisions
left open: between two tones, chromatic kept, maybe two notes, unsure), `get_audio_notes`
(budgeted rows: sung, target, reason, runner-up, now, edits, flags), `tune_audio_notes`
(Correct Pitch toward the targets with `overrides`, center/drift/vibrato/threshold; the
app macro runs the same `correction_patches`), `edit_audio_notes`, `split_audio_note`,
`merge_audio_notes`. Notes are addressed by number in time order (ids cost too many tokens);
targets can come from a MIDI reference clip (`TargetReason::Reference`, octave of the
singer) or a chosen scale (`tuning::fit_for_scale`). `propose_splits` / `propose_targets`
became `analyze_vocal`'s decisions and `get_audio_notes`' columns; `quantize_audio_timing`
was not exposed. Agent: "# Tuning engineer" prompt section, `SessionAgent::run_tuning_pass(
&TuningBrief)` = one undo step "AI tuning pass"; app "Tune with AI" (note editor macros, audio
clip menu); CLI `cadenza tune <project> --clip|--track [--key] [--reference] [--notes]`.
Found on the way: a vibrato swing ending a run became a 60 ms note (swing absorption no
longer needs a glide) and a glide crossing could put a core end a hair past the note
(cores clamped); the expert take's mixed regions 11.8 % -> 10.9 %. Tests: scripted provider
(agent, app, CLI), `mcp/tests/tuning_tools.rs` (E Phrygian sung +30 c: scale and offset
found, 0 c after, vibrato untouched; overrides, edits, split/merge, reference). Not measured:
vibrato depth after the render of an AI pass (the tools leave modulation at 1 unless asked).

NE-7b as planned:

- MCP (`tools::notes`): `detect_notes { clip_id, algorithm?, sensitivity? }`,
  `get_notes { clip_id, range }` (beats + note names + cents error + confidence + flags,
  token budgeted), `tuning_report { clip_id | track_id, key?, reference_clip_id? }` (per note
  error vs scale or vs a MIDI reference, drift/vibrato stats, suspicious segmentations,
  summary numbers), `correct_pitch { clip_id, notes?, center_pct, drift_pct, mod_pct?,
  threshold_cents?, key? }`, `edit_audio_notes { clip_id, patches }`,
  `split_audio_note` / `merge_audio_notes`, `quantize_audio_timing`, `audio_to_midi` (NE-3).
- The deterministic proposals as tools: `analyze_vocal` (fit + alternatives, blips),
  `propose_splits` (refine candidates with their evidence and kind), `propose_targets`
  (targets with reason, runner-up, ambiguity): the LLM decides the ambiguous items (mode,
  ornaments, F vs F#), never reasons over frames.
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

**Shipped (NE-8).** `cadenza_notes::poly`: Basic Pitch hand-ported to Rust, no tract (it adds
minutes of build and failed on SwiftF0): the 35,699 weights copied unchanged from the ONNX
by `tools/basic-pitch/export.py` into `crates/notes/models/basic_pitch.bin` (143 kB,
`include_bytes!`; Apache-2.0 text + provenance in `models/`, shipped in every release
package). Front end = the graph's own CQT (9 octaves x 36 bins by half-band decimation with
its kernels and low-pass), log normalization per 2 s window, batch norm, harmonic stacking,
six convolutions; outputs equal onnxruntime's within 2e-4 (`one_window_matches_the_onnx_model`
with `CADENZA_BASIC_PITCH_REFERENCE`). Windows overlap 30 frames like the original, but each
frame keeps its exact time (the original's drift correction is not needed); windows run on up
to 8 threads (a 20 s take: 2.1 s; mono: 0.14 s). Note creation = the published algorithm
(onset peaks >= 0.5 tracked while the activation >= 0.3, 11 low frames tolerated, inferred
onsets, melodia trick, >= 128 ms); an optional overtone filter (`overtone_ratio`: a note on a
harmonic of a louder note starting within 40 ms, < 60 % of its activation) is off by default:
on a real piano loop (`120_G_Piano_412_01_SP`, the user's test) it took real fifths and octaves
over the bass, and without it the port gives exactly the reference package's 116 notes.
`poly::polyphony` = share of the sounding time with two or more notes, ghosts excluded (the
monophonic corpus: <= 0.11; that piano loop: 1.0).
`cadenza_notes::drums`: per-band onsets (lows 30-150 Hz, mids 150-4k, highs 6-16k; rise >= 9 dB
in 10 ms, within 35 dB of the band's loudest), grouped within 30 ms and labelled by absolute
band levels: kick owns the lows, snare/toms own the mids over the highs (crack 1-4k vs body
150-1k tells snare from tom, the low spectral peak places low/mid/high tom), hats are highs
alone (ring < 150 ms closed, < 600 ms open, else crash); GM notes, velocity vs the loudest of
the kind. `midi::{poly_to_midi, drums_to_midi}`. MCP `audio_to_midi { algorithm: auto | mono
| poly | percussive, drum_note }` (`ClipNotes.{poly, drums}`, `detect_for`; auto = percussive at
>= 1 hit/s with < 30 % pitched frames, poly at > 20 % chord time, else mono; drum clips go to a
new `drum_machine` track, chords to keys). App: "Detection" row in Convert to MIDI (Auto /
Melody / Chords / Drums, preview names it) and the note editor's polyphonic view
(`state::note_editor::PolyLayers`, per file on a worker for the open clip): a clip that sounds
chords shows them as the picture (read only, names when rows allow), a side column explains
that per-note editing works on single lines and offers Convert to MIDI; on single-line clips
a "Chords" chip overlays them. Found with the piano: a note sounding from the first frame
started at -2.5 ms and the model refused the whole detection (regions now clamped to the
audio). CLI `notes to-midi --algorithm
--drum-note`. Round trips (`mcp/tests/audio_to_midi.rs`): electric piano chords -> poly (F >= 0.9),
a drum_machine loop -> percussive kick/snare/hats (F >= 0.9), the NE-3 mono line still mono;
the user's take stays mono. Not done: the piano-specific model, "percussive pitched" (808/tom
pitch per hit), poly bends / MPE, a measurement on real guitar/piano and drum recordings
(synthetic sources only). Later (NE-9c): the model's note and onset activations are cached
on disk by content (`poly::transcribe_cached`, `<sha256>.cdzbp` in the analysis cache, note
creation rerun from them): the user's piano loop converts in 0.14 s instead of 0.91 s the
second time, a 3-minute take skips ~20 s.

## NE-9 — Later

- Realtime rendering of edits (no render step; streaming PSOLA per clip in the engine with
  lookahead, `no_alloc`): NE-9b made renders local instead, see below.
- Live tuner device (Auto-Tune-style retune speed + humanize; prior art expired) reusing the
  NE-1 detector in a realtime form.
- Learned boundary model trained on our corpus if DP + evidence plateaus.

**Shipped (NE-9a, live tuner).** Built-in effect `pitch_correct` ("Pitch Correct",
`cadenza_dsp::effects::pitch_correct`): YIN every 4 ms over 40 ms on a mono copy decimated to
about 12 kHz (2 x 12 dB/oct low-pass at a quarter of that rate), refined on the full-rate
difference function around the coarse lag (parabola), voicing hysteresis (CMND 0.2 on /
0.35 keep, -55 dBFS gate), 50 Hz-1.1 kHz. Target = nearest note of key + scale (12 scales,
0.15 st hysteresis) measured against `reference` (A4 415-466 Hz); shift = amount x (target -
anchor) + transpose, followed with time constant `retune` (0 = instant); `humanize` makes
the anchor a slow average (up to 0.5 s) on notes held over 100 ms, so vibrato and slow
movement inside sustained notes stay while their average is corrected (the expired 1998
Antares idea). Shifting = streaming TD-PSOLA: synthesis marks spaced period / ratio, each
reading the input around the analysis mark (on the input's period train) nearest to it; two
read heads crossfade cos²/sin² between consecutive marks (partition of unity: ratio 1 is
bit-exact delay, formants stay); Hermite reads; unpitched audio passes in 5 ms grains.
Latency = half the detection window (20 ms, reported, PDC): with half the longest period
(10 ms) the estimate lagged the audio and, on the user's real take (E phrygian, singer
+31 c, reference 448.5 Hz, retune 0), notes measured by `segment` were median 6.2 c / p90 38 c
from the scale; aligned: median 2.2 c / p90 7.8 c, long notes <= 6 c (dry take: median 27 c,
p90 82 c). Without matching `reference`, a singer 30 c sharp flips between neighbours on
some notes: the device description and the editor footer say so. Readings `PITCH_DETECTED /
OUTPUT / TARGET`; factory presets Natural, Modern Pop, Hard Tune, Gentle. Editor
(`ui::devices::pitch_correct` + `state::pitch_correct_editor`): readout (note now + cents,
target, correction; scale and reference), live 4 s pitch display (sung faint, corrected in the
track colour over semitone rows, scale rows and the target lit), key strip (click = key,
scale notes lit), scale grid, Retune / Humanize / Amount / Transpose / Reference; compact form
in the inspector. Agent prompt: the insert is the live alternative to the per-note tools.
Tests: `pitch_correct` unit tests (identity at amount 0, sharp notes land within 3 c with
their level, transpose / reference, retune speed, humanize keeps vibrato, melody + noise
without clicks), registry / no-alloc / random-params suites, app click test and screenshots.
Not done: MIDI-driven targets (effects get no MIDI yet), formant shift, a stereo-aware or
polyphonic mode.

**Shipped (NE-9b, local re-renders instead of a streaming engine path).** Measured first: on
the user's 20 s take a whole closed-loop render took 1.4-1.7 s (render 0.14 s, each
verification pass re-analyzes the whole take, 0.3 s, three passes), so about 13 s on a
3-minute take after every edit. Instead of a realtime PSOLA in the engine (a second
resynthesis path that could drift from the offline one, and no closed loop), a render now
re-renders only around what changed:
- `psola` = `plan` (where every grain goes: cheap, no audio) + `psola_range` (any window of
  output samples, bit-identical to the same samples of the whole render; test). Identity
  renders fall back on the source grid at the third mark of every unpitched run (`resync`),
  so two renders that differ before such a point lay the same grains after it (before this
  a moved note left the rest of the take offset by a fraction of a period).
- `resynth::{verified_with -> Verified { render, residuals, effective }, rerender_verified(
  .., Previous { channels, asked, effective }, edits)}`: unchanged notes keep what the
  previous render followed (its closed-loop pitch corrections and level fixes); the window =
  where the grain plans differ (`changed_span`) + where the curves differ + the changed notes,
  widened by the grains' reach (0.1 s); level fix and closed loop (shared `closed_loop`,
  `measure` on the window's own analysis) on the changed notes alone; the window is spliced
  into the previous render. `None` (whole render) on a time edit either side, another
  sibilant gain or a window over 60 % of the take. Test: the spliced result is exactly a
  whole render of the edits it followed (< 1e-6), nothing changes 0.5 s away from the note,
  the moved note lands within 5 c.
- Engine `note_render` (`RENDER_VERSION` 2): every render writes `<render>.json` (edits asked
  and followed) and `<file key>.renders` lists the file's last 8; a render starts from the
  earlier one with the same notes and fewest changed notes (`local` in `NoteRender`); edits
  are compared as JSON reads them back (serde_json does not round-trip every f64 to the bit);
  the pitch marks of the last file stay in memory. While a render is pending the player plays
  that closest earlier render (at most a quarter of the notes changed, at least one) instead
  of the recording.
On the user's take: whole render 1.67 s, then one note moved: 26-70 ms, the note within 2 c.
The realtime streaming path stays open for when edits must sound during a drag.

**Shipped (NE-9d, hear notes while dragging).** Melodyne plays a blob when it is pressed and
as it is dragged; here: the note editor's Main tool auditions the pressed notes and plays
them again whenever the drag moves a note 5 cents or more from where it was last heard
(`note_editor::{with_pitches, heard_differently, HEAR_AGAIN}`), transport stopped, toolbar
"♪ Hear" toggle (on by default). `resynth::audition` renders a stretch (file seconds) open loop
without level fixes or time edits (grain plan of the whole take + `psola_range`: milliseconds);
`state::note_audition::NoteAudition` keeps the take, analysis (shared content cache) and pitch
marks loaded per file on a worker and coalesces requests into the newest (`audition_notes`:
the dragged notes' span + 20 ms before / 60 ms after, at most 4 s, clip gain + track volume).
Engine: `snippet::Snippet` (built off the audio thread with 5 ms end fades, Hermite resampled
to the output rate) handed over through its own ring + garbage ring, `Player::{play_snippet,
stop_snippet}` (`Cmd::Snippet` replaces the playing one, which fades out over 5 ms), mixed
after the graph and before the monitor switches, never in renders; `no_alloc` test. The
committed edit still renders locally (NE-9b) when the drag ends.

**Shipped (NE-9e, Pitch Correct guided by a melody track).** The "not done" of NE-9a: effects
now take MIDI. Model: `Device.midi_input: Option<TrackId>` (effects only, from an instrument
track; optional in JSON), `Command::SetMidiInput { track, device, source }` ("Set MIDI input"),
validated (`ModelError::InvalidMidiInput`: an instrument device, a non-instrument source),
removing the source track disconnects it (undo restores), checked on load, summary
`midi_input`. Engine: an insert (track or master) with a MIDI input compiles its own
`MidiSchedule` from the source track's clips (`graph::DeviceMidi`) and collects it per quantum
like an instrument (`QuantumMidi::fill`: notes off + chase after a reset/seek, loop wraps, none
while previewing); live MIDI edits on a track feeding an effect recompile (`live::feeds_midi`).
No PDC offset for the notes (inserts before it shift the audio by their latency, a few ms
against held notes). DSP: `pitch_correct` param `midi` (0 ignore, 1 held note else the scale =
default, 2 only while a note is held), held keys from the block's events, target = the held
key nearest the sung pitch after moving it to the singer's octave (`nearest_held`).
`cadenza_dsp::effect_takes_midi`. MCP `set_midi_input { track_id?, device_id, source_track_id? }`
(rejects built-ins that ignore notes). App: the editor's "MIDI: <track> ▾" menu (instrument
tracks, none, the three modes; `AppState::set_midi_input`). Agent prompt line. Tests: model
round trip/validation/removal/save, DSP held-note targets per mode, engine offline render
following a silent guide track (chased after a seek), no-alloc with track and master inserts,
MCP tool, UI menu.
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
