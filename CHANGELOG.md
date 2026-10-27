# Changelog

All notable changes to Cadenza are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.3.1] - 2026-10-27

Workflow fixes from day-to-day use: transport, recording, layout and piano roll selection.

### Fixed

- **Stop** on a stopped transport goes back to the start of the song (the pattern start in
  pattern mode).
- **Record button stuck lit**: clicking it while record mode is on and nothing is recording
  turns record mode off; the button now looks different when only the mode is on (outline)
  and while recording (filled), and its tooltip says what a click does. Recording no longer
  gets stuck after the audio device disappears or a take fails to finish in time: the next
  recording starts normally.
- **"+ Instrument / Audio / Bus / Folder"** below the tracks always adds the track at the end
  of the list, not after the selected track.
- **F11 / Ctrl+Enter** restores exactly the layout from before maximizing; View > "Maximize
  focused panel" maximizes the panel you were working in, not the one under the menu.
- **Bottom dock**: the mixer and the editors (piano roll, audio editor, docked device editor)
  never share it. Opening one closes the other and closing it leaves the dock empty.
- **Piano roll rubber band** replaces the selection; hold Shift to add to it.

## [0.3.0] - 2026-10-25

Melodyne-style note editing for audio, audio to MIDI (melodies, chords, drums, 808s),
per-note expression (MPE) in MIDI clips, and genre libraries of synth presets and drum kits.

### Added

- **Note editor for audio clips** (audio editor "Notes", clip menu "Edit notes…"): notes
  detected on a vocal or solo take as blobs on semitone rows; retune, move, split and merge
  notes, flatten or deepen vibrato and drift, shift formants, change a note's level, tame
  sibilants. Edits render in the background with a closed loop (corrected notes land within
  a few cents), only around the notes you touch, and you hear a note while you drag it.
- **Correct pitch / Quantize time / Leveling** macros, with the key detected from the take
  (scale and tuning offset) or taken from a MIDI melody track.
- **AI tuning engineer**: "Tune with AI" on an audio clip and `cadenza tune`; the producer
  gets tools to detect, analyze, tune and edit notes (`detect_audio_notes`,
  `tune_audio_notes`, `edit_audio_notes`, …).
- **Pitch Correct**: a live tuner effect (retune speed, humanize, 12 scales, reference
  pitch, presets Natural / Modern Pop / Hard Tune / Gentle) with its own editor; it can
  follow the notes of a melody track (`set_midi_input`).
- **Audio to MIDI** (audio clip menu "Convert to MIDI…", drag an audio clip onto an
  instrument track, `audio_to_midi`, `cadenza notes to-midi`): melodies with optional pitch
  bend, chords (a built-in Basic Pitch model, no download), drums (kick, snare, hats… on GM
  notes) and pitched hits (808s, toms: a note per hit with its glide). Quantize, key snap,
  hum mode ("Hum to MIDI when recording" on instrument tracks) and MPE export.
- **Per-note expression in MIDI clips**: each note can carry its own pitch curve, pressure
  and timbre (MPE-style), played by Poly Synth, Electric Piano (pitch) and Sampler and sent
  to CLAP plugins as note expressions. Piano roll: Bend tool (G) to draw a note's pitch
  curve, "Note pressure" / "Note timbre" event lanes, Straighten/Clear in the note popover.
  AI tools: `bend`, `pressure`, `timbre` on notes, `set_note_bend`, `set_note_expression`.
- **MPE files**: export a clip or track as MPE (`export_midi { mpe }`) and import MPE files
  with every note's bend, pressure and timbre.
- **Synth preset library**: 93 Poly Synth presets in 12 categories (bass, 808, lead, pluck,
  keys, bell, pad, choir, strings, brass, arp, FX), level-matched, with modulation; presets
  grouped by category with search in the browser and the editor menu.
- **Drum kit library**: 36 genre kits for the Drum Sampler (house, techno, trap, hip-hop,
  reggaeton, latin, afro, acoustic, funk, pop, retro…) from 256 bundled samples, credited
  in `assets/samples/CREDITS.md`.
- CLI: `cadenza notes analyze | segment | to-midi | tune | eval | corpus`.

### Packaging

- Packages ship the bundled drum samples (`samples/`) and the Basic Pitch license
  (`LICENSE-basic-pitch`, Apache-2.0).

### Fixed

- Security: `ctutils` 0.4.6 (RUSTSEC-2026-0385), `tokio` 1.54.1 (RUSTSEC-2026-0386);
  `clipboard-win` 5.4.3 (the previous release was yanked).

### Compatibility

- Projects from 0.2 open and sound the same: the new note, clip and device fields are
  optional (`format_version` stays 1) and new parameters default to the old sound.

## [0.2.0] - 2026-10-21

The Graphite redesign: a new look for the whole app, a dedicated editor for every built-in
device, an FL Studio-style workflow, ten languages and a set of new DSP features.

### Added

- **Graphite look**: new theme with IBM Plex fonts, start screen, empty-project screen,
  redesigned mixer, transport, arrange, piano roll, library, mix assistant and dialogs.
- **Device editors** for all 13 built-ins (EQ, Compressor, Limiter, Reverb, Delay, Chorus,
  Saturator, Gain, Poly Synth, Electric Piano, Drum Machine, Drum Sampler, Sampler) with
  pictures drawn from the DSP itself, live meters and analyzers, presets, A/B compare,
  "Ask AI" with highlighted AI changes, and a compact form for the inspector.
- **Floating windows** (FL Studio style): device editors and track/clip properties open in
  movable, resizable windows, several at once, remembered per item. The inspector is now
  optional (F8).
- **Playlist editing**: arrange tools Select / Draw / Paint / Split / Mute; copy, cut and
  paste clips; Ctrl+B clone; knife across tracks; mute single clips; per-track heights;
  linked clips (edit one, all copies follow) with "Make unique"; chop, stretch (Shift) and
  repeat a pattern by resizing.
- **Languages**: Spanish, Portuguese (Brazil), French, German, Italian, Russian, Japanese,
  Simplified Chinese and Korean (View > Language). CJK text uses a system font. The AI
  producer answers in the UI language.
- **Mixer**: master loudness (short-term, integrated, true peak), Mono and Dim monitoring,
  sidechain targets in the output menu, Peak / Peak+RMS meters.
- **Responsive layout** from 1024×700 to 2560 px: transport modes menu, browser rail,
  overlay panels.
- **EQ**: dynamic bands, mid/side per band, band solo, match EQ to a reference (also an AI
  tool, `match_eq`), and a collision view that suggests cuts against other tracks.
- **Compressor**: real key trace, listen to the key, key filter (HP/LP on the detector).
- **Reverb**: freeze and ducking. **Delay**: up to 4 taps. **Limiter**: 1x/2x/4x
  oversampling.
- **Poly Synth**: wavetable oscillator with 5 morphing tables and two new presets.
- **Modulation**: LFOs and envelope followers on any built-in parameter, with rings on the
  knobs (also an AI tool, `set_modulators`).
- **MIDI learn**: map hardware controllers to device parameters, faders, pans and sends;
  "MIDI Mappings…" window.
- **Explain this device**: the AI explains a device's settings without changing anything.
- Live voice display: lit keys, envelope dots and sample playheads in instrument editors.
- Kit polish: hover readouts, "Ask AI about this" on every control, editor keys (B bypass,
  arrows to select and nudge), value easing.

### Changed

- **Project files are now `.cdz`** (was `.cadenza.json`) and project archives `.cdzpack`
  (was `.cadenza.zip` / `.cadenzapack`). Only `.cdz` files open as projects: rename older
  projects (`Song.cadenza.json` → `Song.cdz`; the content is unchanged). Older archives
  still unpack, with their project file renamed to `.cdz`.
- A mixer insert opens its editor in a floating window on a single click.
- In the Sampler, Alt+arrows move the selected zone (plain arrows nudge values).
- Shift+A switches A/B on the Poly Synth and Electric Piano, whose A–K keys play notes.

### Fixed

- Linux installs go to `~/.local/opt/cadenza`, not the app's data folder.
- macOS: AppKit Touch Bar support disabled to avoid a crash on quit.
- macOS: Korean (and some Japanese) text no longer draws as empty boxes: several system
  CJK fonts are combined until every script is covered.
- macOS: the device probe no longer allocates on its first read of a running meter.
- Security: `rustls-webpki` 0.103.16 and `webpki-roots` 1.0.10 (RUSTSEC-2026-0381, RUSTSEC-2026-0382).

### Packaging

- **Windows installer**: `cadenza-<version>-windows-x86_64-setup.exe`, a classic setup
  wizard (per-user or all users, Start menu and desktop shortcuts, `.cdz` / `.cdzpack` file
  associations, optional `PATH` entry, uninstaller, in-place upgrades, wizard in the app's
  ten languages). The zip and `install.ps1` stay.
- Windows binaries link the C runtime statically: no VC++ redistributable needed.
- AUR package `cadenza-bin` for Arch Linux and Omarchy.

### Compatibility

- Existing projects open and sound the same once renamed to `.cdz`: every new device
  parameter has a default that reproduces the old output bit for bit, and new project
  fields are optional (`format_version` stays 1).

## [0.1.1] - 2026-10-15

### Added

- Application icon on every platform.

## [0.1.0] - 2026-10-14

First release: DAW with an AI arranger and sound engineer, MCP server, CLAP/VST3 hosting,
recording, mixing, export and the style library.

[0.3.1]: https://github.com/Pep3M/cadenza/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Pep3M/cadenza/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Pep3M/cadenza/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Pep3M/cadenza/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Pep3M/cadenza/releases/tag/v0.1.0
