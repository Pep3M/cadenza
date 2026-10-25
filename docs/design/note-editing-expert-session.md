# Expert vocal-tuning session: what the expert did and what Cadenza learned from it

> NE-7a input. Section D at the end lists what was implemented from it and the numbers
> after; sections A-C are the analysis as written (in-sample: one singer, one 20 s take).


Source: `a user project (`testing_record.cdz`)` (55 KB JSON). The three audio tracks play the same file,
a processed copy of one recording (not in the repository) (20.67 s, 48 kHz).
All times below are **file seconds**. Pitches are MIDI numbers (52 = E3). Analysis was run with the
release CLI (`cadenza notes analyze --json` and `notes segment --json` at several sensitivities and with
the user's splits). The analysis scripts (Python over the CLI's JSON) were throwaway and are not kept; the labels are `crates/notes/tests/data/expert_take.json`.

## 0. Provenance and what the data can and cannot tell us

* **Autosaves**: the project's `.cadenza/autosave/` is empty, and so is the user state autosave folder.
  A save clears the autosaves, so no intermediate snapshots are left.
* **`.cdz.bak`** (the previous save) holds `cortes ajustados`, `cruda` and a track named `Audio 3`.
  `Audio 3` matches the final `afinada completa` on every note and edit field, so the last save only
  renamed the track. **We have no edit history.** We only have three end states.
* **Track 2 can be regenerated exactly.** `notes segment <wav> --split t1 ... --split t12`
  (the 12 stored `user_splits`) gives back the stored 55 notes, and plain `segment`
  (sensitivity 1) gives back track 1. So the expert's whole segmentation work is the
  **12 split positions**. There are **no `user_merges`** and no deleted notes.
* **The tuning edits are pitch only.** On all 55 notes, `pitch` is set to an integer MIDI note. `drift`,
  `modulation`, `formant_cents`, `gain_db`, `time` and `sibilant_gain_db` are at their defaults on every
  note. The "pitch/drift/modulation/formant/gain/time edits" in the brief therefore do not appear in the
  data: the expert used **center correction only, at 100 %, and left vibrato and drift as sung**.
* **Likely workflow** (an inference): even notes 1 to 3 c off have `pitch` set (#30 +1 c, #34 -1 c,
  #35 -2 c, #11 -2 c), and so do the three metronome-click "notes" (see A.6). This fits a run of
  *Correct pitch* (defaults: chromatic, 100 %, threshold 0) over all notes, followed by **19 manual drags
  to a different semitone** than the nearest one.

---------------------------------------------------------------------------------------------------

## A. Segmentation: automatic (track 1) vs. expert (track 2)

### A.1 Headline numbers

The 3 E6 click blips are excluded from all scores. That leaves 42 automatic notes and 52 expert notes.

| measure | value |
|---|---|
| expert split points | 12, inside 8 automatic notes |
| net new notes | +10 (45 -> 55) |
| automatic boundaries moved by the re-segmentation after a split | 3: 5.264 -> 5.178 (-86 ms), 15.862 -> 15.823 (-40 ms), 16.973 -> 17.064 (+91 ms). Also 11.493 -> 11.473 (-20 ms) from the split at 11.476 |
| merges / deletions | 0 / 0 |
| boundary precision / recall (onsets + offsets + legato joins, ±30 ms) | **P 0.95, R 0.81, F 0.88** (57 of 70 expert boundaries found, 3 automatic ones not kept) |
| legato joins (note-to-note boundaries with no gap) found | 21 / 34 |
| notes matched (onset ±50 ms, offset, center ±50 c) | 34 / 52 = **65 %** |
| `cadenza notes eval --detector segment` on the expert labels | **COnPOff 70.1 %**, COnP 76.3 %, COn 82.5 %, mixed regions 20 %, merges 20 %, splits 1.9 %, octave 0 % (synthetic corpus: 99.6 % COnPOff) |
| same, `baseline` detector | COnPOff 10.5 %, mixed 58 % |
| automatic notes with a `MaybeTwoNotes` flag | **0 of 45** (every note had confidence 1.0). It caught none of the 8 notes the expert split |

**Conclusion:** the automatic detector is accurate where it does cut (precision 0.95), but it
**under-segments**. Every error is a missing note, never an extra one. The only automatic boundaries the
expert changed are the 3 that the re-segmentation itself moved.

### A.2 Every difference, with the audio evidence

Pitch values come from the pYIN frames. Times have 5 ms resolution, and levels are frame dBFS (the loudest
frame in the take is -10.4 dB).

| # | expert split(s) | automatic note | what the pitch curve does there | evidence at the cut | expert notes (center -> target) |
|---|---|---|---|---|---|
| 1 | **1.114** | 0.993-1.327, core 1.213-1.292, c 52.64 | Falling **pickup** from 50.8 to 49.3 (0.995-1.095, about 100 ms). Then a **jump of +210 c in 20 ms**, an overshoot to 53.5, and a settle at 52.6 | level dip **-8.7 dB** at the cut (-15 -> -24 -> -17 dB), slope 9700 c/s. This is a re-articulation (new syllable) | 50.19 -> D3 (120 ms, core 30) / 52.64 -> E3 |
| 2 | **3.989** | 3.787-4.123, core 3.798-3.942, c 51.23 | Holds Eb3 (51.0-51.6), **falls 320 c in about 50 ms**, then **holds C3 48.0-48.1 for about 85 ms** (4.01-4.09) and decays | lower note about 8 dB quieter (-22 -> -31 dB), no gap. The `edge_glide_core` / `fall_drop_db` rule ("fall bottom") absorbed it | 51.23 -> Eb3 / 48.02 -> C3 (135 ms, core 95) |
| 3 | **4.951** | 4.827-5.264, core 4.942-5.083, c 48.15 | **Flat attack plateau** at 47.6-47.7 (4.83-4.94, 110 ms), then a slow rise to 48.1-48.3: a step of **only +55 c** | no level dip (-0.1 dB), no flux peak, slope 1100 c/s | 47.69 -> **C3** / 48.24 -> **C3**. Both halves go to the same note: the split exists so that each half can be centered separately (a "straighten a two-level note" move done with the split tool instead of drift) |
| 4 | **5.409** (+ automatic join moved 5.264 -> 5.178) | 4.827-5.264 + 5.264-5.613 (c 48.15 / 51.99), core of the second note 5.467-5.613 | **Stepwise ascent C -> D -> E.** D3 holds at 50.3-50.65 for about 140 ms (5.26-5.40), but the automatic detector made it the glide between C and E | rearticulation at 5.17 (jump 153 c, dip 9.8 dB). At 5.409: dip -2.4 dB, flux 0.21 (twice the median), slope 2900 c/s | 50.57 -> D3 (230 ms) / 52.20 -> E3 |
| 5 | **11.476** (moves 11.493 by -20 ms), **11.617**, **11.711** | 11.280-11.493 (c 55.80) + 11.493-11.848, core 11.753-11.848, c 52.31 | **Melisma on one vowel:** G (55.7) -> dip to 53.8 -> back to 55.5 (a lower-neighbour turn, 11.47-11.61) -> **slows down through F** (53.4 -> 53.0 at about 1000 c/s, 11.64-11.70, between 3500 and 2400 c/s stretches) -> E (52.0-52.5) | no gaps. Dips of -3.3, -2.3 and -0.1 dB | 55.80 -> G3 / 54.99 -> G3 (140 ms, core 30) / 53.10 -> F3 (95 ms, core 45) / 52.31 -> E3 |
| 6 | **12.868**, **12.950** | 12.803-13.057, core 12.958-13.018, c 49.20 | **Rising run:** B2 hold (46.8-47.0, 40 ms) -> slows at 48.45-48.66 (12.89-12.94, about 50 ms) -> peaks at 49.2-49.3 (12.97-13.01) -> falls into a gap | steps +118 c and +67 c, slopes 3100 and 1700 c/s, no dips | 47.35 -> B2 (65 ms) / 48.53 -> C3 (80 ms) / 49.20 -> **C3** (-120 c: the overshoot to C# is pulled back to C, which reads as B -> C with an overshoot) |
| 7 | **15.745**, **15.826** (moves 15.862 by -40 ms) | 15.500-15.862, core 15.513-15.727, c 53.54 | F holds at 53.6 -> **level dip -7.2 dB** + a jump to 54.2-54.3 (15.765-15.81, about 50 ms) -> falls to E 52.45 | a re-articulated syllable sung sharp | 53.54 -> F3 / 54.19 -> **F3** (-119 c) / 52.47 -> E3 |
| 8 | **16.874** (moves 16.973 by +91 ms) | 16.254-16.973 (c 50.85) + 16.973-17.798 (c 47.25) | **Stepwise descent D -> C -> B.** C3 holds at 48.85-48.96 for about 80 ms (16.95-17.03), but the automatic detector made it the transition | no dip (-0.6 dB), slope 2400 c/s | 50.82 -> D3 / 48.89 -> C3 (191 ms, core 115) / 47.27 -> B2 |

**Where the missed notes are.** Each of the 10 new expert notes has its core **0-9 % inside the automatic
cores**. Every one sat in the automatic onset or offset transition zone:
before the core (1.0, 4.83, 5.26, 11.47-11.70, 12.81-12.94) or after it (4.00-4.12, 15.76-15.82,
16.95-17.03).

**Classification of the 10 missed notes:**

* **Passing tones in stepwise runs** (D in C-D-E at 5.26, F in G-F-E at 11.65, C in B-C at 12.90, C in
  D-C-B at 16.95): **4**. These are slow-downs or holds of 40-140 ms in the middle of a run. The segmenter
  takes them as a glide because the slope is above `max_drift` 150 c/s while still far below the glide
  speed.
* **Phrase-end landing note taken for a "fall bottom"** (C3 at 4.00-4.12, 8 dB quieter): **1**.
* **Re-articulations** (pitch jump with a 7-10 dB level dip: 1.10, 15.745): **2**, plus the pickup that
  each one separates.
* **Melisma turn** (11.47-11.61, a lower-neighbour dip and return): **1**.
* **Two-level note split to tune each half** (4.95, +55 c, same target): **1**.
* **Rising overshoot split off and pulled back** (12.95, 49.2 -> 48): **1**.

### A.3 Which features predict a needed split

Positives are the 8 automatic notes the expert split; negatives are the 34 real automatic notes left whole
. The test is a best two-part piecewise-constant fit over the **whole note
region (onset..offset)**, not just the core:

| feature | positives | negatives |
|---|---|---|
| step between the two parts | 49, 159, 162, 182, 184, 220, 290, 308 c | 0-55 c for 28 notes. Larger: 79, 79, 102, 128, 139, 139, 161, 770 c |
| region pitch RMS around its median | 32, 80-185 c | 12-56 c for 31 of 34 notes. Larger: 70, 81, 83, 383 c |
| rule `step >= 150 c` | 7 / 8 found | 2 / 34 false |

The negatives with large steps are informative, because they show what the expert does **not** split:

* **Monotonic attack scoops or falls with no hold:** 1.373 (52.5 -> 50.5), 6.162 (52.0 -> 53.6 over
  120 ms), 9.128 (fall-in 56.1 -> 53.7), and 14.808 (an octave scoop from 41.8 to 51.4 over 280 ms).
* **A quiet pickup of about 50 ms:** 11.107-11.16 (51.8 at -23 dB, then +160 c and +10 dB). Compare
  split #1 at 1.114, where the pickup lasted about 100 ms.
* **Ornaments left inside the note:** a dip of about 90 c at 2.63-2.72, and an upper-neighbour bump
  of +170 c at 16.30-16.36 inside the long D (the expert did not split it).
* **Case 16.973** only looks like a negative: its plateau belonged to split #8.

**What separates them:**

1. **Hold vs. glide.** Is there a sub-segment where the pitch *slows down* (|slope| < about 1000 c/s over
   30 ms) for >= 25-40 ms? A scoop or fall changes pitch all the way through.
2. **Duration.** The shortest expert note is 65 ms (core 30 ms). Pickups and turns of 80-140 ms were
   kept as notes, and a 50 ms quiet pickup was not.
3. **Pitch distance.** Splits with no other cue needed >= 60 c. With a re-articulation cue (level dip
   >= 5 dB, jump >= 100 c within 20 ms), 65 c was enough (#7).
4. **Level and flux are weak cues** inside legato. Of the 12 cuts, only 3 have a dip >= 6 dB, and flux
   barely rises (0.11-0.21 against a median of about 0.11). A pure onset detector would not find these
   notes; the evidence is the shape of the pitch curve.

### A.4 Why the automatic detector missed them (code-level)

* `self_check` (segment.rs:777) only looks at the **core ranges** ("over the core ranges only"), and it
  fits lines (so drift is absorbed). The missed notes were all outside the cores, so they were invisible
  to it. That is why **0 `MaybeTwoNotes` flags** were raised and every confidence stayed at 1.0.
* The DP prices cores as lines with |slope| <= 150 c/s (`max_drift`). A passing tone sung as a
  slow-down at 500-1000 c/s is not a core, so it becomes transition frames (cost 300 each while moving),
  which are cheaper than a new note (10 000).
* `edge_glide_core` (100 ms) with `fall_drop_db` 3 dB turns a short, quieter landing note at a run edge
  into a "fall bottom" (case #2: 95 ms core, 8 dB quieter, a real C3).

### A.5 Would a different sensitivity have reproduced the cuts?

**No.** Sensitivity buys recall only by adding many spurious cuts:

| sensitivity | notes | expert-added boundaries found (of 13) | boundaries not in the expert set | boundary F | notes matched |
|---|---|---|---|---|---|
| 0.5 | 36 | 0 | – | 0.82 | 46 % |
| **1 (default)** | 42 | **0** | 3 | **0.88** | 65 % |
| 1.5 | 49 | 1 | 9 | 0.85 | 62 % |
| 2 | 59 | 5 | 16 | 0.83 | 63 % |
| 3 / 4 | 61 | 6 | 17 | 0.83 | 63 % |
| 6 | 76 | 11 | 27 | 0.82 | 67 % |
| 8 | 84 | 12 | 32 | 0.79 | 65 % |

Sensitivity scales `note_cost` uniformly. The missed notes need a different *kind* of evidence (holds
and slow-downs inside transitions), not a cheaper note everywhere.

### A.6 Metronome clicks detected as notes

Notes 18 / 43 / 44 of track 1 (8.593, 19.098, 19.598 s; 50-60 ms; MIDI 88.0-88.1) are a **pure
1318-1320 Hz tone**: aperiodicity 0.07-0.16, hf_ratio 0, level -38 / -51 / -51 dB. The engine's
*normal* metronome click is 1320 Hz (`metronome.rs:81`), and the last two are 0.5 s apart, after
the singing ends. These are **click bleed from the headphones into the mic**. The expert left them in
and they got "tuned" to 88 (harmless, at most 11 c). The detector should drop them: < 80 ms, more than
2 octaves above the take's median (E3), and a near-sinusoidal spectrum.

### A.7 Prototype splitter learned from the session (in-sample, so a hypothesis to test)

Two rules applied inside each automatic note:

* **R-hold:** find slow stretches with |Δp| over 30 ms < **1000 c/s** lasting >= **25 ms**. Merge
  stretches whose medians are < **60 c** apart. Drop stretches more than 10 dB below the loudest one.
  Cut where the curve crosses halfway between consecutive stretches. Keep the cut only if both resulting
  notes are >= **80 ms**.
* **R-rearticulation:** a pitch jump >= **100 c within 20 ms** together with a level dip >= **5 dB**
  (against the louder neighbours ±20-120 ms). Both sides must be >= 80 ms.

On this take the union finds **9 of the 13 expert-added boundaries with 2 false cuts**. The false cuts
are the 2.72 onset dip and the 16.385 upper-neighbour bump. Default sensitivity finds 0 of 13 with 3
false cuts, and sensitivity 6 finds 11 of 13 with 27 false cuts. The 4 misses are 11.613 (the end of the
turn), 12.867 (B2 hold of only 40 ms), 15.823 and 16.873 (cut-placement offsets above 30 ms). The
parameters were tuned on these same 12 cuts, so the numbers are optimistic. They need validation on
more labeled takes before any default changes.

---------------------------------------------------------------------------------------------------

## B. Tuning evaluation (track 3 "afinada completa")

### B.1 Summary

* **All 52 real notes (and the 3 clicks) are retuned to integer semitones**: full snapping (center
  100 %), with no partial correction.
* **No other parameters were touched on any note.** Vibrato was kept: about 25 c deep at 6.5 Hz on
  6.73-7.70, 6.3 Hz on 13.62-14.25 and 4.7 Hz on 17.06-17.80. Drift was kept: -111 c/s on 9.34-9.68,
  -109 c/s on 16.25-16.87, +150 c/s on 14.81-15.17. Formant, gain, timing and sibilants were not
  touched.
* **No note was left untouched**, not even short or gliding ones. The notes that stayed close are
  simply the ones already sung within a few cents.
* **The size of the correction does not depend on note length:** mean |shift| is 50 c for notes
  < 100 ms, 41 c for 100-150 ms, 41 c for 150-250 ms and 42 c for > 250 ms. Overall, |shift| has a
  mean of 42 c, a median of 38 c and a max of 120 c, and 19 notes moved by more than 50 c.
* **The singer is sharp on average:** sung - target is **+34 c** (median +33 c, core-weighted +32 c),
  and 43 of 52 notes are sharp. The offset grows over the take, phrase by phrase: +21 c (0.99-4.12),
  +8 c (4.83-7.70), +49 c (8.72-10.42), +46 c (11.11-14.25), +63 c (14.81-17.80).

### B.2 Key / scale

The expert's target pitch classes, weighted by core seconds: **E 3.52**, F 1.74, D 1.57, C 1.02,
B 0.78, G 0.57, Eb 0.15 (one note), F# 0.06 (one note). That is **E Phrygian**: the C-major pitch set
with E as the tonal center, F natural as the colour note, and one chromatic Eb and one F#. Flamenco /
Andalusian colour: the E-F-G-F-E and D-C-B figures.

* **Cadenza's `detected_key` / Krumhansl on the sung centers gives E minor** (r 0.657; E major 0.533).
  Even run on the *expert's own targets*, Krumhansl still says E minor (0.621 against C major 0.479):
  the tonic-weighted profiles cannot choose F vs F#. And because the singer's F is sung 50-80 c sharp
  (53.5-53.8), the rounded sung pitch classes are full of F#.
* **A blind joint fit works far better:** fit a diatonic pitch set (12 roots) and a global reference
  offset (±50 c) to the sung centers, by core-weighted squared distance to the nearest tone, with no
  expert data. The best fit is the **C-major set (E Phrygian / A minor) at +35 c**
  (rms 36 c). Snapping each `center - 0.35` to the nearest tone of that set **matches the expert on
  49 / 52 notes**. Caveat: the next-best fit is the same tone grid seen one semitone over (C# major at
  -50 c, rms 39 c), so a prior on small offsets is needed.

### B.3 Agreement with candidate automatic strategies

| strategy | agrees with the expert |
|---|---|
| Cadenza *Correct pitch* defaults (chromatic, nearest semitone, 100 %) | **33 / 52** |
| *Correct pitch* with *Detected* key (E minor, nearest in key) | 40 / 52 |
| region median / level-weighted mean / second-half median, rounded | 31 / 32 / 30 of 52 (the core center is the better estimator) |
| E Phrygian set, no offset | 45 / 52 |
| **E Phrygian set after the blind +35 c offset** | **49 / 52** |

The 19 notes where the expert did **not** pick the nearest semitone (all but one moved *down*):

* **F sung as F#**, F3 chosen (9 notes): 5.672, 9.128, 9.683, 11.107, 15.500, 15.743 (54.19 -> 53,
  -119 c), plus 8.718, 12.491 and 11.613 (these were nearest anyway).
* **Eb sung for D**, D3 chosen: 1.373, 5.178, 13.198, 15.993, 16.253 (-57 to -82 c).
* **F sung for E**, E3 chosen: 1.113, 13.318, 15.170.
* **C# sung for C**, C3 chosen: 12.868, 12.948 (49.20 -> 48, -120 c, below nearest-in-key D),
  16.872 (-89 c).
* **Ab sung for G**, G3 chosen: 11.280 (55.80 -> 55, -80 c).
* **Upward** (1 note): 14.808 51.41 -> E3 (+59 c). Its core is only 70 ms inside a 280 ms octave scoop,
  and the curve is still rising through 52.1 at the note end. The expert tuned to where the scoop lands,
  not to the brief core.

These are **melodic decisions** (mode + singer offset), not intonation rounding.

What the E-minor macro would get wrong:

* every F becomes F#: 9 notes, including #34 (53.01 -> F#, +99 c) and #31 (53.10 -> F#), which the
  singer had sung right;
* 3.787 Eb -> E;
* 12.948 -> D.

The chromatic default would push 18 notes a semitone too high.

Exceptions to the best rule (3 notes), which show a judgment layer above the scale:

* **3.787 51.23 -> Eb3.** A clearly sung chromatic note (145 ms core, 23 c from Eb) between E and C.
  The expert kept the chromatic note instead of applying the take offset, which would give D.
* **4.827 47.69 -> C3.** A short flat plateau before a C on the same syllable. It takes the pitch of the
  following note (see split #3).
* **6.162 53.55 -> F#3.** Core only 55 ms, a scoop from 52.0 to 53.6 that falls back to E. The expert
  heard F#.

### B.4 The full per-note table

The table is in the analysis scripts. Bold marks where a strategy disagrees with the expert.
Columns: note, region, duration / core, sung center, expert target, shift, nearest semitone, nearest in
E minor, and nearest in the C set after -35 c.

---------------------------------------------------------------------------------------------------

## C. Expert rules (quantitative, implementable) and how an AI tuning engineer should use them

### C.1 Segmentation rules

1. **Look for notes inside transitions, not only inside cores.** Run the split check over
   onset..offset, after the DP. In this session, 10 of 10 missed notes were outside the automatic
   cores.
2. **Slow-down = note (R-hold).** Inside a transition, a stretch with pitch speed < **1000 c/s** (30 ms
   window) lasting >= **25-40 ms**, whose median is >= **60 c** from the neighbouring stretches, is its
   own note when the result is >= **80 ms**. In practice this means passing tones in stepwise runs
   (C-D-E, D-C-B, G-F-E) and melisma steps.
3. **Re-articulation = boundary (R-rearticulation).** A pitch jump >= **100 c within 20 ms** together
   with a level dip >= **5 dB** is a boundary even inside legato. Both sides must be >= **80 ms**.
4. **Do not split monotonic scoops or falls.** A segment that keeps moving (no hold) is an attack or
   release, however large the step (the 2.0 st, 1.6 st, 2.4 st and 9.6 st cases were all left whole).
   Do not split quiet pickups < **60 ms** that are more than 6 dB below the note.
5. **A landing note is not a fall bottom when it holds.** A post-fall segment with a hold of >= **60 ms**
   at a pitch >= 150 c below the note is a note, even when it is 8 dB quieter. Gate `edge_glide_core` on
   "no hold" instead of only on duration and level.
6. **Split two-level notes for tuning.** If a note's region has two plateaus >= 100 ms each,
   >= **50 c** apart, then splitting them (same target) lets each half be centered. Alternatively,
   correct it with drift. The expert chose the split.
7. **Ornament policy: ambiguous.** The expert split a +65 c sharp bump that followed a re-articulation
   (15.76) and pulled it to the note's pitch. They left a +170 c upper-neighbour bump without a level
   cue (16.30) inside the note. Rule: split ornaments only when a re-articulation cue (rule 3) is
   present. Otherwise leave them as part of the note.
8. **Drop non-vocal blips:** < 80 ms, more than 24 st from the take median, near-pure tone (hf_ratio 0,
   aperiodicity < 0.2). In this take these are metronome bleed.

### C.2 Tuning rules

1. **Snap the center 100 % to an integer semitone.** Never partial, at every note length (this expert,
   this style).
2. **Do not touch vibrato or drift by default:** natural ~25 c / 4.7-6.5 Hz vibrato, and drift up to
   about 110-150 c/s, were all kept. No formant, gain or timing edits.
3. **Pick the target from a fitted pitch set plus a reference offset, not from nearest-semitone
   rounding.** Fit (diatonic set, root, mode) jointly with a global offset (here +35 c, |offset| <= 50 c,
   prefer small offsets) by core-weighted squared distance. Better still, fit the offset per phrase:
   phrases separated by >= 300 ms gaps had offsets of +8 to +63 c. Allow modes: the target set here is
   Phrygian, which Krumhansl major/minor gets wrong.
4. **Keep clear chromatic notes.** A note whose core is >= 120 ms and within 25 c of a semitone outside
   the set stays on that semitone (3.787 Eb).
5. **Short notes are tuned by the note they join.** A plateau of <= 120 ms on the same syllable as the
   next note, < 60 c from it, takes the next note's target (4.827 -> C3). An overshoot after a rising
   run takes the run's goal note (12.948 C# -> C).
6. **For a short core inside a big scoop, tune to where the scoop lands.** When the core is < 80 ms and
   the region's last 30 % is above the core, use the landing pitch (14.808 -> E3).

### C.3 How the AI tuning engineer (NE-7) should work

**Deterministic code, exposed as tools.** The LLM should not reason over 5 ms frames:

* `analyze_vocal(clip)`: the take summary, made of the median, range, voiced %, blips detected (rule
  C.1-8) and the **fitted pitch set + offset per phrase** with alternatives and their rms (C.2-3).
* `propose_splits(clip)`: candidates from rules C.1-2/3/5/6. Each candidate carries its evidence
  (time, cents on each side, hold length, level dip, slope, kind: passing_tone | rearticulation |
  landing | two_level | ornament) and a score. Also re-check the automatic notes and return
  `MaybeTwoNotes`-like flags computed over the full region.
* `propose_targets(clip, scale, offset)`: a target per note, with a reason code (nearest_in_set,
  chromatic_kept, joins_next, scoop_landing, ambiguous) and the runner-up target.
* The existing edit tools: split, merge, `UpdateAudioNotes` patches. The whole pass is one undo
  group.

**The LLM's job:**

* choose the musical frame: style, mode, whether chromatic notes are intended. It can ask the user
  or use the project style;
* resolve the *ambiguous* items, such as ornaments, F vs F#, and which run notes are intended;
* explain every change (before -> after cents per note);
* respect user preferences, such as "keep it natural" (no drift or vibrato edits by default).

Rules stay in code, so they can be tested and evaluated. The LLM only makes the choices those rules
flag as ambiguous.

**Verify:** render, then re-analyze (`resynth::verify`) and report residual cents.

### C.4 What this session gives us as training / eval data

* **A labeled real-recording case is ready:** `crates/notes/tests/data/expert_take.json` (eval `labels`
  format, 52 notes). Each note carries onset/offset/cores from the expert's cuts, `pitch` = sung center
  and `nominal` = the expert's target. The audio stays out of the repository:
  `CADENZA_NOTES_EXPERT_TAKE=<wav> cargo test -p cadenza-notes --test expert`, or put the
  labels next to the audio as `expert_take.json` in `$CADENZA_NOTES_CORPUS` for
  `cadenza notes eval` (COnPOff 70.1 % before NE-7a). **Caveat:** the expert's boundary *placements* are the segmenter's own
  (re-detection after splits), so this case measures *which* notes exist, not boundary placement by
  ear. Score at ±50 ms.
* **A tuning target set:** 52 (center -> expert target) pairs, with 19 non-nearest decisions. The
  natural metric is the share of notes whose proposed target equals the expert's. Baselines: 33/52
  (chromatic), 40/52 (detected key), 49/52 (set + offset fit).
* **Negative examples** for over-splitting: the 34 notes the expert left whole, including the scoops
  and ornaments listed in A.3.
* **Limits:** one singer, one 20 s take, one style (E Phrygian), and no edit history. The thresholds in
  C.1/C.2 are fitted in-sample. Add 5-10 more expert sessions before changing defaults. For each one,
  record the order of edits (the undo labels) so we learn the workflow as well as the end state.

---------------------------------------------------------------------------------------------------

---------------------------------------------------------------------------------------------------

## D. Implemented in NE-7a, and the numbers after

**Segmentation** (`cadenza_notes::refine`, on by default in `SegmentOptions::refine`; one more
partition with the new boundaries as forced splits, exactly how the expert's splits work):

* holds are found on the tracked curve (frames under 1000 c/s over 30 ms, or under it on the
  curve without vibrato where there is vibrato; the vibrato-removed curve alone turns a D-C-B
  run into one straight line), never across an existing boundary, split where a slow stretch
  spans two pitches, joined when closer than max(60 c, 2.5 x vibrato depth) (60 c across an
  existing boundary), dropped 15 dB below the loudest hold within 300 ms;
* a boundary is added between two holds of different pitch with no boundary between them
  (at the halfway crossing), and at a re-articulation (jump >= 100 c in 20 ms with a level
  dip >= 5 dB); every new note >= 80 ms, holds of at least 35 ms (edge holds 60 ms, so a fall
  slowing down at its bottom is not a note); a boundary inside a hold is misplaced and does
  not block a cut;
* a landing note after a fall that holds >= 60 ms, 150 c or more below, is kept (`landing_hold`);
* blips < 120 ms, > 24 semitones from the take's median, nearly pure tones are dropped
  (the metronome bleed);
* `MaybeTwoNotes` also flags a note holding another pitch outside its core.

Result on this take (`tests/expert.rs`, run with `CADENZA_NOTES_EXPERT_TAKE=<wav>`): COnPOff
70.1 % -> **83.7 %**, mixed regions 20 % -> 11.8 %, 0 false cuts, the 3 clicks gone. The
synthetic corpus is unchanged or better at every rate (16 / 22.05 / 44.1 / 48 kHz). Still
missed: the fast melisma turn (11.47-11.70), the 55 c two-level note (4.95), the run peak
pulled back to C (12.95), one placement (15.82): they are tuning decisions, left to the
flags and to the AI engineer (NE-7b) rather than rules fitted to one take.

**Targets** (`cadenza_notes::tuning`): `fit_scale` fits every diatonic mode and the harmonic
minor jointly with a reference offset (-50..50 c, prior 100 c^2 at 50 c, at least 6 notes;
the mode is the set's tone the cores dwell on, the last note counting double); `targets` adds
the chromatic (core >= 120 ms within 25 c of a tone outside the scale), joins-next (core
< 120 ms legato into a note < 60 c away) and scoop-landing rules, each target with its
reason, runner-up and an ambiguity flag. On the expert's labels (`tests/tuning.rs`): **E
Phrygian, +33 c, 51/52 targets equal to the expert's** (nearest semitone: 33/52; the one
difference is the 6.16 F# the expert heard by ear). The note editor's key is now
"E phrygian +33 c (detected)" by default and Correct Pitch uses these targets; `cadenza notes
tune --key auto` prints the fit and the reason per note.
