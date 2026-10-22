# Cadenza — UI/UX redesign brief

## 1. What Cadenza is

Cadenza is an open-source desktop DAW (digital audio workstation) and audio editor written in
Rust. Its differentiator: an **AI producer** that works as arranger + sound engineer. It writes
MIDI, picks and inserts instruments/effects, mixes, automates and renders, and it does all of
this through the **same command API the UI uses**. Every AI request lands as ONE undo step
("AI: add a bass"), so the user can always see, audition and revert what the AI did.

Target users: bedroom producers and songwriters who know FL Studio / Ableton / Studio One /
Bitwig, plus musicians who are not engineers and lean on the AI. Linux first, also macOS and
Windows. Desktop only, mouse + keyboard, dense pro-tool UI. Typical screens 1920x1080 and
2560x1440, must still work at 1366x768 and scale to 4K (HiDPI).

## 2. The ask

Redesign the whole app UI: **more attractive, better UX, more functional**. Keep every existing
feature (they are listed in §5) but rethink layout, hierarchy, visual language and flows.
I attach screenshots of the current state (`screens/core` = the most important, `screens/extra`
= every other feature). They are functional but look like a generic dark developer tool.

The editors of the built-in instruments and effects (EQ, compressor, synths...) come in a
second brief right after this one; here, keep the design system ready for them (knobs,
displays, meters, a palette for colored curves on dark displays).

I want, in this order:

1. **Design direction**: 2 or 3 short visual directions (mood, palette, type, one key screen
   each) so I can pick one. Name them.
2. **Design system** for the chosen direction, as tokens I can copy into code:
   - Colors: surfaces (5-6 levels, darkest to lightest), borders, text (primary/dim/faint),
     accent, semantic states (play, record, mute, solo, bypass, warning, error, automation,
     loop, marker, pattern mode, tempo, frozen), meters (low/mid/high/bg), 16 track colors that
     work on dark lanes AND as clip fills with readable text.
   - Typography: UI family + mono family (for time/numbers), sizes (small/body/button/heading/
     display for the position counter), weights.
   - Spacing scale, radii, stroke widths, control heights (dense: ~20-24 px).
   - Component states: default / hover / active / focused / disabled / selected.
   - Give the values as hex + px in a table, plus a `:root { --token: ... }` block.
3. **Screens** (high fidelity, 1920x1080, dark):
   1. Main window: transport + arrange + mixer + browser + inspector (the everyday view).
   2. Arrange close-up: track headers, clips (MIDI and audio with waveform), markers, loop,
      automation lane, folder track, take lanes.
   3. Mixer: channel strips, inserts, sends, routing, meters, master, bus and folder strips.
   4. Piano roll with velocity / controller lane.
   5. AI producer chat panel mid-request and after it (tool calls, what changed, undo, cost).
   6. Mix assistant: analysis report (loudness vs target, spectrum vs tone curve, per-channel
      levels, masking pairs, issues with Fix buttons, before -> after comparison of an AI pass).
   7. Drum sampler (4x4 pads + waveform + knobs) and sampler (keyboard zone map).
   8. Style library browser (search, filters, pattern cards, a style document).
   9. Start screen / empty project (first-run experience).
   10. Export dialog (mixdown + stems, format, loudness normalization) and its result.
   11. Recording: armed tracks, input meters, recording in progress, count-in.
4. **UX notes per screen**: what changed and why, interaction details (hover, drag, keyboard),
   empty states, error states.
5. A short **icon list** (every icon the app needs, see §6) mapped to an open-licensed icon set
   (Lucide or Phosphor preferred).

## 3. Problems I see in the current UI (please fix these)

- **No visual hierarchy.** Everything is the same flat gray with the same 12 px light font;
  panels blend into each other; section titles ("INSPECTOR", "HISTORY", "PRESETS") are tiny gray
  caps. Nothing tells the eye where to look first.
- **Transport bar** is a row of equal-weight buttons with cryptic labels: `PAT`/`SONG`,
  `PUNCH`, `MIDI rec`, `A: Read`, `No count-in`, `Follow: Page`. The position counter is the
  only thing with weight. Recording/metronome/loop state is hard to read at a glance. The
  "audio off" status and master meter on the right are nearly invisible.
- **Track headers** are cramped: tiny `A M S` + record buttons, the volume field truncates
  ("-6.0 d"), the instrument name is small and gray. Bus tracks and instrument tracks look the
  same.
- **Clips** are saturated full-color slabs; the MIDI content preview inside is hard to read;
  selected vs unselected is unclear.
- **Mixer** wastes space: very thin faders, huge empty insert boxes, truncated insert names
  ("Parame…" = Parametric EQ), sends as plain text fields, no pan knob (just "C"/"L15" text),
  meters are thin and dim, master is not visually distinct. Bus / track / folder strips look the
  same.
- **Browser** (left) is wide but mostly empty; Devices / Files / Library tabs are tiny; device
  list has no icons, categories or favorites; presets are a loose list below.
- **Inspector** (right) is mostly empty space, with Undo history squeezed under it.
- **Notifications**: toasts duplicate the status bar and show long absolute paths.
- **Start screen** is a small modal floating over an empty, disabled-looking app.
- **AI chat**, the product's main differentiator, looks like any other side panel: tool calls are
  monospace lines, errors are red truncated text, the "what changed / undo it" result is not
  prominent, the quick actions are plain chips. It should feel like a collaborator, not a log.
- **Drum sampler**: pads truncate names ("Tam…", "Peda…"), labels repeated, two rows of knobs
  with duplicated names (pad Gain vs kit Gain) and no grouping.
- **Keyboard-first features are invisible**: there are many shortcuts (F5-F12 panels, piano
  roll tools P/B/E/C, Ctrl+B clone...) but nothing hints at them except an F1 overlay.

## 4. Principles

- **Music first, chrome second.** The arrange and piano roll content (clips, notes, waveforms)
  should be the most colorful thing on screen; the chrome is calm and recedes.
- **Glanceable state.** Playing, recording, armed, looping, pattern vs song mode, metronome,
  automation write mode, plugin dropouts: readable from across the room.
- **AI as a visible collaborator.** Show what the AI is doing (live tool steps in plain words),
  what it changed (tracks/clips highlighted, a diff-like summary), and one obvious Undo /
  Keep. Offer context-aware suggestions from the current selection. Never hide that it costs
  money (per-request and session cost).
- **Dense but breathable.** Pro users want lots on screen; use alignment, grouping and contrast
  instead of extra padding.
- **Progressive disclosure.** Simple by default (new user can make a song with the AI and a few
  clicks), depth on demand (automation, takes, routing, sidechains, stems).
- **Consistent controls**: one knob, one fader, one toggle, one value field (drag to change,
  double-click to type), one dropdown, used everywhere.
- **Accessibility**: text contrast ≥ 4.5:1 for body text, state never conveyed by color alone
  (icons/labels too), focus rings for keyboard navigation.
- **Original**: take ideas from Bitwig, Ableton Live, FL Studio, Studio One, Logic, but do not
  copy their look or assets. Cadenza needs its own identity (the logo is a blue "CADENZA"
  wordmark; feel free to propose a refined brand accent).

## 5. Feature inventory (everything the UI must still offer)

**Window layout** — dockable panels toggled by keys: F5 arrange, F6 browser, F7 piano roll /
editor dock (bottom), F8 inspector, F9 mixer (bottom), F10 AI panel (right), F11 maximize focused
panel, F12 close tool panels, F1 shortcut overlay. Menu bar: File, Edit, Track, View, AI, Help.
Status bar: saved/unsaved, last action, counts.

**Transport** — go to start, stop, play, record; PAT|SONG mode switch (pattern mode loops the clip
open in the piano roll and solos its track); loop on/off; metronome (on play / on record, level);
count-in 0/1/2/4 bars; punch; MIDI record mode (merge/overdub/takes per pass/quantize);
automation mode Read/Touch/Latch; position (bars.beats.ticks) + time (m:ss.mmm); BPM; time
signature; follow playhead Off/Page/Continuous; audio device status; dropout warning "⚠ n";
master output meter; tempo lane toggle.

**Arrange** — song overview strip with draggable viewport; ruler with bar numbers, loop region,
time-signature tags; marker lane (Intro, Verse, Chorus...); tempo lane and signature lane;
track headers (number, color, name, instrument, record arm, automation toggle, mute, solo,
volume, input selector + monitor button + input meter for audio tracks, take lanes toggle,
FRZ freeze badge, folder disclosure + indentation); clips (MIDI with note preview, audio with
waveform, fades, automatic crossfades, take badge); automation lanes under each track (curves,
points, shapes linear/hold/smooth); snap + grid, zoom, rubber-band selection, split, duplicate,
trim; "+ Instrument / + Audio / + Bus / + Folder" and drop zone; drag & drop of devices,
presets, files and library patterns (ghost clip preview).

**Mixer** — strip per track + buses + master; inserts (add, reorder, bypass, remove, open plugin
editor), sends (level, pre/post, + Send), output routing, pan, fader, peak/RMS meters with
peak hold, mute/solo/arm, folder strips (VCA fader only), frozen badge.

**Browser** — Devices (built-in instruments: Drum Machine, Drum Sampler, Electric Piano, Poly
Synth, Sampler; effects: Chorus, Compressor, Delay, Gain/Utility, Limiter, Parametric EQ,
Reverb...; CLAP/VST3 plugins after a scan), presets per device, Files (bookmarks, audio/MIDI
filter, drag to import), Library (style packs: styles, forms, progressions, grooves, patterns;
search + filters kind/genre/role/section/energy; cards; click auditions, drag writes a clip;
style documents with feel, harmony, forms with Apply, roles, mix targets; credits/licenses).

**Inspector** — properties of the selected track / clip / device (params as sliders/knobs with
units, Edit plugin window, presets), undo history.

**Editors (bottom dock)** — Piano roll: tools Draw / Paint / Select / Slice, snap, key + scale
highlighting, ghost notes from other tracks, clip menu, quantize, velocity and controller lanes
(mod wheel, pitch bend, sustain, CCs), note properties popover. Audio editor: waveform, region
handles, fades + curves, clip gain, normalize / reverse / stretch. Drum sampler and Sampler
editors.

**AI panel** — two tabs: PRODUCER (chat: streamed markdown answers, collapsible thinking, tool
steps with ✓/✗, quick-action chips, Stop, per-request footer with status, cost, session total and
undo label, settings: provider, model, reasoning effort, budget, API key) and MIX (mix
assistant: Analyze, report sections MASTER / ISSUES / TONE / CHANNELS / MASKING, Fix / Fix all,
"AI mix pass" with reference track and notes, before -> after table, Keep / Revert).

**Recording** — arm tracks, input selection and monitoring (Off/Auto/On), input meters, count-in
display "-1.3", live waveform/notes while recording, take lanes and comping (click a take, drag
to swipe-comp), punch, loop recording.

**Project & dialogs** — start screen (new, open, demo song, recent), export dialog (range,
mixdown, stems, format WAV/FLAC/MP3, bit depth, dither, sample rate, peak/LUFS normalization,
metadata, folder, progress, result table with loudness per file), collect files, locate missing
files, recover autosave prompt, unsaved changes prompt, audio settings (devices, sample rate,
buffer, latency offset, test tone), plugin crash toasts.

## 6. Technical constraints (important: the result must be implementable)

The UI is built with **egui (immediate mode, Rust) rendered with wgpu**, not HTML/CSS. Design
within what egui draws well:

- Filled/stroked rounded rectangles, lines, circles, polygons, text, textures (images/SVG
  rasterized), simple linear gradients (as meshes) and soft drop shadows on floating windows
  and popups. **No** backdrop blur, glassmorphism, complex filters, or animated illustrations.
  Subtle animations (hover fades, panel slide, value easing) are fine.
- Custom fonts must be embeddable TTF/OTF with an open license (OFL/Apache/MIT), e.g. Inter,
  IBM Plex Sans, Geist, JetBrains Mono, IBM Plex Mono. They must cover arrows and geometric
  symbols (← → ◆ ● ✓ ✗) or I fall back to another face.
- Icons: an open icon set (Lucide ISC / Phosphor MIT) as SVG; tell me sizes (14/16/20 px).
- Dark theme is the main one. A light theme is optional (nice to have, low priority).
- Everything is resizable; panels have min sizes; the arrange, piano roll and mixer scroll and
  zoom. Show me how the layout adapts at 1366x768 and 2560x1440.
- Realtime meters and the playhead redraw every frame: keep them simple shapes.
- Keep the existing panel model (F-keys, docks) unless you have a clearly better proposal; if
  you change navigation, explain the new model.

## 7. Attached screenshots

`screens/core/` (start here):
01 main window (arrange + mixer) · 02 start screen · 03 empty project · 04 piano roll ·
05 AI chat after a request · 06 AI chat running · 07 drum sampler editor · 08 library style
document.

`screens/extra/`: device/clip selected (inspector), automation lanes, controller lanes, time
signatures, folders, armed tracks with takes, recording in progress, audio editor, crossfade,
sampler editor, library list and preview, mix assistant empty/analyzing, export dialog/result,
pattern mode, shortcut overlay, note properties, stale freeze + dropout warning, audio settings,
AI settings.
