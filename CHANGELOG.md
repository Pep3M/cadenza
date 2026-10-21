# Changelog

All notable changes to Cadenza are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

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

[0.2.0]: https://github.com/Pep3M/cadenza/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Pep3M/cadenza/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Pep3M/cadenza/releases/tag/v0.1.0
