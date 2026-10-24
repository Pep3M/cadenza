# Poly Synth preset library

The `poly_synth` factory library: 93 presets in 12 categories that recreate the synth sounds of
popular and trending music (2022-2026 plus the classics people still look for). Presets live in
`crates/dsp/src/presets/poly_synth.rs`; this document records how they were made, how to add more,
and what the synth itself is missing (the study doubles as the synth's roadmap).

## Method

1. **Research.** Two passes over published sound-design breakdowns: genre signature sounds (trap,
   drill, phonk, amapiano/afro, reggaeton, current pop, synthwave/80s, EDM, house, melodic techno,
   bass music, lo-fi/ambient) and iconic sounds of specific hits with public recreations. About 155
   recipes, each with oscillators, unison, filter, envelopes, glide, modulation and effects, marked as
   sourced or derived. The richest sources are listed at the end.
2. **Translation to this synth.** Recipes give percentages of other synths' knobs; they were mapped
   to absolute values (cutoff % on a log 20 Hz-20 kHz sweep, envelope amount % on our ±60 semitones,
   pulse width on the PWM wavetable's position) and adapted where a feature is missing: a short filter
   envelope transient instead of a pitch envelope, the Formant table + LFO instead of a formant filter,
   PWM = an LFO on the PWM table position, an octave/twelfth on oscillator 2 instead of a third
   oscillator, +19 instead of +31 semitones.
3. **Naming.** Presets carry evocative generic names ("Night Drive Lead", "Fjord Hook Lead") and
   genre descriptions; artist, song, brand and product names stay out of shipped text.
4. **Listening tests in code** (`crates/engine/tests/preset_library.rs`): every preset plays a
   phrase typical of its category (bass line, overlapping 808 slides, lead melody, pluck melody,
   sixteenth arp, chord stabs, chords, sustained pads, a held FX note) and must
   - land in **-15..-10 LUFS momentary** (the loudest 400 ms: fair to short stabs and long pads),
     unless its transients reach the peak ceiling first (true peak >= -2.5 dBTP: short stabs and deep
     subs are as loud as their peaks allow),
   - stay under **-1 dBTP**,
   - have modulators that change the sound by more than -26 dB (a forgotten depth is inaudible),
   - be categorized, uniquely named, described in 60-400 characters, and accepted by the model.

   Levels were calibrated to -12 LUFS momentary with a -1.5 dBTP ceiling. Before the library the five
   presets spread over 20 dB.

## Listening

`CADENZA_PRESET_AUDITION_DIR=dir cargo test -p cadenza-engine --release --test preset_library --
--ignored --nocapture` writes one WAV per preset (`NN Category - Name.wav`) and prints a table of
momentary/integrated loudness, true peak, crest factor and five band energies. Use it after editing
a preset: adjust its `gain` until the level test passes.

## Format and behaviour

- `FactoryPreset { name, category, description, params, zones, modulators }`; categories are
  `cadenza_dsp::PRESET_CATEGORIES` (Bass, 808, Lead, Pluck, Keys, Bell, Pad, Choir, Strings, Brass,
  Arp, FX). Other built-ins keep an empty category (ungrouped).
- `PresetModulator` mirrors the model's `Modulator` (LFO free / synced / note-retriggered, or an
  envelope follower of the device's own signal) with targets `(param, depth)`;
  `cadenza_engine::factory::{preset_modulators, apply_preset}` turn a preset into a device.
- **A built-in preset is the whole patch**: loading one replaces the device's explicit params (others
  return to their defaults) *and* its modulators, in one undo step. User presets save and restore
  modulators too.
- MCP: `create_track` / `insert_device` take `preset` (factory name, case-insensitive, close names
  suggested on a typo; `params` apply on top); `list_presets { category, query }`; the tool lists a
  long unfiltered library without descriptions so the model narrows it down; `get_device_info`
  groups preset names by category; `load_preset` reports the modulators it installed.
- App: the browser's preset list groups by category (collapsible, search box, the loaded preset
  highlighted) and the device editor's preset menu has one submenu per category.

## Library

| Category | n | Presets |
|---|---|---|
| Bass | 14 | Sub Bass, Reese Bass, Garage Reese, Rubber Tech Bass, Rolling Sub, Organ House Bass, Disco Octave Bass, Acid Line, Wobble Bass, Vowel Wobble, Eighties Pulse Bass, Outrun Sweep Bass, Log Drum, Pump Bass |
| 808 | 6 | 808 Clean, 808 Glide, Drill Slide 808, Punch 808, Rage 808, Phonk 808 |
| Lead | 15 | Night Drive Lead, Euphoria Lead, Fjord Hook Lead, Trance Supersaw, Festival Lead, G-Funk Whistle, Trap Sine Lead, Smoke Flute, Bendy Square Lead, Darkwave Sync Lead, Rave Scream Lead, Hyperpop Supersaw, Honky Resonant Lead, Gated Techno Lead, Mono Synth Lead |
| Pluck | 9 | Pluck Lead, Digital Pluck, Melodic Techno Pluck, Dark Trap Pluck, Tropical Pluck, Progressive Pluck, Afro Marimba, Rubber Bloop, Dream Pluck |
| Keys | 11 | House Chord Stab, Progressive Stab, Rage Chord Stab, Lofi Tape Keys, Glass Keys, Velvet Keys, Tine Keys, Tremolo Keys, House Organ, Pop Square Keys, Midnight Keys |
| Bell | 5 | Haunted Bell, Glass Bell, Steel Drum Bell, Music Box, 808 Cowbell |
| Pad | 11 | Warm Pad, Vowel Pad, Eighties Dip Pad, Future Bass Chords, Wobble Chords, Dusty Lofi Pad, Ambient Bloom, Sunrise Pad, Lush Jazz Pad, Psych Filter Pad, Liquid Pad |
| Choir | 3 | Dark Choir, Disco Choir, Snow Choir |
| Strings | 3 | Outrun Strings, String Machine, Indie Strings |
| Brass | 5 | Arena Brass, Replicant Brass, Swell Brass, Fanfare Brass, Psych Brass |
| Arp | 6 | Eighties Horror Arp, Night Pulse Arp, Disco Sequence, Progressive Arp, New Wave Sequencer, Rising Filter Arp |
| FX | 5 | Dark Drone, Noise Riser, Laser Zap, Random Burble, Downlifter |

31 presets carry modulators: wobbles and vowel wobbles (note-retriggered synced LFOs on cutoff /
Formant position), PWM brass (synced LFO on both PWM positions), self-pumping bass (synced saw on
gain), tremolo keys, gated techno lead (square LFO on cutoff), flute vibrato and tape wow (LFO on
oscillator 2 cents with oscillator 2 carrying the note), slow pad and choir drifts, sample-and-hold
burble.

## What the synth is missing (by impact on this library)

Counted over the researched recipes; each item names the presets that would improve.

| Gap | Recipes | Effect today |
|---|---|---|
| **Pitch envelope** (AD, ±48 st) | ~20: 808 knock, log drum (essential), festival/big-room zaps, melodic techno plucks, 80s horror arp, fanfare brass, hyperpop zips, future bass scoops | Short filter-envelope transients stand in (Punch 808, Log Drum, Eighties Horror Arp). |
| **Mono / legato** with glide only on overlapping notes, last-note priority | ~30 leads and basses: 808 slides, acid, classic mono leads | Glide always slides from the previous note, even for chords; presets say "write mono lines". |
| **FM** (2 operators, index envelope) | ~15: log drum, DX-style electric piano, trap bells, kalimba/marimba, steel-drum riffs | Harmonics/Digital tables and inharmonic octave partials approximate. |
| **Unison > 4** (7-16 per oscillator) | supersaw trance, future bass, rage | Two 4-voice stacks (8 voices) with wider detune. |
| **Cutoff modulation in octaves** | every cutoff LFO | Depth is linear in Hz (range 20-20000), so the same depth is a huge sweep at 300 Hz and a nudge at 3 kHz; presets tune it per base cutoff. |
| **Random / free-running start phase** | chord stabs, plucks | All voices start in phase: a chord's attack peaks coherently, so short stabs are peak-limited (they sit ~5 dB below pads in momentary loudness). |
| **HP / BP filter modes** | ~10: 808 cowbell, filtered French-house lead, Juno-style HPF on arps, supersaw low cut | LP + an EQ insert. |
| **Modulation envelope** (ADSR as a modulator: wavetable position, osc2 pitch) | sync sweeps, wavetable plucks | Slow LFOs or followers. |
| **Third oscillator / sub** | M1 organ bass, layered basses, three-oscillator leads | Oscillator 2 does double duty. |
| osc2 range beyond ±24 st | M1 organ (+31), trance leads (+31) | +19 keeps the fifth. |
| LFO delay / fade-in, drawable LFO shapes | delayed vibrato, syncopated wobbles | Mod-wheel vibrato. |
| Effects | phaser/flanger, OTT/multiband compression, multiband distortion, vinyl noise | Descriptions suggest the closest built-ins. |

Already there (the research assumed otherwise): velocity scales the filter-envelope amount
(`velocity_sensitivity`), so accents open the filter (Acid Line, Disco Octave Bass, Tine Keys).

## Sources

Syntorial preset recipes (complete values; ~60 read):
<https://www.syntorial.com/preset-recipe-sitemap1.xml>,
<https://www.syntorial.com/preset-recipe-sitemap2.xml>.
Reverb Machine patch breakdowns (Prophet-5 V, TAL-U-NO-LX, Mini V values):
<https://reverbmachine.com/blog/how-kavinsky-created-nightcall/>,
<https://reverbmachine.com/blog/drive-synth-sounds-2020/>,
<https://reverbmachine.com/blog/tame-impala-synth-sounds/>,
<https://reverbmachine.com/blog/vangelis-blade-runner-synth-sounds>,
<https://reverbmachine.com/blog/mgmt-kids-synth-sounds/>,
<https://reverbmachine.com/blog/exploring-the-yamaha-dx7-pt2/>.
Attack Magazine Synth Secrets:
<https://www.attackmagazine.com/technique/synth-secrets/808-bass-with-saturation/>,
<https://www.attackmagazine.com/technique/tutorials/reese-bass-redux/>,
<https://www.attackmagazine.com/technique/synth-secrets/fred-again-inspired-reese-basslines-with-twin-3/>,
<https://www.attackmagazine.com/technique/synth-secrets/detuned-festival-leads-with-ableton-drift/>.
Genre guides: <https://www.edmprod.com/how-to-make-phonk/>,
<https://musictech.com/tutorials/fl-studio/making-melodic-techno-plucks-in-fl-studio>,
<https://articles.roland.com/?p=67237> (amapiano), <https://www.kvraudio.com/forum/viewtopic.php?p=7983521>
(log drum), <https://musicradar.com/how-to/how-to-create-an-80s-juno-style-pad-sound>,
<https://www.musicradar.com/tuition/tech/how-to-create-a-korg-m1-style-organ-bass-sound-642650>,
<https://www.soundonsound.com/techniques/synthesizing-cowbells-claves>,
<https://violetrecording.com/how-to-make-808-slides/>, <https://noisechest.com/articles>.
