# Graphite redesign — implementation plan

Source: the "Cadenza visual directions" design export (pages 00–18, kept outside the repo;
the briefs are in `redesign-brief/`). Part 1 (`PROMPT.md`) is the app shell, part 2
(`PROMPT-devices.md`, pages 14–18) the built-in device editors.

## Done

- Tokens, fonts (IBM Plex), egui style (`theme`), clips/headers in the arrange, transport,
  status bar, browser, inspector overview, chat, panel titles.
- Start screen page + empty project state (09).
- Mixer (03): toolbar, gutter + dB scale, buses divider / ghost strip, input row, pan knob,
  long fader, narrow mode, "New bus from this track".
- Devices D0–D6 (2026-10-19, tasks T1–T18 in `redesign-progress.md`): kit, probe + exact DSP
  pictures, device editor host/header (presets, A/B, undo hint, Ask AI + AI change card,
  pop-out), dedicated editors for all 13 built-ins (dock + inspector compact).

- Shell batch 4, FL parity (floating windows, playlist editing, linked clips), 10 languages, device gaps and new DSP (T19-T41, 2026-10-21): see `redesign-progress.md`.

## Shell, still to do (queued as batch 4, T19–T26 in `redesign-progress.md`)

1. Inspector tabs Track / Clip / Device (01, 02).
2. Arrange tools Select / Draw / Split, ruler and marker polish (02).
3. Piano roll screen (04): toolbar, keyboard, event lane look.
4. Library tab cards and style/form documents (08).
5. Mix assistant report cards (06).
6. Export, Locate missing files, audio settings dialogs (10, 11).
7. Responsive layout for narrow windows (12).
8. Mixer extras needing the engine: live LUFS, Mono/Dim on the master, sidechain targets in
   the output menu, Peak / Peak+RMS meter mode.

## Devices (part 2)

Every built-in device gets its own editor in the bottom dock (like the piano roll and the
sampler editors), a compact 360 px form for the inspector, and an optional floating window.
Plugins keep the generic controls / their own editor.

### D0 — device kit (`ui::device_kit`, pure helpers in `state::device`)

- Display surface `#0b0c0e`; glow = one 5–8 px stroke at 15–20 % alpha under a 1.6 px line;
  fills 7–20 %. Band/voice palette (coral, orange, yellow, green, teal, sky, violet, pink);
  function colours: blue AI/selection, violet `#c884e8` automation, coral `#ff7a5c` gain
  reduction, teal/orange L/R.
- Knob S/M/L/XL (270°, arc 2.5/3/4 px), uni/bipolar (bipolar from 12 o'clock), automation
  ring (violet; dot = playing value, pointer = written value), 200 px drag = full range,
  Shift ×0.1, wheel 1/100 (Shift 1/1000), floating value 2 px above, never under the pointer.
- Value readout: click to type, units/shorthand ("3.2k", "-6", "1/8", "C4"), Enter/Esc, out of
  range flashes red then clamps, Tab = next param (pure `parse_value(param, text)`, tested).
- Toggle (function-coloured ring), segmented (≤ 5 choices), note-value grid, mode/wave picker
  cards (picture = label), ADSR editor (cube-root time axis, A ⇔, D both, S ⇕, R ⇔),
  frequency display (log 20 Hz–20 kHz, labelled grid, pre fill / post line analyzer, 20 px
  nodes, dormant = hollow ring, piano strip with hovered key), meters (green ≤ −9, yellow ≤ −3,
  red; 1.5 s peak hold; GR hangs from the top), level history (min/max per column, right to
  left, freezes and dims to 50 % when stopped).
- Shared rules on every control: hover readout after 300 ms, double-click = default, right-click
  Automate / Reset / Copy value / Paste value / Ask AI about this; values ease over 80 ms when
  changed by preset, A/B, undo or the AI (never while dragging).
- One `SetParam` per gesture, like the existing knobs; undo labels name the param
  ("Peak 2 gain").

### D1 — engine data for the displays

- Device probe: for the device whose editor is open, the graph copies its input and output
  into a wait-free ring (no alloc, covered by `no_alloc`); the UI derives the spectrum
  (1/6-octave, 150 points, falls 12 dB/s), level history, scope (300-point trigger-aligned) and
  goniometer/correlation from it.
- Device readings: an optional `Processor` hook publishing a few floats per block into atomics
  (compressor/limiter gain reduction, chorus LFO phase, envelope stage, drum voice triggers,
  sampler playheads).
- Exact pictures from the DSP: public pure functions in `cadenza-dsp` for the EQ magnitude
  response, filter responses (poly synth / sampler SVF), saturator shaping curves, so the UI
  draws what is heard; drum voice envelopes from a short offline render on a worker.
- Live loudness for the limiter (short-term every 100 ms, integrated, true peak) from the
  master tap with `cadenza-analysis`; also feeds the mixer's LUFS (shell item 8).

### D2 — device editor host and header

- `view.device_editor` in the bottom dock; opened by double-click on a mixer insert, a track
  header instrument, or inspector "Edit" (built-ins); 320–420 px high, displays take the extra
  height.
- Header: power, name, track chip, preset ‹ › ▾ (factory + user presets, amber dot = unsaved),
  Save, A/B (two full states, A→B copy; one `SetDeviceState` with params), undo hint,
  sidechain source (devices that have one), IN/OUT meters, Mix (devices with `mix`), Ask AI,
  pop-out, close. Compact header: Save, A→B, Mix, undo hint move into ⋯.
- Floating window (egui window over the workspace, shadow, "Dock", "Keep on top", size
  remembered per device).
- AI change: params the last AI request moved get a blue ring + delta tag, summary card with
  Keep / Undo / hold-to-hear-before (live params only, no command); touching a param keeps it.
- Keyboard while focused: ←→ select band/pad/zone, ↑↓ nudge, B bypass, A/B compare.
- Responsive tiers: ≥1600 all; 1600 header drops the undo hint; 1400 secondary displays 240 px,
  knob grid 3→2; 1100 secondary displays become tabs; 800 two-row header, knobs S and wrap;
  560 instruments stack OSC/FILTER/AMP; 360 compact. The main display is never removed.

### D3 — EQ and Compressor (15)

- EQ: 7 band nodes (HP, LS, 1, 2, 3, HS, LP), summed curve + faint per-band fills, pre/post
  analyzer, band chips row, selected band panel (freq + note name, gain, Q / slope), range
  ±12/±24, output; wheel = Q or slope step, ⌘-drag gain only, ⌥ axis lock, drag curve moves
  nearest band, double-click empty space wakes the nearest dormant band, 1–7 select, Del
  sleeps the band. Compact 360 form.
- Compressor: level history (input, output, GR trace, key trace) with draggable threshold tag,
  transfer curve (threshold dot ⇔, ratio above the knee ⇕, knee shading), big GR meter + hold,
  attack/release/knee/makeup/mix knobs, auto-makeup suggestion with Apply, sidechain source
  picker (uses `SetSidechain`). Compact 360 form.

### D4 — other effects (16)

Limiter (waveform + GR, draggable ceiling, loudness panel with target chips −14 / −9 / −23,
reset integrated), Reverb (energy/time tail with pre-delay, decay, damping nodes; space box
for size/width; low-cut strip; wet meter), Delay (taps on a beat timeline, L/R alternating
with ping-pong, sync/free + note grid, feedback filter band), Chorus (animated two-voice LFO
traces, width fan), Saturator (mode cards, shaping curve with drive drag, before/after scope,
harmonics from a test sine), Gain/Utility (big gain, goniometer drag pan/width, Ø L / Ø R,
correlation).

### D5 — synth instruments (17a–c)

Poly Synth (signal flow OSC 1 + OSC 2 + NOISE → FILTER → AMP; wave cards, semi/cents,
unison; draggable filter node; amp and filter ADSR editors; output scope; on-screen keyboard
and A–K computer keys, Z/X octave; expert row), Electric Piano (instrument face, tone
partial bars mellow↔bright, tremolo LFO picture), Drum Machine (one strip per voice with
colour, envelope picture and level bar; selected voice large; pads flash on triggers).

### D6 — sample instruments (17d–e), replacing today's editors

Drum Sampler (kit column, 4×4 pads with name, note, mini waveform, tags for choke / extra
output / layers / round-robin; drag pad onto pad swaps, ⌥ copies; folder drop maps a kit by
names; selected pad waveform with start/end flags and A/D envelope; velocity layers,
round-robin, reverse, choke, output). Sampler (zone map keys × velocity with draggable /
resizable zones and root dot; selected zone waveform with loop region and crossfade ramp; loop
mode; globals: amp/filter ADSR, filter curve, trigger, polyphony, transpose, glide, legato,
velocity curve, key track).

### Needs new DSP (from page 18 "Ideas" and missing hooks; queued as batches 5–6, T27–T36)

EQ band solo/listen, compressor key listen and key filter, dynamic EQ, mid/side bands,
match EQ to reference (engine ready), collision view (engine), LFO/envelope modulation,
reverb freeze + ducking, multi-tap delay, limiter oversampling, wavetable morph, MIDI CC
assignment, AI "explain this device".
