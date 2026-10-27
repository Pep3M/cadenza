# cadenza-analysis — contract

Moved from CLAUDE.md (kept in sync with the code: update it when the contract changes).
Scope: (phase 18b)

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
