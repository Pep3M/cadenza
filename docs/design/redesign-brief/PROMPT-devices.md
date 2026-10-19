# Cadenza — built-in instruments & effects: device UI brief

This is the second part of the Cadenza redesign (the first part redesigned the app shell:
arrange, mixer, piano roll, AI panel). **Use the design system we defined there** (colors,
type, spacing, controls) and extend it with what devices need.

## 1. Context

Cadenza ships 13 built-in devices (8 effects, 5 instruments). Today none of them has its own
interface: every device is a generic list of sliders in the inspector (see
`screens/devices/01-eq_as_sliders_today.png`: the EQ is 18 sliders). The two sample
instruments have a basic editor in the bottom dock (`02`, `03`).

I want every built-in device to get a **dedicated, visual, interactive editor**, the way
FabFilter does it: the main control IS the visualization (you grab the EQ curve, the
compressor's transfer curve, the envelope shape), with a few well-chosen knobs around it.

**Inspiration: FabFilter (Pro-Q, Pro-C, Pro-R, Pro-L, Saturn, Timeless, Twin).** What I like
and want to capture:
- Big, interactive display taking most of the window; direct manipulation (drag points/bands,
  mouse wheel for Q/width, double-click to reset or create).
- Real-time feedback drawn behind the controls: spectrum analyzer, gain reduction trace, level
  history, waveform.
- Very clean dark surfaces, thin precise lines, glowing colored curves, soft fills under
  curves, one color per band/element, excellent typography for values.
- Values appear in a floating label next to what you are dragging; knobs are few and large.
- Expert controls hidden in a collapsible bottom bar.
- Smooth, subtle animation (curves easing to new values).

**Do not copy FabFilter's look literally** (no trade dress, no identical layouts or assets).
Take the principles and make a family that clearly belongs to Cadenza.

## 2. Where device editors live

- Opened by double-clicking a device in the mixer insert slot, in the track header or in the
  inspector ("Edit"). Shown in the **bottom editor dock** (same place as the piano roll and
  the sampler editors) and optionally popped out into a floating window.
- Must work at **two sizes**: dock (full width x ~320-420 px high) and a compact view (~360 px
  wide) for the inspector side panel. Show both for the EQ and the compressor; at least the
  dock size for the rest.
- A shared **device header** for every device: device name + track name, power/bypass,
  preset browser (prev / next / dropdown / save), A/B compare, undo/redo hint, input/output
  meters, dry/wet where the device has `mix`, and an "Ask AI" button (the AI can tune the
  device: e.g. "make the vocal brighter" -> it moves EQ bands; show how its change appears,
  highlighted, with Undo).
- Every control must show: the value with unit while hovering/dragging, the default on
  double-click, fine adjust with Shift, type a value on click on the readout, right-click
  menu with "Automate" (creates an automation lane) and "Reset". Automated params get a
  marker (e.g. a colored ring) and show the automated value moving during playback.

## 3. The devices and their real parameters

Ranges are the real ones. Discrete params list their choices. Design only what exists; if you
think a param or a visualization would be a big win, propose it in a separate "Ideas" list
(we can add DSP later).

### Effects

**Parametric EQ** (`eq`) — 7 fixed bands:
- High-pass: freq 20–2000 Hz, slope off / 12 / 24 / 48 dB/oct.
- Low shelf: freq 20–1000 Hz, gain ±24 dB.
- Peak 1, Peak 2, Peak 3: freq 20–20000 Hz, gain ±24 dB, Q 0.1–18.
- High shelf: freq 1000–20000 Hz, gain ±24 dB.
- Low-pass: freq 1000–20000 Hz, slope off / 12 / 24 / 48 dB/oct.
- Output gain ±24 dB.
Wanted: log-frequency display (20 Hz–20 kHz), dB grid, the summed response curve, each band
as a colored draggable node (x = freq, y = gain, wheel = Q, click node = band panel with exact
values, slope picker for HP/LP), per-band curves faintly, live pre/post spectrum analyzer
behind, a piano-key or note-name hint for the hovered frequency. Bands that are "off"
(HP/LP slope off, gain 0) look dormant but grabbable.

**Compressor** (`compressor`) — threshold −60–0 dB, ratio 1–20, attack 0.1–200 ms, release
5–2000 ms, knee 0–24 dB, makeup −12–24 dB, mix 0–100 %. Has a **sidechain input** (key from
another track; show the source and allow choosing it).
Wanted: scrolling level history (input, output, gain reduction trace) like a waveform
over time, the transfer curve (input vs output, with knee) draggable for threshold/ratio, big
gain-reduction meter, attack/release knobs, makeup with an "auto" suggestion hint.

**Limiter** (`limiter`) — input gain 0–24 dB, ceiling −24–0 dB, release 1–1000 ms.
Wanted: scrolling waveform with gain reduction drawn on top, big GR and output meters,
ceiling line draggable, loudness readout (short-term LUFS, true peak) since mastering is a
core use (the app targets e.g. −14 LUFS / −1 dBTP).

**Reverb** (`reverb`) — size 0–100 %, decay 0.2–20 s, damping 0–100 %, pre-delay 0–250 ms,
width 0–100 %, low cut 20–1000 Hz, mix 0–100 %.
Wanted: a visual of the decay (an energy/time envelope: pre-delay gap, decay length, damping
as high-frequency falloff), a space/size visualization, low-cut shown on a small frequency
strip, live output level.

**Delay** (`delay`) — time 1 ms–max (free) or tempo sync (off, 1/32, 1/16T, 1/16, 1/8T, 1/8,
1/4T, 1/4 ... up to several bars: 12 sync steps), feedback 0–95 %, ping-pong on/off, high cut
500–20000 Hz, low cut 20–2000 Hz, mix 0–100 %.
Wanted: the echo taps drawn on a beat timeline (decaying, alternating L/R when ping-pong),
sync/free toggle with note-value picker, a filter band on the feedback path.

**Chorus** (`chorus`) — rate 0.05–5 Hz, depth 0–100 %, feedback 0–90 %, width 0–100 %, mix
0–100 %.
Wanted: an animated LFO / modulation visualization, stereo width view.

**Saturator** (`saturator`) — drive 0–36 dB, mode tape / tube / hard clip, tone −100–100 %,
output −24–12 dB, mix 0–100 %.
Wanted: the transfer/shaping curve per mode (drive bends it), a harmonics or waveform
before/after view, mode selector as three visual cards.

**Gain / Utility** (`gain`) — gain −60–24 dB, pan −100–100 %, width 0–200 %, invert left,
invert right.
Wanted: compact; stereo field / goniometer-style width view, phase invert toggles, big gain.

### Instruments

**Poly Synth** (`poly_synth`, subtractive):
- Osc 1 wave and Osc 2 wave: sine / saw / square / triangle; osc 2 semitones ±24, cents ±100;
  osc mix 0–100 %; unison 1–4 voices, unison detune 0–100 cents; noise 0–100 %.
- Filter: cutoff 20–20000 Hz, resonance 0–100 %, slope 12 / 24 dB, env amount ±60 st, key
  track 0–100 %.
- Amp ADSR: attack 0–10 s, decay 1 ms–10 s, sustain 0–100 %, release 1 ms–20 s.
- Filter ADSR: same ranges.
- Velocity sensitivity 0–100 %, glide 0–2000 ms, gain −60–12 dB.
Wanted: signal-flow layout (OSC 1 + OSC 2 + NOISE -> FILTER -> AMP), waveform pictures as
selectors, filter response curve draggable (cutoff/resonance), **draggable ADSR envelope
shapes**, a mini oscilloscope of the output, a small on-screen keyboard to audition.

**Electric Piano** (`electric_piano`) — brightness, bell, velocity sensitivity, tremolo depth
(0–100 %), decay 0.5–15 s, release 10 ms–3 s, tremolo rate 0.1–10 Hz, gain.
Wanted: a characterful, simple "instrument face" (few big controls), tremolo LFO visual,
bright/mellow tone hint.

**Drum Machine** (`drum_machine`, synthesized GM kit + percussion): per voice tune ±12 st,
decay, level (kick also punch; snare also snappy): kick, snare, clap (level), hats (tune,
closed decay, open decay, level), toms, cymbals (decay, level), percussion (tune, decay
25–400 %, level); global tone 0–100 %, gain.
Wanted: a pad/voice grid (one strip per voice with its own color), click a pad = audition,
selected voice shows its controls large, plus a tiny amplitude envelope picture.

**Drum Sampler** (`drum_sampler`, 16 pads, GM notes): global gain, velocity sensitivity,
polyphony 1–64, kit tune ±24 st. Per pad (sample zone): sample file, start/end, tune, gain,
pan, attack, decay, choke group 1–16, output (main or extra out 1–8), reverse, velocity
layers and round-robin. Drop audio files on pads.
Wanted: redesign of today's editor: 4x4 pads with readable names, waveform with start/end and
envelope overlay, pad controls clearly separated from kit controls, choke/output shown on
the pad, drag & drop states.

**Sampler** (`sampler`, multi-zone): zones mapped on a keyboard (key range, root key, velocity
range, tune, gain, pan, start/end, loop off/forward/ping-pong with loop start/end and
crossfade, reverse); globals: gain, polyphony 1–64, trigger gate / one-shot, amp ADSR, filter
off / LP / HP / BP with cutoff, resonance, env amount ±96 st, filter ADSR, filter velocity,
key track, velocity sensitivity, velocity curve, glide, legato, transpose ±48.
Wanted: zone map (keys on x, velocity on y) with draggable zone rectangles, waveform with
loop region and crossfade, ADSR shapes as in the Poly Synth, filter curve.

## 4. Deliverables

1. **Device component kit** (extends the shell design system): knob (sizes S/M/L, bipolar and
   unipolar, modulation/automation ring), value readout, toggle, segmented selector,
   wave/mode picker cards, ADSR editor, frequency display (grid, labels, analyzer, nodes),
   meter (vertical, GR), level history display, device header, preset bar, A/B, floating
   value tooltip, band/voice color palette (8+ colors that read on the dark display).
2. **High-fidelity screens**, dock size (1920 x ~400) unless noted:
   - EQ (dock + compact), with a band being dragged and its value bubble.
   - Compressor (dock + compact), with gain reduction happening and sidechain set.
   - Limiter, Reverb, Delay (sync on, ping-pong), Chorus, Saturator, Gain/Utility.
   - Poly Synth, Electric Piano, Drum Machine, Drum Sampler, Sampler.
   - One example of the AI having changed a device (highlight + Undo).
   - The EQ in a floating window over the arrange.
3. **Interaction spec** per device: what is draggable, wheel, modifiers, double-click,
   right-click, keyboard, what animates.
4. **Ideas** list (optional): features worth adding later (e.g. dynamic EQ bands, mid/side,
   match EQ against a reference — Cadenza already analyzes reference tracks; LFO modulation).

## 5. Technical constraints (same as the shell brief)

- Drawn with **egui** (immediate mode, wgpu): lines, polylines, filled polygons, rounded rects,
  circles, text, simple linear gradients (meshes), drop shadows on floating windows. Glows
  must be faked with a few wide semi-transparent strokes; no blur filters, no backdrop effects.
- Curves are polylines (fine: 1–2 px, anti-aliased); fills under curves are translucent
  polygons. Displays redraw at up to 60 fps: keep each one to a few hundred shapes.
- Live data (spectrum, gain reduction, levels, oscilloscope) comes from the audio engine at
  ~30–60 fps as arrays; design the visuals assuming that, and show them in their "idle"
  (transport stopped) state too.
- Fonts and icons as defined in the shell design system (open licenses).
- Layouts must be responsive between the compact width and the full dock width; tell me what
  collapses first.

## 6. Attached screenshots

`screens/devices/`: 01 the EQ today (18 sliders in the inspector), 02 the drum sampler today,
03 the sampler today.
