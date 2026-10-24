# Drum kit library

The `drum_sampler` factory library: 36 genre kits in 12 categories, plus the two synthesized
kits it had before ("Synth Kit", "Tight Synth Kit", now in Retro). They are made from 256 bundled
samples (`assets/samples/*.flac`, 48 kHz 16-bit mono, 8.1 MB). Presets live in
`crates/dsp/src/presets/drum_sampler.rs`, which is **generated**: edit the specs in
`tools/drum-kits/` and regenerate (see below) instead of editing it by hand.

| Category | Kits |
|---|---|
| House | Deep House, Chicago Jack, Tech House, Filter House |
| Techno | Berlin Techno, Industrial Techno, Minimal Click |
| Dance | Big Room, Future House, Eurodance |
| Trap | Atlanta Trap, Rage Trap, Phonk, UK Drill |
| Hip-Hop | Electro 808, Boom Bap, Lo-Fi |
| Retro | Synthwave, Synth Kit, Tight Synth Kit |
| Acoustic | Studio, Rock, Jazz, Unplugged Rods |
| Funk | 70s Funk, Minneapolis, Disco |
| Pop | Modern Pop, 80s Pop |
| Reggaeton | Classic Dembow, Perreo, Latin Trap |
| Latin | Salsa Percussion, Cumbia, Bachata |
| Afro | Afrobeats, Amapiano, Afro House |

`KIT_CATEGORIES` (dsp) holds these names; `PRESET_CATEGORIES` = `SYNTH_CATEGORIES` then
`KIT_CATEGORIES`, and the app translates every one (`i18n/external.rs`).

## Method

1. **Research.** One recipe per kit from published genre production breakdowns: tempo, swing,
   which machine or recording each pad comes from, decays, tuning, choke groups, what is layered.
   Each number was marked as sourced or derived; the machine voices (808, 909, 707, CR-78) got
   clean-room synthesis recipes from public descriptions of how the circuits behave. No samples of
   the original machines were used.
2. **Sources.** Electronic voices are synthesized by the drum lab (`tools/drum-kits/drumlab.py`,
   numpy/scipy): bridged-T style 808 kicks, 909 kicks with a pitch sweep and a click, metallic hats
   from six square oscillators with a 6-bit crush for the 909, multi-burst claps, cowbells, congas,
   toms, trap 808 basses tuned to F1 (the pad's tune knob moves them to the song key), a rumble
   kick, gated reverb, bit/sample-rate reduction, a phaser. Acoustic kits and hand percussion are
   cut from redistributable recordings (table below); processed variants (lo-fi, gated room, the
   dembow snare, a funk rim, timbales from untensioned snares tuned up) are made from them.
   Stand-ins where no free recording was good enough: the güira and the talking drum are
   synthesized, the timbales are snares without snares, tuned up.
3. **Cutting.** Each sample: decode (ffmpeg) -> start/length -> optional pitch, high/low-pass,
   effects -> fade -> 3 dB of transient limiting -> peak at -1 dBFS -> FLAC. Only samples some kit
   uses ship.
4. **Pads.** A kit names samples per role (kick, snare, clap, rim, closed/pedal/open hat, toms,
   crash, ride, tambourine, cowbell, or any GM percussion note). Roles fix the GM note, a default
   pan and the hat choke group; kits may override gain, pan, tune, attack, decay, choke and reverse
   per pad, stack velocity layers (`vel`) or round-robin groups (`rr`). The builder rejects more than
   16 notes, duplicate notes, notes `pad_layout` cannot show and descriptions outside 60-400 chars.
5. **Naming.** Generic genre names and descriptions (tempo range, how to play it); no artist,
   song, brand or product names in shipped text.

### Percussion on the 4x4 grid

Latin and Afro kits need congas, bongos, timbales, guiro, claves... which are GM notes but not
among the 16 pads of `PADS`. `drum_sampler::pad_layout(notes)` gives such a note the pad of an
unused GM pad, least essential first (`TAKEOVER`: Kick 2, Snare 2, Pedal Hat, Tambourine,
Cowbell, Ride, Crash, toms...), lowest note first. The processor's trigger counters and the
editor (`drum_sampler_editor::kit_pads`) use the same layout, so the grid shows "Hi Bgo", "Mt Cga"
and so on. A kit on the plain GM pads gets `PADS` unchanged.

## Level policy

- **Pad gains** come from role loudness targets relative to the kick (K-weighted level of each
  hit's loudest 150 ms; the kick at -9 dB), clipped to -30..+6 dB. Short sounds are measured fairly
  that way; a 400 ms window undercounted them.
- **Kit gain** is calibrated (`calibrate.py`) by playing the kit's genre groove: aim at -14 LUFS
  momentary with a -1.6 dBTP ceiling; whichever hits first wins.
- **Test** (`crates/engine/tests/drum_kits.rs`, `every_kit_sounds_at_a_matched_level`): each kit
  plays its category's groove at the category tempo plus a tour of every pad and must land in
  -16..-10 LUFS momentary (or be peak-limited: true peak >= -2.5 dBTP and no quieter than
  -23 LUFS) and stay under -1 dBTP. Coincident hits (kick + clap + crash) made the old Synth Kit
  reach +9 dBTP, so its pads were rebalanced and it is now quieter than before.
- Other tests: every file in `assets/samples` is in `KIT_SAMPLES`, credited in `CREDITS.md`, used
  by a kit, on a GM drum note, peaks above -6 dBFS and lasts under 4 s; at least 30 genre kits,
  two per category (Retro excepted), descriptions 60-400 chars, every kit valid as an `AddTrack`.

Listen: `CADENZA_KIT_AUDITION_DIR=dir cargo test -p cadenza-engine --release --test drum_kits --
--ignored --nocapture audition_kits` writes one WAV per kit and prints loudness, true peak, crest
and band levels.

## Sources and licenses

All files are credited per source in `assets/samples/CREDITS.md`, which ships next to them.

| Source | License | Files |
|---|---|---|
| Cadenza drum lab (synthesized) | GPL-3.0-or-later | 69 |
| Versilian Community Sample Library (VCSL) | CC0-1.0 | 39 |
| AVL Drumkits: Black Pearl, Blonde Bop, Blonde Bop Hot Rod (Glen MacArthur) | CC-BY-SA-3.0 | 69 |
| Hydrogen drumkit GMRockKit | GPL (no version given, so any: GPL-2.0-or-later) | 30 |
| Hydrogen drumkit SF3007-2011-Set-03 | WTFPL | 27 |
| Hydrogen drumkit Millo MultiLayered 2 | GPL (no version given) | 11 |
| Hydrogen drumkit circAfrique v4 (Jim Harney) | CC-BY-SA-3.0 | 9 |
| Hydrogen drumkit Gimme A Hand 1.0 | GPL (no version given) | 2 |

CC-BY-SA samples stay CC-BY-SA (the edits are listed in `CREDITS.md`); they are data the program
plays, not code linked into it. Excluded after review: kits sampled from commercial drum machines
(e.g. a Denon CRB-90 set) and kits with unclear attribution terms.

## Distribution

The samples live in the repository (`assets/samples`) and ship with every package: next to the
binaries in the tar.gz, zip and Windows installer (`samples/`), at `usr/share/cadenza/samples` in
the AppImage, `Contents/Resources/samples` in the macOS bundle and `/opt/cadenza/samples` from the
AUR package. `engine::factory::bundled_sample_dir()` finds them (`$CADENZA_BUNDLED_SAMPLES`, then
those places, then the repository for development builds; the folder must contain `CREDITS.md`).
`ensure_factory_samples` copies new or changed files into the user's factory folder once per
process (flat names, so `factory_relinks` and portable projects keep working), and
`list_factory_samples` lists the installed ones (MCP: filter by `query` words and `pad`, 40 by
default, `total`).

## Regenerating

```sh
cd tools/drum-kits
python -m venv .venv && .venv/bin/pip install -r requirements.txt   # plus ffmpeg on the PATH
./fetch_sources.sh                        # ~400 MB into ./sources (or $CADENZA_KIT_SOURCES)
.venv/bin/python build.py ../..           # samples, CREDITS.md, drum_sampler.rs
.venv/bin/python calibrate.py ../.. 2     # kit gains (kit_gains.json), code only
cargo fmt && cargo test -p cadenza-engine --release --test drum_kits
```

`build.py ../.. --code-only` rewrites only the Rust file and `CREDITS.md`. The build is
deterministic: the same sources give the same FLAC files byte for byte. Specs: `samples_synth.py`
(drum lab voices), `samples_real.py` (cuts from recordings), `kits_electronic.py` and
`kits_real.py` (the kits); `kitgen.py` has the roles, the builder and the level matching.

## Next

- Real güira, talking drum and timbale recordings under a free license would replace the stand-ins.
- Per-kit grooves in the style library (the test grooves are a start) so a kit preview plays its
  genre.
- Kit-wide character controls in the processor (drive, room) would let fewer samples cover more
  variants.
