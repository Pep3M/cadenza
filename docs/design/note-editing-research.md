# Note-based audio editing: research notes (Melodyne-style)

Status: research (2026-10-24). The build plan is `docs/design/note-editing-plan.md`.

Goal: on audio tracks, detect the notes a recording contains, retune / retime / reshape
each note like Melodyne, and turn the notes into MIDI clips for the piano roll. The same
engine later powers hum-to-MIDI, audio-to-MIDI, automatic tuning and an AI that reviews
and perfects intonation.

Hard requirement from the user's experience with Melodyne: **note boundaries must be
right**. When a "blob" spans parts of two real notes, its pitch center sits between them;
snapping the blob to the scale then leaves the actual pitch curve off-center, i.e. out of
tune, and the only fix is manual re-splitting. Section 5 is about making that not happen.

## 1. What the shipped binary gave us (and what it did not)

`~/Downloads/Melodyne.exe` (SHA-256 `b39e129b…a3ba74`, PE32+ x86-64, built 2020-12-07,
`R:\hudson\Melodyne_1_Windows\MelodyneApp\x64\MDRelease\Melodyne.pdb`) is only the
**standalone launcher** of Melodyne 5.1.1.003:

- Imports are plain Win32/CRT (kernel32, user32, wininet, wintrust, version...): no DSP,
  no FFT library, no audio API.
- It loads the engine from `%sCore-%s.dll` -> `MelodyneCore-5.1.1.003.dll`, which is
  **not** in the download.
- It embeds PACE/iLok anti-piracy (`libpacefusion 5.3.1`, `libpaceedenexperience.dll`).

The file holds no analysis or resynthesis code. Reaching it would mean obtaining the core
DLL and defeating PACE: anti-circumvention (illegal in the EU and the US), against the
EULA, and against Cadenza's clean-room rule. Everything below comes from Celemony's public
disclosures (patent, manual), other public patents, and open literature.

## 2. Legal constraints

| Patent | Holder | What | Status |
|---|---|---|---|
| US 8,022,286 B2 (priority DE 10 2008 013 172, 2008-03-07) | Celemony | "DNA": polyphonic note detection + per-note separation and editing | **active, expires 2029-06-15** |
| US 5,973,252 | Antares (Auto-Tune) | autocorrelation pitch detection + intonation correction | **expired** |

- Reading patents is fine; implementing the **claimed combination** of US 8,022,286 is not,
  where it is in force, until it expires. The German priority patent's term (likely ~2028)
  and other Celemony patents were not verified (Justia/Google listing blocked our fetch).
- Monophonic pitch editing (pitch tracking, PSOLA, vocoders, envelope preservation) is old
  prior art; real-time intonation correction is free since the Auto-Tune patent expired.
- Polyphonic **detection -> MIDI** with open methods (Basic Pitch, NMF, harmonic summation)
  is prior art too. Polyphonic **per-note editing** is the patent-sensitive part: ship it
  only after a legal review or after 2029-06-15. This is not legal advice.

## 3. Melodyne's user model (manual, Melodyne 5)

Unit of editing: the **note object** ("blob").

Algorithms, picked automatically per file (manual "Audio characteristics and algorithms"):

| Algorithm | Detects | Used for |
|---|---|---|
| Melodic | notes at pitches + **sibilants** (s, ch, k, t, breaths) that stay unaltered by pitch/time edits | voice, flute, sax, bass; needs dry recordings |
| Percussive | events, all at one pitch | drums, noise, atmospheres |
| Percussive Pitched (new in 5) | events with a pitch | 808 kick, tabla, berimbau |
| Polyphonic Sustain / Decay | stacked notes (DNA); Decay when the attack differs from the sustain | organ, legato strings / piano, guitar, pizzicato |
| Universal (manual only) | events at one pitch, no attack-speed tool | mixes, mixed material, time/pitch of everything |

DNA separates by pitch, not by instrument: the same note on two instruments is one blob.
Melodyne 5 adds a "Robust Pitch Curve" option (slightly smoothed curve, for reverberant
signals).

Per-note parameters (tools): pitch center (scale snap or cents), **pitch modulation** (fast,
intentional: vibrato, trills), **pitch drift** (slow, unintentional), pitch transition (slide
into the next note), formant, amplitude / mute, fade, sibilant balance, time (move, stretch,
**attack speed**), **note separation** (double-click adds / removes a split). Modulation and
drift are shown as % : **100 % = as recorded, 0 % = straight line, −100 % = inverted** about
the same axis. When a split changes, Melodyne re-searches the pitch curve in the blob and
recomputes the pitch center (the blob visibly "jumps": purely the new center).

Macros: Correct Pitch (% pull of centers to the scale + % drift reduction), Quantize Time,
Leveling. Also scale/key detection, tempo detection, "Save as MIDI".

## 4. Pitch tracking (frame level)

| Option | Licence | Notes |
|---|---|---|
| YIN / **pYIN** (Mauch & Dixon 2014) | method public | YIN with a threshold distribution -> several f0 candidates per frame + HMM/Viterbi for voicing and continuity; second stage = HMM note segmentation (Tony) |
| **SwiftF0** (2025) | MIT, ONNX | 16 kHz, 16 ms hop, 46.9 Hz–2.09 kHz, outputs f0 + confidence + loudness, ~14k params per the README (the paper's version: 95.8k), ~180x realtime on one core, beats CREPE by >12 pts at 10 dB SNR |
| CREPE | MIT | 1-cent bins, heavy (millions of params) |
| RMVPE (2023) | check | vocal pitch straight from a mix (no separation), robust at low SNR; useful for vocals with bleed |
| FCPE (2024-25) | check | fast context-based estimator |
| Auto-Tune style autocorrelation | free (expired) | the real-time detector for a live tuner |

Plan: own pYIN in pure Rust as the always-available baseline (deterministic, explainable),
SwiftF0 via `tract` (pure-Rust ONNX, MIT/Apache) as the robust option, fused per frame
(candidate lists from both, one Viterbi). Need per frame: f0 candidates + probabilities,
voicing probability, aperiodicity, RMS/loudness, spectral flux.

Octave errors are the classic failure: half/double f0. Viterbi over candidates with an
octave-jump penalty plus a harmonic-consistency check (energy at f0/2 partials) fixes most.

## 5. Note segmentation — the critical part

### Why Melodyne-style splits fail

A split placed inside a glide or a scoop, or a missing split between two notes joined by
legato, gives a segment whose pitch distribution is **bimodal**. Any center taken over the
whole segment (mean, or even median) lands between the modes. Retuning moves that wrong
center to the scale, so the real note stays off. The center must come from the **stable
core of one real note**, and the segmentation must be checked for homogeneity.

### Prior art

- **pYIN / Tony note HMM** (after Ryynänen & Klapuri): per pitch a small left-to-right model
  (attack -> stable -> silent), transitions between pitches penalized by interval; Viterbi
  over the pYIN candidates gives notes. Tony (Mauch et al. 2015) measured how much manual
  correction it still needs.
- **SiPTH** (Molina et al., IEEE/ACM TASLP 2015): regions of stable chroma; voiced/unvoiced by
  aperiodicity + power; after an onset a **dynamic average** of the pitch is tracked and a
  **hysteresis** on the deviation from it decides the next note change — designed for
  untrained singers whose pitch is unstable inside a note.
- Learned note-level singing transcription: VOCANO (hierarchical boundary classification),
  phoneme-informed models (2023), **ROSVOT** (ACL 2024, multi-scale boundaries + attention
  pitch decoder, robust to noise). Metric: COnPOff / COnP F-measure (mir_eval: onset ±50 ms,
  pitch ±50 cents, offset ±20 % of length). Dataset MIR-ST500 (500 Chinese pop songs).
- Optimal changepoint detection (PELT, Killick et al. 2012) / dynamic programming:
  exact minimum of "fit cost per segment + penalty per boundary".

### Design for Cadenza (combine evidence, optimize globally, verify each note)

1. **Evidence per frame** (≈5 ms): f0 candidates, voicing, aperiodicity (consonants),
   loudness, spectral-flux onset strength, energy dips, and an optional learned boundary
   probability (later).
2. **Global segmentation by dynamic programming** over the pitch curve: each segment is
   modeled as *transition-in + stable core + transition-out*; cost of a segment = robust
   (Huber) deviation of the core from its own center + a vibrato term (a sinusoid 3–9 Hz
   around the center costs nothing) + boundary costs that are **cheap where the evidence
   says "boundary"** (onset, consonant, voicing change, a pitch move that settles) and
   expensive elsewhere. Exact DP (PELT-style pruning) — no greedy split decisions.
3. **What is not a note**: a scoop/fall/glide is a transition belonging to the note it
   reaches/leaves (shorter than ~120 ms and with no plateau). A plateau of ≥ ~60–80 ms at a
   distinct pitch is a note even if short (grace notes, melismas). Vibrato excursions never
   split (detected as periodic motion, extent ≲ 2 st).
4. **Repeated notes at one pitch** split on onset/energy-dip/consonant evidence only.
5. **Boundary placement**: between two notes the cut goes where the glide crosses halfway
   between the two centers (pitch-wise), or at the onset when there is one; the glide itself
   is a shared **transition zone** excluded from both centers.
6. **Center = stable core estimate**: energy- and voicing-weighted mode of the core's pitch
   (kernel density in cents), averaged over whole vibrato cycles; not the plain mean.
7. **Self-check per note** (confidence 0..1): bimodality test of the core (1 vs 2 Gaussians,
   BIC), spread, distance of the first/last 25 % from the center, octave-jump flags. Low
   confidence => the analysis proposes the split (or merge) itself, and UI/AI flag it.
8. **Edits never depend on perfect cuts**: the correction is applied as one continuous
   **pitch-offset curve** over time (each core shifted by its own amount, transitions
   interpolated smoothly), and every edit recomputes the affected centers. A slightly wrong
   cut moves a transition, not a whole note off-pitch.
9. **Closed loop**: after rendering, re-analyze the output and measure each note's residual
   error in cents; anything above tolerance (e.g. ±5 cents) is reported/fixed again.

Ground truth to tune and guard all this: a synthetic corpus (rendered with known notes,
glides, scoops, vibrato, legato, repeated notes, breaths, octave jumps, noise) plus a
hand-labeled set of real recordings. Our own metric besides COnPOff: **center error in
cents** after correction (what the user actually hears).

## 6. Pitch curve decomposition and correction math

`p(t) = center + drift(t) + mod(t)` per note core: `drift` = low-pass (~2–3 Hz) of `p − center`,
`mod` = remainder (vibrato 4–8 Hz, trills). Melodyne's tools are percentages on these terms:

`p'(t) = target + a_drift · drift(t) + a_mod · mod(t)`, `target = center + k·(scale_pitch − center)`

with `a_*` in −1..2 (100 % = unchanged). Transitions keep their shape, rescaled to join the
new neighbouring targets. Rendering needs only the **ratio curve** `r(t) = 2^((p'−p)/12)`.

## 7. Resynthesis (pitch, formant, time, sibilants)

| Method | Pros | Cons |
|---|---|---|
| TD-PSOLA (pitch marks, 2-period grains, OLA) | formants preserved by nature, cheap, excellent on mono voice for moderate shifts | needs good epoch marks; artifacts on big shifts / breathy voices |
| Phase vocoder + spectral envelope (true envelope / cepstral) | robust, formant shift = envelope warp | phasiness unless phase-locked |
| **WORLD** (Morise; modified-BSD): f0 + CheapTrick envelope + D4C aperiodicity -> synthesis | pitch, timbre, noise in separate streams: retune, formant and sibilant balance fall out naturally | analysis/resynthesis of the whole signal (slightly "vocoded" on some material) |
| Signalsmith Stretch (MIT, C++) | `setTransposeFactor` per block (time-varying OK, align with `outputLatency()`), `setFormantFactor` + `setFormantBase(f0)`, `setFreqMap` | Cadenza only has it behind the optional `stretch` feature (C++/libclang) |

Choice for the plan: a pure-Rust, on-by-default engine. Primary: PSOLA on voiced cores with
the ratio curve, unvoiced/sibilant segments passed through (their gain = sibilant balance);
formant control by envelope warping on a vocoder path. The rendered result is verified by
re-analysis (section 5, step 9). Time edits = piecewise time map with the attack region kept
at ratio 1 (attack speed scales just that region).

## 8. Polyphonic material

DNA (patent, read only): 2048-pt STFT, ≥4x overlap, instantaneous frequency from phase
advance, per-bin tonality, a cent-axis energy landscape plus harmonic summation, iterative
ridge tracing (stop on > 50 cent jumps) with 50 % energy subtraction until 10 % remains,
transient "event objects" attached to the note whose energy rises after them, and per-bin
sharing of energy between notes from a harmonic model (Hann^4 weights, ~2 semitones) with a
residual. **Do not reproduce this combination.**

Open path: **Basic Pitch** (Spotify, Apache-2.0 code + weights, ONNX shipped). Harmonic CQT
(3 bins/semitone, 7 harmonics + 1 subharmonic), ~11 ms hop, 22.05 kHz mono input, 16,782
parameters; outputs onsets + note activations (1 bin/semitone) and a multipitch contour
(3 bins/semitone, used for bends). Notes: onset peaks ≥ 0.5, track forward while activation
stays above τ (tolerating 11 low frames), then fill from remaining activation peaks, drop notes
< ~120 ms. Instrument-agnostic (guitar, voice, ensembles); on piano a dedicated model
(Onsets and Frames, Apache-2.0) is far better (95 % vs 71 % note F1). Detection-only use is
the safe, high-value feature (audio-to-MIDI of chords).

## 9. Audio -> MIDI

- Notes from section 5 (mono) or section 8 (poly): start/end in ticks through the `TempoMap`.
- Pitch = rounded center; cents + drift + modulation -> `pitch_bend` lane (mono only; poly
  bends need MPE, not supported), bend range ±2 st (Cadenza instruments).
- Velocity = note peak short-term loudness mapped over the clip's range.
- Options: quantize, min length, merge repeats, key snap, "notes only / with bends".
- Hum-to-MIDI = the same pipeline on a recording with stronger smoothing, octave folding to
  the target instrument's range, quantization to the project grid and key.
- Output = one `Batch`: `AddClip` (MIDI) + `SetControllerLane` — the path MIDI import uses.

## 10. Automatic tuning and the AI

- Key/scale: the pitch-class histogram of note centers weighted by duration, Krumhansl
  profiles (already used by `library::import`), or the project's style/key.
- Target per note: nearest scale note, or the **MIDI reference** when there is one (a
  melody clip / the arrangement's chords: notes outside the chord tones get extra review).
- "Tuning report" for the AI: per note center error (cents), drift/vibrato stats, confidence,
  suspicious segmentations; the AI applies Correct Pitch–style moves through commands,
  re-analyzes, and reports before -> after (same loop as the sound-engineer pass).
- Real-time tuner device (Auto-Tune-like, retune speed + humanize) is free prior art and
  can reuse the detector; separate from offline note editing.

## 11. Cadenza building blocks that already exist

| Need | Already in the code |
|---|---|
| decode + resample | `audio-io` (`AudioCache`, symphonia, rubato) |
| FFT | `realfft` (workspace), `analysis::spectrum` |
| f0 (rough) | `dsp::util::pitch::detect_pitch` (YIN, single estimate for sample mapping) |
| pitch/time (optional) | `audio-io::stretch` (Signalsmith; `stretch` feature, off by default) |
| processed copies swapped in undoably | audio editor: render to `Audio/`, `UpdateAudioClip` |
| MIDI clips, bend lanes, piano roll | `ClipContent::Midi`, `SetControllerLane` |
| key inference | Krumhansl in `cadenza-library::import` |
| engineer-style AI loop | `SessionAgent::run_engineer_pass`, one undo step |

## 12. Open questions

- Patent terms per jurisdiction; other Celemony patents.
- RMVPE / FCPE licences; whether CREPE adds anything over SwiftF0.
- PSOLA vs WORLD-style vocoder: decided by listening tests + the closed-loop metric (NE-4).
- Realtime playback of edits vs offline render (first ship render).
- MPE in the piano roll for polyphonic bends.

## Sources

- US 8,022,286 B2, Google Patents: https://patents.google.com/patent/US8022286
- US 5,973,252 (Auto-Tune, expired): https://patents.google.com/patent/US5973252
- Melodyne 5 manual: algorithms https://helpcenter.celemony.com/M5/doc/melodyneStudio5/en/M5tour_AudioAlgorithms ,
  modulation/drift https://helpcenter.celemony.com/M5/doc/melodyneStudio5/en/M5tour_ToolModulationDrift_2
- Sound On Sound, Melodyne 5 review: https://www.soundonsound.com/reviews/celemony-melodyne-5
- Tony / pYIN: https://code.soundsoftware.ac.uk/projects/tony
- SiPTH (Molina et al. 2015): https://portaldelainvestigacion.uma.es/documentos/6624691fdc30f904d76a10a8
- ROSVOT (ACL 2024): https://arxiv.org/abs/2405.09940
- SwiftF0: https://arxiv.org/abs/2508.18440 , https://github.com/lars76/swift-f0
- RMVPE: https://arxiv.org/abs/2306.15412 ; FCPE: https://arxiv.org/html/2509.15140v1
- Basic Pitch: https://arxiv.org/abs/2203.09893 , https://github.com/spotify/basic-pitch
- WORLD vocoder: https://github.com/mmorise/World
- Signalsmith Stretch: https://github.com/Signalsmith-Audio/signalsmith-stretch
