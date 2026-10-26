# cadenza-app — contract

Moved from CLAUDE.md (kept in sync with the code: update it when the contract changes).

Desktop app (`cadenza-app [project]`, or `cadenza gui [project]`, which execs the binary next to
the CLI so the CLI stays free of GPU/windowing deps). egui 0.36 / eframe with the wgpu renderer
(the crate sets `rust-version = 1.95`); native file dialogs through `rfd` (xdg-desktop-portal).

- `state::AppState` (no egui; unit-tested) holds the `cadenza_mcp::Session` as a
  `SharedSession` — the same document + undo history + path + plugin catalog + hosting the MCP
  tools and the in-app AI agent use — plus selection (ids only, pruned after edits), view
  (zoom/scroll/panels), notices, meters, jobs. Views read a snapshot (`DocView`: project +
  history labels; `state.document()`), never the lock; `state.session()` locks briefly.
- **Every edit is a `Command` through `AppState::apply`** (or `edit_session` to reuse MCP tool
  functions such as `import_audio`, `import_midi`, `save_project`): validated + undoable by
  `Document`, marks dirty and prunes the selection. All paths (and the agent's tool edits,
  picked up each frame by `sync_session(false)` with `try_lock`, so the UI never waits) end in
  `sync_session`, which refreshes the snapshot when the change key moved and runs that
  bookkeeping; saves by any owner clear the dirty flag. `state::live::diff(before, after)` decides
  how the player follows: volume/pan/send levels, device param values and automation lanes go
  live (`set_live_*`, `set_automation`, MIDI clip/note edits on instrument tracks via
  `set_track_clips`; sounding notes keep playing, also for undo/redo and shared MCP tools),
  anything structural schedules a debounced (120 ms) reload. Gesture edits
  (faders, knobs, send levels, clip moves, automation points) are previewed locally (and heard
  live) and committed as ONE command on release; value widgets only commit real user edits.
  Mixer/inspector controls show the automated value at the playhead. `state::edits` holds the
  pure gesture -> Command builders (move/resize/duplicate/split clips, left trim
  `trim_start` keeping content in place, rubber band `clips_in`, new tracks/clips, reorder).
- Automation (`state::automation` builders + `ui::automation`; rows laid out by
  `state::rows`): per-track and master lanes expand under their channel (A key, header "A"
  toggle, track menu "Automate"), lane picker (volume, pan, sends, every device param by
  name), click adds, drag moves (snap; Alt free; Shift vertical), rubber band (Shift adds),
  Ctrl+drag ramp, right-click/Del deletes, double-click cycles linear/hold/smooth, tooltip in
  real units. Right-click "Automate" on mixer faders/pans/sends and inspector controls creates
  and shows the lane. Tempo lane (transport ♩ toggle, pinned under the markers): step curve
  of the `TempoMap`, click adds a change, drag moves, right-click removes; the BPM field edits
  the segment under the playhead. Time signatures (phase 17, `state::meter` builders): the
  transport TIME field edits the signature in effect at the playhead; the signature lane
  (shown with the tempo lane, above it) has a chip per signature, drag a change to another bar,
  right-click a bar = presets / custom / remove. Ruler numbers, grids (`Timeline::grid_lines`:
  bar/beat/step lines restarted at every bar line), snapping (`snap_meter`), bars.beats
  display, pattern length, new clips, loop-the-bar, export bar ranges and MIDI clip growth all
  follow the changes; the ruler tags each change.
- Automation recording (phase 17, `state::autowrite`): transport "A: Read/Touch/Latch" button
  (click cycles, right-click picks; `AppState::set_write_mode`). While playing in Touch/Latch,
  `preview_volume/pan/send/param` and plugin editor values (`editor_updates`: Value = write,
  GestureEnd = release) write `(position, value)` samples per frame into a `Writer` pass and the
  `commit_*` calls release instead of committing a static value; Touch returns to the old
  curve (or static value) one tick after the release, Latch keeps writing the last value until
  stop. Lanes being written are suspended in the player (`sync_write_suspension`). When playback
  stops (or the mode goes to Read) the pass becomes ONE undo step "Write automation" (Batch of
  `SetAutomation`, each stretch replacing the points inside it, thinned by Ramer-Douglas-Peucker
  at 1 % of its range; a loop wrap starts a new stretch so the later pass wins). Nothing of
  this runs on the audio thread.
- Freeze / bounce / consolidate (phase 17, `state::bounce` builders, renders on a worker
  `jobs.bounce`, one undo step when done): track menu Freeze / Unfreeze (FRZ badge on the
  header), Bounce in place (new audio track "<name> (bounce)" below with the render from the
  bar of the first clip, same volume/pan/output/folder, original muted); clip menu
  "Consolidate selected clips" (two or more clips of one track: MIDI merged at once with notes
  and controller lanes, audio rendered without inserts into one clip). Renders go to
  `.cadenza/freeze/` (freeze) or `Audio/` (state folder before the first save). MCP:
  `freeze_track`, `unfreeze_track`; summaries show `frozen` and `freeze_stale`. Stale freezes
  (phase 19a, `model::freeze`): `Freeze.source` = `Project::freeze_source(track)` (FNV-1a of
  clips, instrument, inserts, their device lanes and tempo; file names only, fader/sends left
  out) taken when the render starts; `freeze_is_stale` compares it (no fingerprint = never
  stale). `DocView::freeze_stale` caches it per change; the FRZ badge (header + mixer strip,
  `arrange::freeze_badge`) turns amber "FRZ!" and a click refreezes; track menu "Refreeze".
- Folder tracks (phase 17, `state::folders` pure builders): "+ Folder" track button, track
  menu "Group into new folder" / "Take out of folder"; headers indent with a guide line per
  folder level, folders have a disclosure triangle (`view.collapsed`, `toggle_folder`) and a
  row that summarizes the clips inside; collapsed contents are hidden in the arrange
  (`Rows::hidden`) and the mixer (folder strips: fold toggle, VCA fader, M/S; no inserts,
  sends, output or pan). Dragging a header (`move_track_to` -> `folders::move_track`, one undo
  step) moves a folder with its contents and files the track: right under an open folder =
  into it, else into the folder of the row above. MCP: `set_track_folder`, create_track kind
  folder.
- Audio editor (`state::audio_edit` + `ui::audio_editor`; double-click an audio clip): shares
  the bottom editor dock with the piano roll (`view.audio_editor`; F7 toggles the dock).
  Source-file time axis, peaks when zoomed out and decoded samples (`PeakCache::samples`) when
  zoomed in; region start/end handles, fade handles + curve pickers, clip gain pill, selection
  with split / trim / delete range (clip commands). Normalize/reverse/gain (and time-stretch/
  pitch with the app's `stretch` feature) render a processed copy on a worker into
  `<project dir>/<project> audio/` (user cache when unsaved) and swap it in with one undoable
  `UpdateAudioClip` batch; the original file is never modified. Overlapping audio clips crossfade
  automatically (`edits::crossfades` mirrors the engine rule): the arrange draws what plays
  (clips by start, later on top; waveforms shaped by `edits::crossfade_gain`, the earlier clip
  fading out under the later one and cut at the crossfade end), the fade-out/fade-in curves and
  a length handle at the crossfade end (drag, snapped unless Alt; double-click resets to the whole overlap; one `SetClipFades`
  on the later clip, `edits::set_crossfade`); nested clips that sum keep the X mark.
- `audio::AudioEngine`: the `Player` on its own thread (like MCP `PlayerHandle`) but never
  blocking the UI: requests are fire-and-forget, a burst of loads is coalesced into the newest,
  results come back as polled `AudioEvent`s; position via `PlayerClock`, levels via `MeterBank`,
  plugin crashes polled every 500 ms and shown as toasts. No device = app works without sound.
- Dropouts (phase 19a, `state::xruns`): the player thread sends `AudioEvent::Xruns { device:
  Player::device_xruns() (cpal `Xrun` errors counted in the stream's error callback), plugins:
  plugin_xruns() }` when they change (500 ms poll); the transport shows "⚠ n" left of the audio
  status only when there are dropouts since the last reset (amber fill for 3 s after a new
  one), tooltip per source, click resets. `VirtualDevice::report_xrun` for tests.
- `jobs`: waveform peaks (decoded + `WaveformPeaks` on a worker, cached by path), plugin
  rescans, exports, file copies and file dialogs (files and folders) run off the UI thread.
- Project files (phase 14; `state::safety`, `state::export`, `state::project_io`,
  `ui::project_dialogs`): File menu New / Open (`.cdz` projects and `.cdzpack`
  archives, extracted into a chosen folder) / Open Recent (persisted, 10) / Start Screen / Save /
  Save As / Import / Export… (Ctrl+E) / Collect Project Files / Locate Missing Files / Export
  Project Archive / Autosave interval. Start screen (no project on the command line;
  `ui::start`, a full page under the menu bar, no workspace behind it): recovery banner
  (instead of the modal), "Describe it to the producer" (new project + chat request),
  "From a style" (new project + the style's first form via `apply_library_form`), Empty
  project, Demo song (bundled `examples/first_light`, written to the state folder), recent
  list with tempo/signature/style, track count, age and a clip thumbnail (`state::start`:
  summaries read on a worker, cached per path, invalidated on save), footer (F1, plugin
  count + Rescan, version). An empty project shows a centered "Add your first track" target
  (+ Instrument/Audio/Bus/Folder) and "Or ask the producer" (input + chips) in the arrange.
  Export dialog: range (whole song, loop region, selected clips, bars) + tail, mixdown, stems
  (every track, buses + direct tracks, chosen tracks; through the master, pre-fader), format
  (WAV 16/24/32f, FLAC 16/24, MP3 when built), dither, sample rate, normalization (peak or LUFS
  + true-peak ceiling, stems linked or individual), metadata, folder (default `Renders/`), file
  name preview; runs `export_project` on a worker with a progress bar and Cancel, then shows a
  per-file loudness table and "Open folder"; settings persist in `Prefs.export`. Missing files on
  open show the Locate dialog (folder search, one undo). "Save As" to a folder that lacks the
  project's external files offers to collect them. Processed audio copies go to `Audio/`.
  Safety (off in `AppState::headless`, on via `enable_safety(state_dir)`): autosave every
  `Prefs.autosave_minutes` (default 2, 0 = off) after the first unsaved change, written on a
  worker from a project clone to `.cadenza/autosave/` (unsaved projects:
  `$XDG_STATE_HOME/cadenza/autosave/untitled-<pid>/`), rotating 5; saves clear them, "Don't save"
  deletes them. A `.cadenza/session.lock` (pid) marks an open project, removed on new/open/quit;
  opening a project whose newest autosave is newer than the file offers Recover (unsaved
  document, same path) or keeping the saved version (reason: crashed when a dead pid's marker
  is left); untitled sessions of dead processes are offered at startup.
- `ui`: views only read state and call its methods; `theme` is the single palette/metrics
  module. Panels (`state::layout::PanelLayout`, FL Studio-like keys): F5 arrange, F6 / Alt+F8
  browser, F7 piano roll, F8 inspector, F9 mixer, F11 / Ctrl+Enter maximize the focused panel
  (last one clicked), F12 close tool panels, F1 shortcut overlay (`layout::SHORTCUTS` is the
  single shortcut table; View menu shows checkmarks + keys). The piano roll docks at the bottom
  (borrowing the mixer's place, given back on close); with the arrange hidden it takes the
  center. Layout, zooms, snap, follow mode, scale, bookmarks persist per user in eframe storage
  (`layout::Prefs`, key `cadenza-prefs`; panel sizes via egui memory).
  Responsive shell (design 12, pure `layout::Responsive::for_window(w, h, chat_pinned)` in
  `view.responsive`, set every frame): below 1600 px the browser is a 40 px icon rail (an icon /
  F6 opens it as an overlay, `PanelLayout.browser_overlay`, closed by Esc, a press on the
  workspace or a drop) and the inspector an overlay from the right; below 1800 px the AI panel is
  an overlay drawer unless pinned (`chat_pinned`, header Pin/Unpin); Esc closes the top overlay;
  floating windows stay above overlays and shrink/move into a smaller window. Docked side panels
  have min/max widths (`BROWSER/INSPECTOR/CHAT_WIDTH`, arrange keeps `ARRANGE_MIN_WIDTH`), right
  panels run full height, mixer 180 px-60 %, editor 200 px-70 % of the height. `TransportLayout`
  folds Click/Count-in/MIDI record/Punch/Automation/Follow into "Modes ▾" (dot = not default)
  below 1600, then drops captions/labels/time; `StatusLayout` shortens counts and chips; the
  mixer goes Narrow by itself when Normal strips do not fit (`MixerView::auto_narrow`).
- Floating windows (T37, FL Studio-like; pure registry `state::windows::Windows` in `view.windows`,
  views `ui::windows::FloatingUi`): built-in device editors (one `DeviceEditorUi` each), track
  properties (`open_track_window`: mixer strip double-click, track menu "Track properties…") and clip
  properties (`open_clip_window`: clip menu, Alt+double-click) float over the workspace, several at
  once (`WindowItem::{Device, Track, Clip}`): opening an open one focuses it, a click focuses (keys +
  probe go to the focused one; a press on a panel blurs), Esc / × close, Keep on top, Dock (devices),
  new ones cascade, clamped to the screen; rects remembered per item and per kind in
  `Prefs.windows` (`SavedWindows`). Track / clip windows draw `inspector::item_window` (the
  inspector's Track / Clip rows).
- Inspector (F8, optional: hidden by default, `ui::inspector` + pure `state::inspector`): header (colour chip, name, what it is) over a
  TRACK | CLIP | DEVICE tab strip (`InspectorView::follow`: the tab follows the selection's `Focus`
  until the user clicks another, which holds until the selection changes; `Targets`: track or master,
  one clip, selected device else the track's instrument; empty tabs dimmed with a hint). Track = name,
  16 colours, kind, instrument, input/monitor, output, volume/pan, M/S/arm, sends, inserts (click =
  Device tab, double-click = editor), delete; Clip = name, start/length typed in bars.beats.ticks
  (`parse_position`/`parse_length`), track, notes/pitch/velocity ranges or file/offset/gain/fades,
  a notes/waveform preview, open piano roll / audio editor, the instrument's compact form inline;
  Device = the compact bodies (or generic sliders + Edit for plugins). History is a fixed footer
  (numbered, AI steps badged, foldable, ~30 % of a short panel). One command per edit.
- Navigation (arrange and piano roll alike, math in `state::timeline`): wheel vertical,
  Shift+wheel horizontal, Ctrl+wheel zoom at the mouse, Alt+wheel track/key height,
  middle-drag pan (Space stays play/stop), themed scrollbars, song overview strip with a
  draggable viewport, Home/End, PageUp/Down (Shift: vertical), Z zoom to selection, Shift+Z fit,
  follow playhead Off/Page/Continuous, edge auto-scroll while dragging. Ruler/markers pinned,
  headers scroll with lanes. Clips: Ctrl+drag copies, Alt disables snap, double-click a MIDI
  clip opens the piano roll.
- Arrange toolbar + tools (redesign T20, pure `state::arrange_tool`): a toolbar row above the
  overview (title, Snap + grid, ↖ Select V | ✏ Draw D | ✂ Split C, Tempo lanes toggle, zoom);
  `view.arrange_tool` (persisted in `Prefs`). Select = the gestures above; Draw: click on an
  empty instrument lane = one-bar clip at the grid cell, drag = clip over the cells touched
  (`draw_span`, Alt free; ghost with position · length), clips still move; Split: hover shows
  the snapped cut line, click splits the clip (or the selection it belongs to,
  `split_targets`/`split_at`, one step). S stays split-at-playhead. Ruler: bar lines + numbers,
  beat ticks only when readable, loop = cyan bar with end brackets on the ruler top (tint +
  edges over the lanes), playhead flag cap; marker chips sized to the name (amber bar + tint).
- Piano roll (`state::piano` pure builders + `ui::piano_roll`), FL Studio parity (phase 13b):
  absolute song time axis with the clip span marked and outside shaded, keyboard (click
  plays, drag glides), tools Draw (P) / Paint (B) / Select (E) / Slice (C) / Bend (G: drag over a note draws its pitch curve, Shift whole semitones, double-click straightens). Draw does nearly
  everything: click on empty grid adds a note with the last length/velocity and keeps
  dragging it (pitch + snapped time, auditioned on press and pitch change); drag a note =
  move it (or the selection), its right/left edge = resize end/start; Shift+drag duplicates;
  Ctrl+drag rubber band, Ctrl+click toggles; Alt = no snap; right-click deletes the note,
  holding the right button deletes every note swept over (`sweep_hits`, one `RemoveNotes`);
  double-click = properties popover (pitch, velocity, start, length; `set_note`). Paint drags
  notes at every `paint_cell` (spacing = last length rounded to the snap); Slice splits
  notes at the snapped line (`slice_notes`). Snap is the piano roll's own `PianoSnap`
  ("magnet": Main follows the arrange snap; default Step = 1/16; persisted in `Prefs`).
  Look (redesign T21, design 04): toolbar = clip chip (colour, name, "on <track>" menu) + "+",
  tool chips with glyph + key (`arrange::toolbar_chip`, glyph only < 1320 px), Snap toggle +
  step menu, Key chip (root/scale), "Scale rows" (`PianoView::scale_rows`: in-scale rows
  lighter, root lighter still), Ghosts, Events, count, Quantize Q, Fit/−/+; one row per key
  (C labels, hovered/held key tinted), notes in the track colour by velocity with a release
  grip and names (flats when `piano::prefers_flats`), selected pale with a white outline,
  ghosts outlined; ruler and playhead flag shared with the arrange (`arrange::paint_playhead`);
  lane tabs row (`EventLane::tabs`: Velocity, Mod wheel, Pitch bend, Sustain, Expression +
  lanes with data, "+ CC" menu) over the lane, left column = name + scale or the value at the
  playhead (`controller_text`); velocity stems (white when selected), controllers violet.
  Keys (focused panel, consumed before global keys): Ctrl+B `clone_notes` (selection span
  widened to the snap grid, whole bars when >= a bar; copies selected so repeats append),
  Ctrl+C/X/V (`NoteClipboard`: pastes in place, or at the playhead when it was moved since
  the copy and the transport is stopped), Ctrl+A / Ctrl+D or Esc (deselect), Del, Ctrl+Q / Q
  quantize, Ctrl+U glue (selected same-pitch notes), Ctrl+L quick legato (so the global
  loop toggle is Ctrl+L from other panels), Up/Down semitone, Shift or Ctrl+Up/Down octave,
  Left/Right snap step, Shift+Left/Right bar — arrows/legato apply to all notes when nothing
  is selected (`targets`). Event lane (toolbar "Events" toggle; selector left of the lane, `PianoView::event_lane`:
  Velocity, Mod wheel, Expression, Sustain, Pitch bend, Pressure, CC 74, CC 7, any CC#, lanes
  with data marked ●). Velocity: drag paints (relative on a multi-selection), Alt/right-drag
  draws a line (`velocity_line`); Alt+wheel over a note (FL) moves the velocity of the
  selection it belongs to, else of that note (`wheel_targets`, `nudge_velocities`, 2 % per
  notch), previewed with a live tooltip and committed as one `UpdateNotes` when Alt is released
  or the wheel rests 0.6 s (over empty grid Alt+wheel stays key height; Ctrl+wheel stays
  zoom); a resting pointer on a note shows its name and velocity. Controllers (step graph, value tag under the pointer): pencil
  drag writes one event per snap cell (`controller_paint`), Alt/right-drag a ramp, right-click
  deletes the event under the pointer; switches snap on/off; one `SetControllerLane` per
  gesture (`set_controller`). Selected notes are warm red
  (`theme::NOTE_SELECTED`), brightness by velocity, names when zoom allows, dim ghost notes;
  off-screen notes are culled (2000 notes cost ~1 ms/frame). Opening a clip centers on its
  pitch range (C4 when empty). One command per gesture; editing past the clip end grows the
  clip to the next bar in the same batch (notes before the start are clamped). Notes are
  auditioned through `AppState::preview_note_on/off`, released on mouse up, panel close and
  track change.
- Transport modes (`state::transport`, view state, not in the project): FL-like PAT | SONG
  switch (L). Song mode plays the arrangement, looping the arrange loop region when on
  (Ctrl+L or the ⟲ button; drag the ruler's upper half or Ctrl+drag the ruler to set it, click
  the region to toggle; drawn on ruler and lanes). Pattern mode loops the clip open in the
  piano roll and auditions only its track (`Player::set_audition`); pattern length (FL):
  notes rounded up to the next bar (min one bar) or the clip length if longer
  (`transport::pattern_range`), so notes drawn past the end grow the loop live (no
  restart). Play starts at the pattern start unless the playhead was placed inside it since
  the last stop (`AppState::seek` cues, internal `locate` does not); Stop rewinds to the
  pattern start. No clip open = song with a toast. `AppState::sync_transport` (every tick) sends loop/audition only
  when they change; switching modes while playing restarts; audition only while playing.
- Piano roll access: F7 / double-click an instrument track header / track menu open the
  selected instrument track's clip (`piano::clip_for_track`: selected, under the playhead,
  last opened (`PianoView::recent`)) or create an empty one-bar clip named after the track at
  the first free bar from the playhead (`piano::new_clip`, one undo step). Adding an
  instrument selects its track and hints at F7; the piano roll toolbar has a clip menu (the
  track's clips) and "+" (new clip); the empty dock explains how to open one.
- Drag & drop (`state::dnd`: `Payload` x `DropTarget` -> `DropAction`, executed by
  `AppState::drop_payload`; views only hit-test and show the hint next to the pointer):
  devices -> track/lane (instrument or insert), mixer insert slot, below the tracks (new
  instrument track / bus); presets -> device or track; mixer inserts reorder / move to another
  strip (Ctrl copies); browser Files tab (bookmarks, audio/MIDI filter) and OS file drops:
  audio/MIDI import at the drop beat/track (new tracks below), `.cdz` opens (dirty
  prompt). Other shortcuts: Space, Home, Ctrl+Z/Shift+Z, Ctrl+S/Shift+S/O/N/I, Ctrl+E (export dialog), Ctrl+D, S
  split, Del (automation points first, then clips), A lanes; arrange: drag empty lane =
  rubber band, drag a clip's left edge = trim start. Dirty state is confirmed on new/open/close.
- Playlist editing (FL, T38; pure `state::playlist`, one undo step per gesture): arrange tools
  Select V / Draw D / Paint B / Split C / Mute T. Ctrl+C/X/V clips (`ClipClipboard`: relative
  tracks kept; paste at the playhead if it moved since the copy, else right after the copied
  span, appending; onto the track under the pointer), Ctrl+B clone after itself (grid / whole
  bars, copies selected), Ctrl+G glue (= consolidate), Q quantize clip starts, Ctrl+click a
  header / track menu selects its clips. Paint: drag repeats the pressed / selected / last
  clip (`view.paint_source`) on the template's own grid, skipping occupied cells; Split drag =
  knife cutting every track it crosses at the line's (snapped) x ("Slice clips"); right-drag in
  Draw/Paint deletes the clips swept over; Mute click/sweep toggles (muted clips dim + hatched,
  clip menu Mute/Unmute). Track height: drag a header's bottom edge (per track,
  `view.track_heights`, `Prefs.track_heights`), double-click / track menu resets, Alt+wheel
  scales all. T39: copies of MIDI clips (Ctrl+V / Ctrl+B / Ctrl+D / Paint / Ctrl+drag) are
  linked by default (`playlist::Copier`: the source joins a new link in the same step; the
  clipboard assigns a link so pastes of a cut clip share one), Shift = independent; linked
  clips draw a chain glyph, a selected copy outlines its siblings, the piano roll chip / clip
  window say "linked ×N"; clip menu / inspector "Make unique" (`playlist::make_unique`),
  "Chop into…" 2/4/8/16 parts / every beat / bar (`chop`, scratch-document splits),
  Shift+drag a MIDI clip's right edge = `stretch` (notes + controller ticks scaled; the copy
  becomes unique), a plain grow of a MIDI clip whose content ends inside it repeats the
  content (`resize_repeating`: period = content rounded up to bars, at most the old length;
  repeats are materialized as notes/points with fresh ids, so the engine and piano roll need
  nothing new). Splitting / left-trimming / stretching a linked copy unlinks it first.
- Sampler editors (`state::samples` pure builders; `ui::instrument_editor` keeps the drop hit areas): the device
  editor's body for `sampler` / `drum_sampler` (inspector "Edit pads/zones", double-click on
  the instrument in a track header or mixer strip). Drum sampler (redesign 17d,
  `ui::devices::drum_sampler` + pure `state::drum_sampler_editor`; `InstrumentEditorUi`
  keeps its `PadHits` for drops): KIT · ALL PADS column (gain, velocity, polyphony, kit
  tune; "Map folder…" / a folder dropped on the kit or a pad (OS or browser Files folder
  rows, `DropTarget::Kit`, `DropAction::MapKit`) = `map_kit` by file / folder names
  (`kit_slot` keywords; kick/snare overflow to Kick 2/Snare 2; loaded pads keep their
  settings) as ONE step "Map kit from folder", unplaced files named in a notice); 4x4 pads
  (pad 1 bottom-left; name, C1 = 36 note, mini waveform in the pad colour, tags C1 choke /
  →2 output / L3 RR3, "Empty · Drop sample", a dragged file shows a blue dashed "Drop to
  replace"; click auditions + selects, higher = harder; drag a pad onto another = swap,
  Alt = copy (`move_pad`, one step); right-click choke group / output / reverse / replace /
  add layer / clear / locate file (browser Files tab); double-click an empty pad = file
  dialog; keys 1–8, Shift+1–8 = pads 9–16 while focused; pads flash from the processor's
  `readings::DRUM_PAD_TRIGGERS_START` counters); selected pad: file menu, layer chips
  (velocity split or round-robin, `set_round_robin` / `add_layer` / `remove_layer`), ⇆
  Reverse, waveform (decoded samples when zoomed in) with start/end flags per layer
  (double-click resets; wheel zooms, Shift scrolls) under the A/D envelope the processor
  plays (draggable nodes), tune / gain / pan / attack / decay knobs, choke / output pickers;
  every gesture one `SetSampleZones` step. <1080 px stacked, <700 px and the inspector the
  compact form (small pads + pad knobs). Sampler (redesign 17e, `ui::devices::sampler` +
  pure `state::sampler_editor`): ZONE MAP (x = key with a piano strip that auditions, y =
  velocity with a labelled axis; `MapWindow` wheel zooms keys, Shift velocity): zones are
  boxes coloured per key range ("C3–B3 / 80–127", root dot); drag the body = keys +
  velocity (root follows), edges resize, the dot ⇔ = root (`drag_zone`, one step each);
  double-click an empty spot = a zone of the last loaded file (`InstrumentView.last_sample`,
  else the selected zone's) filling it (`new_zone` / `fit`), double-click a zone =
  `fit_to_neighbours`; right-click auto-map by file name (`auto_map`), split velocity,
  duplicate, fit, round-robin group, locate, delete; arrows nudge the selected zone while
  focused; preview notes light the map and keys; a file dropped on an empty spot adds a
  zone (`DropTarget::ZoneArea`, `SampleSlot::Area`). Selected zone: name + range, Off /
  Forward / Ping-pong, ⇆ Rev, waveform with start / end / loop flags and the crossfade ramp
  foot (`effective_crossfade`: forward only, as the processor clamps it), "Loop 1.42 – 2.36
  s · xfade 120 ms", root key readout (note names), tune / gain / pan / crossfade knobs.
  GLOBAL · ALL ZONES: amp ADSR, filter Off/LP/HP/BP + `FreqDisplay` with
  `sampler_filter_response_db` (node ⇔ cutoff ⇕ resonance, wheel env amount), cutoff /
  reso / env / filter velocity knobs or the "▸ Filter envelope" ADSR; bottom row trigger
  Gate | One-shot, polyphony / transpose / glide readouts (type or drag ⇕), legato,
  velocity + curve presets, key track. <1360 px map above zone | globals, <560 px stacked
  (both scroll); the inspector shows the compact form (small map + zone knobs). Audio file drops (browser Files or OS) on a pad / zone / the
  keyboard go through the shared `load_sample` tool; Alt+drop below the tracks or the Files
  context menu "Load into new sampler" adds a sampler track auto-mapped to the file.
- Style library (phase 18a; `state::library` pure builders + `ui::library`): browser
  "Library" tab (`AppState::library()` = the session's `Library`; `headless()` uses the
  core pack only; `new()` loads user packs and warns about broken ones via
  `report_library`). Design 08 (T22): while the tab is open the browser is its own wide
  panel ("browser-library", 70 % of the window, max window - 320 px); >= 640 px it is three
  columns (`ui::library::column` clips each): facet chips (genre most used first, 10 + "+ n
  more"; role; section; energy 1-5; Clear), packs + "Credits and licenses" | search box +
  kind tabs with counts (`kind_counts`) + "n results · sorted by fit" + a card grid
  (200 px min, culled off-screen) | the open document beside (>= 980 px, else in place of
  the results, × "Close document"); < 640 px it stacks (filter combos, one card per row,
  documents replace the list, "‹ Library" back). Cards: kind tag, `pack · license`, name,
  `card_subtitle`, a picture (`mini_picture`: the pattern realized over Am F C G, drums on
  rows, cached in egui memory; groove timing dots; progression numerals; form/style section
  strips in section colours), `card_meta`, `card_energy` dots, playing state (green ring,
  ■, playhead from `pattern_preview_progress`); tooltip = summary, facts, step grids.
  Click a pattern = audition it once at the song tempo with the default
  harmony (`default_harmony`: the song style's first loop, else i-VI-III-VII; A minor /
  C major) on a fitting instrument track (`preview_track`) through `preview_note_on/off`
  (`PatternPreview` events sent by `tick`; refused while playing; click again stops);
  double-click writes it at the playhead; drag = `Payload::Pattern` -> `DropAction::
  AddPattern` on an instrument lane/header (from the bar under the pointer, ghost clip
  shown) or below the tracks (new Drums/Bass/Keys/Pad/Lead track with a fitting built-in
  and preset): `library::add_pattern` = one Batch "Add pattern '<name>'" via the MCP
  `build_pattern_clip` (4 bars minimum, `drop_bars`). Styles, forms, progressions and
  grooves open as read-only documents: style = "Start a song in this style" (first form)
  / "Ask producer…" (prefills the chat), Feel (tempo vs the project's), Harmony numeral
  cells, Forms with Apply + section strip (`form_length`), Roles as role-coloured
  draggable/previewable pattern chips (`role_rows`), Sounds, Mix target tiles, Do / Don't
  columns; form = Apply form + strip + section table (bars, energy blocks, roles).
  "Credits" lists every pack (license tag, attribution, links, not-redistributable flag,
  location) and load problems.
- Mix assistant (phase 18b; `state::mix` logic + `ui::mix_assistant`): the AI panel (F10)
  has PRODUCER | MIX tabs (`mix.open`; AI menu "Mix assistant"). Analyze = `run_analysis` on a
  `cadenza-mix-analysis` thread (progress, Cancel, waker), report card: master LUFS vs target
  (style or override), true peak/crest/width/low-end mono, octave spectrum bars, tone vs the
  profile curve, per-channel loudness bars + peak tick, masking pairs, issues with Fix (the
  issue's moves via `mix_pass_command`, one undo step "Mix fix: ...") and Fix all ("Mix
  fixes"); a stale report (document changed) offers Analyze again. "AI mix pass": measure
  (reuses a fresh report), `AiChat::send_mix_pass` -> `SessionAgent::run_engineer_pass`
  (reference track via File dialog `FileAction::MixReference`, notes), measure again,
  `compare_passes` table with Keep / Revert (undo, only while "AI mix pass" is the last step).
  Report order: Master, Issues (Fix all on the header), Tone, Channels, Masking; each folds
  (`MixSection`, `mix.folded`; header summary while folded). Until the user folds anything,
  panels shorter than `MixSection::COMPACT_HEIGHT` fold tone/channels/masking. Look (design 06,
  T23; pure helpers in `state::mix`): range/"analyzed n s ago" bar with the Target menu
  (style, `TARGET_PRESETS`, custom) + Analyze (progress bar + Cancel while measuring, the old
  report dimmed); empty state = one big Analyze; Master = big LUFS readout, peak/crest/width/
  lows-mono tiles and a −30..−6 loudness bar with the target tick; stale bar "Mix changed since
  this analysis · Re-analyze"; issue cards (severity glyph, `issue_parts` headline + detail,
  track chip = `open_track_window`, move chips, Fix / ✔ Fixed); Tone = smooth octave spectrum
  over the dashed `tone_target` (gap per band = `tone_deviation`), worst band shaded; Channels =
  LUFS bars with the role's suggested band (`channel_rows`, median anchor ± `BAND_HALF_DB`),
  peak tick, ok / +n; Masking = both colours, `octave_range`, 5-step `masking_level`. Pinned
  footer: reference, brief (`mix.notes`), budget, "AI mix pass" (running: spinner + Stop); a
  finished pass shows its own review page (summary = last reply, Keep / Revert, `review_rows`
  before → after against the targets).
  Library style documents show the mix profile and role levels.
- Mixer (`ui::mixer`, options in `state::mixer::MixerView`, persisted in `Prefs.mixer`):
  toolbar (Normal 124 px | Narrow 76 px strips, Input/Inserts/Sends/Routing rows on/off,
  + Bus), a left gutter naming the rows once plus the fader's dB scale (every strip shares
  one `StripLayout`), strips typed by look (tracks raised, buses cooler after a vertical
  BUSES divider or a dashed "+ Bus" ghost strip, folders as VCA, master accent-tinted and
  wider; the toolbar folds by measured label widths, `mixer_fold`: rows into a "Rows ▾"
  menu, then the layout / meter choices too), input row, inserts with status dot / struck-through bypass, sends as mini-faders
  (PRE tag), output menu with "New bus from this track" (`mixer::new_bus_from`, one undo
  step), pan knob (`widgets::pan_knob`), long fader with a light cap. T26: output menu lists
  sidechain targets (`mixer::sidechain_targets`: devices on other strips/master that take a key
  and stay cycle free, checked when keyed by this track; `toggle_sidechain` = one
  `SetSidechain`); Peak | Peak+RMS toolbar switch (`MixerView.meter: MeterMode`, `meter_bars`);
  the master strip shows LOUDNESS S/I/TP (`mixer::MasterLoudness` = `LiveLoudness` fed from the
  player's master tap via `AudioEvent::Ready.master`, silence while stopped, click resets) in
  its sends/output rows and Mono / Dim −20 toggles (`AppState::set_monitor_fx`, monitoring only,
  kept across device reopens).
- Presets: browser Presets list grouped by category (collapsible, search box, loaded one in the
  accent; `load_factory_preset` goes through MCP `load_preset`), editor preset menu one submenu per
  category (`device::preset_groups`: built-in order, plugin banks after); `PresetEntry.category`.
- Look: the Graphite design system (`docs/design/redesign-brief`; tokens in `theme`: 7 surfaces,
  text/accent/semantic `*_SOFT` states, meters, 16 `TRACK_COLORS`, clip tint alphas, radii,
  metrics). Fonts: IBM Plex Sans/Mono (OFL-1.1, `crates/app/assets/fonts`, embedded) with one
  egui family per weight (`theme::medium/semibold/mono_medium`, `heading()`), egui's faces as
  fallbacks and Hack last in proportional (arrows, shapes, blocks: `← → ◆ ● ⊘ ▍`);
  `theme::applied(ctx)` re-themes fresh contexts (the first pass is discarded so the weight
  families exist); `proportional_fonts_cover_the_app_symbols` checks every non-ASCII symbol.
- i18n (T40, `crate::i18n`): gettext-style, the English text is the key. `tr("Save")` ->
  `&'static str`, `trf("{n} undo steps", &[("n", &n)])` (named placeholders), `trn("{n} track",
  "{n} tracks", n)` / `trn_with` (plurals, CLDR order per language), `mark("..")` in const
  tables + `tr_str(s)` where they are drawn (also strings from other crates: DSP param names,
  roles, profiles, pass rows listed in `i18n/external.rs`, synced with the registry by a
  test). `locales/<code>.toml` (`"English" = "traducción"` or `["one", "other"]`) is compiled
  by `build.rs` into sorted static tables: a lookup is a binary search, no allocation; a
  missing entry falls back to English. `every_language_file_matches_the_sources` scans the
  sources for every `tr/trf/trn/trn_with/mark` literal: each must be in every file, no unused
  keys, placeholders equal. Add a string: wrap it, add the key + translation to every
  `locales/*.toml`. Add a language: a `Language` variant (code, native name, plural rule,
  decimal comma, table) + `locales/<code>.toml`. Language: View > Language (System default +
  languages, `Prefs.language`, live); the app sets the process language
  (`set_global`, OS locale via LC_ALL/LC_MESSAGES/LANG then `sys-locale`), headless states only
  this thread's (`set_thread`, so parallel tests stay English). Numbers: `i18n::decimal/num`
  (decimal comma in es/pt-BR/fr; `format_value`, `format_db`), typed values accept ',' (`delocalize`
  in `parse_value`). Not translated: project data, library pack text, AI tool/context texts,
  undo labels built in `state` (model/AI vocabulary; some device-editor gesture labels are
  translated), logs, `ParamInfo.description`.
- Languages (T40/T41): English (source), Español, Português (Brasil) (`locales/pt-BR.toml`;
  `Language::code()` stays the BCP 47 primary subtag `"pt"` for OS-locale matching and
  `LanguageChoice` round-tripping even though the file carries a region suffix — `build.rs`
  turns a hyphenated file stem into a valid Rust identifier for the generated static table),
  Français, Deutsch, Italiano, Русский, 日本語 (`ja`), 简体中文 (`locales/zh-CN.toml`; `code()`
  `"zh"`, matches `zh_CN`/`zh_Hans`/`zh_Hant` OS locales — only Simplified is shipped), 한국어
  (`ko`) — ten languages total. French and Portuguese use the CLDR "one at 0 or 1" plural rule
  (`Language::plural_index`), German and Italian the English/Spanish "one at 1 only" rule,
  Russian its three-form "one/few/many" cardinal rule on the last two digits (ends in 1 but not
  11 = one, ends in 2-4 but not 12-14 = few, else many; `Language::plural_forms()` returns 3 for
  Russian), and Japanese/Chinese/Korean a single CLDR cardinal form ("other" for every `n`,
  `plural_forms()` returns 1, `locales/{ja,zh-CN,ko}.toml` plural keys are one-element arrays).
  The seven European languages use a decimal comma; Japanese, Chinese and Korean use a decimal
  point like English. Script coverage: Cyrillic is already in the embedded IBM Plex Sans
  (checked by `proportional_fonts_cover_the_app_symbols`, which also scans every translation
  file for glyphs, splitting out CJK characters — ideographs, kana, hangul, `theme::is_cjk` —
  into their own check), so `ru` needs no font work; CJK cannot be embedded (binary size), so
  `Language::needs_cjk_fallback()` (true for ja/zh-CN/ko) gates `theme::fonts_for_language`/
  `find_cjk_font`, which searches common system paths (Noto Sans CJK, Source Han Sans, Droid
  Sans Fallback, WenQuanYi on Linux; PingFang/Hiragino on macOS; Microsoft YaHei/Malgun
  Gothic/Yu Gothic/MS Gothic on Windows) and appends a found face to **every** font family UI
  text draws with — `Proportional`, `Monospace` and the named `medium`/`semibold`/`mono_medium`
  families `fonts()` builds per weight (egui has no font weights) — not just `Proportional`:
  missing any of them showed as empty boxes in buttons/tabs, panel titles or the position
  counter even with a CJK font installed. No face found = a clear notice (`AppState.notices`),
  not empty boxes (verified: `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc`, the first
  `CJK_FONT_CANDIDATES` entry, is present on the reference dev machine and renders every CJK
  string correctly, menu bar to mixer strips, both when a CJK language is active and when
  another language's View > Language picker lists their native names). `theme::needs_apply`/
  `apply` re-theme a context whenever the active language's CJK need changed (not just on first
  use), so switching into/out of a CJK language mid-session picks up or drops the fallback
  font — the font is per-active-language, so another language's native name in the picker
  (e.g. 日本語 shown while using English) still boxes until that language is actually selected;
  a future pass could load the CJK fallback unconditionally now that the catalog always
  includes one. Arrange/piano-roll toolbars fold their tool chips (key dropped, then the name,
  glyph only) from the width actually left for them (`arrange::chip_width`/`arrange_fold`,
  `piano_roll::piano_fold`; pure text measurement, never by drawing a probe widget twice — that
  briefly duplicated accessibility nodes and broke `egui_kittest` queries), and the arrange
  toolbar moves Sel/Fit into a "⋯" menu when even glyph-only tools don't leave room (found via
  the German 1024 px shot: "Tempo" drawn under "Einpassen", "Sel" cut off). The master strip's
  Mono toggle shows the fixed, untranslated "M" in narrow strips instead of an abbreviated
  translated word (was "Mo", which doesn't track every language's "Mono" translation or fit a
  CJK one).
- MIDI learn (T36, pure `state::midi_learn` + `ui::midi_learn`): every kit control menu and the
  mixer/inspector `automate_item` menus get "MIDI learn" / "Cancel MIDI learn" / "Forget MIDI (CC 74)";
  learning pulses an amber ring on the control (`theme::LEARN`) with a bottom hint (Esc cancels) and
  takes the next controller the monitor reports (`learned_map`: replaces the target's mapping, takes
  the CC from another target; range = the param's / -60..+6 dB / full pan) = one step "MIDI learn CC
  74"; mapped controls show a "CC 74" badge. `tick_midi_learn` mirrors the values controllers set
  (`mirror_value`: previews + autowrite) and commits each after 400 ms rest (`commit_address`).
  File > "MIDI Mappings…" window: min/max, mode, takeover, × (one `SetMidiMap` per edit).
  `live::diff` sends map changes live (`AudioEngine::midi_map`); `AudioEvent::Ready.midi_map`.
- Explain this device (T36): editor header "Explain" (next to Ask AI) and the control menu
  "Explain this device" -> `AppState::explain_device` -> `AiChat::send_explain` ->
  `SessionAgent::run_explain(prompt::explain_request(device))`: read only (`EXPLAIN_TOOLS`; other
  tool calls are refused with an actionable message, the undo group is aborted so nothing stays);
  prompts carry an "# Explain" section.
- AI reply language: the producer answers in the UI's language by default. `AiSettings.
  reply_language: ReplyLanguage` (UI language | English | a typed-in language, `Prefs.ai`,
  settings dialog "Reply language") resolves (`resolve_name`) to `AppContext.reply_language`
  (`None` = English, no hint), rendered as an extra "Reply in: <language>" line in the request
  context (`cadenza_agent::prompt::AppContext::render`); `SESSION_SYSTEM_PROMPT` tells the model
  to use that line and keep tool names/parameters/ids/code in English regardless. Tests use the
  scripted provider (no network).
- Modulation (T33, `ui::devices::modulation` + pure `state::modulation`): a MOD strip under every
  built-in editor body (collapsed header "▸ MOD" + "+ LFO" / "+ Envelope"; open when the device has
  modulators): cards with a draggable handle (egui dnd `ModDrag`), live value bar, shape cards,
  Free/Note/Song retrigger, Sync + step combo or Rate knob, Phase; envelope Input/Key, Attack/
  Release/Gain; target chips (Alt+click removes). `ParamKnob` draws the modulation ring
  (`theme::MOD` teal, outermost; range from `modulation::extent`, live dot from
  `AppState::modulation_values`), a handle dropped on a knob adds a target (depth 0.25), dragging
  the ring sets the selected modulator's depth (200 px = 1), Alt+click on the ring removes it;
  control menu "Modulate with ▸" (toggle per modulator, New LFO / New envelope). One
  `SetModulators` per gesture; depth drags preview only the ring (heard after release).
- Device kit (redesign D0, `ui::device_kit` + pure `state::device`: `parse_value` with units/
  shorthand/note names, `format_value`, `KnobSpec`): display surface `theme::DISPLAY_BG`, 8
  `BAND_COLORS`, automation violet / GR coral / L-R teal-orange, glow strokes; `Knob` S/M/L/XL
  (uni/bipolar, automation + AI rings, floating value, 200 px drag, wheel), `param_knob`/
  `param_toggle` (read the automated value, one `SetParam` per gesture, `control_menu`:
  Automate/Ask AI about this (`device_ai::ask_about`: opens the editor's Ask AI popover
  prefilled "<param> (now <value>): ")/Reset/Copy/Paste; it also draws the 300 ms hover
  readout `floating_tip` = value + `ParamInfo.description` (`ControlInfo.tip`; knobs draw
  their own via `Knob::description`) and remembers the param touched last, `touched`),
  typed `readout` (eases 80 ms on outside changes like the knob, never while dragging /
  automated), toggle, segmented (`segmented_with`: font + disabled segments, used by the
  export dialog), grid selector, mode cards, level/GR/IO meters with peak hold;
  `widgets::checkbox` = egui's checkbox with a visible Graphite box (every dialog uses it);
  displays (pure math in `state::device_display`): `adsr` (cube-root time axis, per-stage
  `Gesture`s, D = decay + sustain, voice dots), `FreqDisplay` builder (log 20 Hz-20 kHz, ±12/±24/
  filter dB ranges, sum glow + band fills, pre/post analyzer from `spectrum_bins` (FFT, 1/6 oct,
  `AnalyzerState` 12 dB/s fall), nodes -> `FreqEvent`s, piano strip + note hint, one mesh per
  fill), `LevelHistory` (min/max columns from probe frames) + `LevelHistoryView` (input fill,
  output line, GR from the top, draggable threshold, `stopped` dims); `gallery` is rendered by
  `render_device_kit`.
- Device editors (redesign D2, `ui::device_editor` + `state::device`): every built-in opens in
  its floating window (`open_device_editor`; T37) from a mixer insert click, the track header's
  instrument name double-click, the inspector "Edit"; `open_device_in_dock` / a window's Dock
  shows it in the bottom editor dock instead (`view.device_editor`, device in
  `view.instrument.device`; the dock's pop-out button floats it again); `toggle_device_editor`
  focuses / closes; `device_editor_target` = focused window, else dock, else topmost window;
  plugins keep their windows. Shared
  header: power (`SetDeviceBypass`), name, track chip, preset ‹ › ▾ over factory + user
  presets (MCP `list_presets` / `load_preset`, one undo step; amber dot = `preset_status`
  dirty), Save (MCP `save_preset`, name prompt), IN/OUT `io_meters` from the probe
  (`IoLevels`), Mix slider when the device has `mix`, close; below 800 px the compact
  two-row header (Save in ⋯, 34 px meters, Mix in the body). The probe follows the open
  editor (`sync_editor_probe`). Bodies are functions dispatched by device id in
  `DeviceEditorUi::body` (`BodyCx`: ref, info, rect, drained probe frames, compact); the
  default `generic_body` draws every param (`param_groups`, `param_control`: knobs M,
  segmented / grid choices, toggles) in packed rows on a display panel.
  Header extras (D2b, `state::device_ab`): A | B compare per device (view state; the active
  slot is the live params, switching = one Batch "Compare B" of `SetDeviceState { params }`,
  undo/redo flips the slot back; A→B copies; A key while the editor has focus, Shift+A on the
  Poly Synth / Electric Piano whose A–K play notes). Editor keys (T29, `DeviceEditorUi::
  shortcuts`, listed in F1): B bypass; ← → select band (EQ) / pad / zone / voice, ↑ ↓ nudge
  its value (band gain or filter freq, pad layer / zone gain, voice level), elsewhere the
  control touched last else `device::main_param` (`device::nudge`: 0.5 dB, Shift 0.1; one
  step on whole numbers; else 1 % of the travel); Sampler Alt+arrows move the zone
  (`device_editor::arrows` consumes them). Undo hint
  "Ctrl+Z <step>" when the last step touched the device (`StepTouches`; both only >= 1600 px);
  pop-out: the dock's device moves to its floating window, the dock goes back to the piano roll.
  EQ (`ui::devices::eq` + pure `state::eq_editor`): `FreqDisplay` with exact `EqBands` curves,
  pre/post analyzer, 7 nodes (drag/curve-drag = one Batch "Peak 2 freq and gain" via
  `AppState::commit_params`; wheel Q/slope, double-click reset/wake, Ctrl gain only), chips,
  band readouts, Range; compact form also replaces the inspector's sliders; keys 1-7 / Del; a soloed band (Listen) dims the other bands' fills.
  Dynamic bands (T30, `eq_editor::{toggle_dynamic, live_offsets, activity}`): band row "Dynamic" toggle (+ Thr/Ratio,
  Att/Rel when room; compact: Thr/Ratio knobs), sum = live curve (band gain readings) over the static ghost, node with a
  dashed ring, 0 dB → gain range bar, live marker and a GR halo by activity (`FreqNode::dynamic`, `FreqDisplay::ghost`).
  Mid/side + match EQ (T31): band row Stereo | Mid | Side segmented (compact: a "Channel" row with Match…;
  `eq_editor::{CHANNELS, channel, set_channel, badge, uses_mid_side}`, one step "<band> channel"); M/S node badges
  (`FreqDisplay::badges`); with an awake mid/side band the display draws the mid sum (`theme::EQ_MID` = text) and the
  side sum (`theme::EQ_SIDE`, pale amber) tagged M / S (`sum2`, `sum_tags`); footer "Side" toggle = analyzer on
  (L-R)/2 (`SampleWindow::push_channel`). "Match…" (band row right / compact Channel row): Audio file… (file dialog
  `FileAction::MatchReference`) or another track -> `state::match_eq` (`AppState::start_match_eq` on a
  `cadenza-match-eq` worker, progress, cancel; `match_eq_proposal` refits live from amount / max gain;
  `apply_match_eq` = one `commit_params` step "Match EQ to <ref>"); while ready the display shows the EQ input (pre fill)
  and the level-matched reference spectrum (`FreqDisplay::reference`, `display_spectra`) and the proposal dashed in the
  accent (`FreqDisplay::proposal`); a card over the display: Amount %, Max dB, "before → after dB", Apply / Cancel.
  Collision view (T32, `state::collisions` + `ui::devices::collisions`): footer "Collisions" toggle (disabled on
  master/bus EQs) shows the Mix assistant's report (fresh one reused, else `mix_analyze` on its worker, progress chip;
  stale chip "Analyze again"): `analysis::issues::track_collisions` (-> `masking::collisions_for`: same rule as `masking`
  but per (other, octave band), `center_hz` = loudest shared third-octave, `cut_db` = `cut_gain_db`, `cut_here`) drawn as
  red columns + a heat strip (`FreqDisplay::heat`, `HeatBand`, relative to the worst band), this track's spectrum as the
  pre fill and the top 3 others as thin lines in track colours (`FreqDisplay::spectra`, legend), the strongest pair
  labelled ("Pad × Keys 177–354 Hz"); strip hover = both levels per track, click (or the label) = `suggest_collision_cut`
  (`cut_changes`: near / free / least-used peak, Q 1.4, half the cut when the rule says the other track should move)
  dashed as the proposal + card Apply (one step "Cut '<own>' under '<other>' at <hz>") / Cancel. Offline only (no live
  probe version).
  Compressor (`ui::devices::compressor` + pure `state::compressor_editor`): level history (key trace from
  the key track's meter, threshold tag drag), transfer curve (dot = threshold, above the knee = ratio, wheel = knee,
  double-click -24/4:1), GR meter, auto makeup (GR at -18 dBFS) chip, key picker (`SetSidechain`); compact in the inspector (with "Listen" to the key, T29).
  Key trace = the compressor's `KEY_LEVEL_DB` reading; footer "Listen to key" (headphones `device_kit::glyph_toggle`).
  KEY FILTER strip under the history (dock, compact): `FreqDisplay` of the key filter, HP/LP x-only nodes
  (`compressor_editor::{KeyFilter, drag_key, key_off}`: dragged to the end = off, double-click = off).
  Listen modes (`AppState::set_listen/listen_mode`, `Request::Listen`) are view-only, follow the probed editor device and
  are cleared when its editor closes. EQ: "Listen" on the selected band (dock + compact) or Alt+click a node
  (`eq_editor::toggle_listen`; the solo follows the selection, `listen_follow`; listened chip ringed in SOLO yellow).
  AI on the device (D2c, `ui::device_ai` + pure `state::ai_changes`): header "Ask AI" (before pop-out) opens a
  prompt popover (Enter sends, per-device suggestion chips) -> `ask_ai_about_device` = `send_ai("<text> (<Device> on
  '<Track>')")` + chat opened; `AppContext.open_device` names the editor's device. Every request snapshots built-in
  params; when it ends `AiChanges { label, request, depth, devices: {param, before, after} }` (view state): `ParamKnob`
  draws the blue ring + delta tag ("+3.0 dB") by itself, toggles/readouts (`ReadoutState::Ai`)/EQ nodes (`FreqNode::ai`)
  turn blue, EQ draws the before response dashed; card "AI changed N params" with Keep, Undo (only while it is the
  last step) and Hold to hear before (`hear_before`: live params only, never a command). Cleared by Keep, undoing the
  step, a new request; a commit / later step on a changed param drops it.
  Limiter (`ui::devices::limiter` + pure `state::limiter_editor`): waveform history (`LevelHistoryView::waveform`, ceiling line on both halves, drag = input gain, wheel = release), IN/GR/OUT meters, LOUDNESS from `state::loudness_meter::LiveLoudness` (UI thread, output frames: short-term per 100 ms, `LoudnessMeter::integrated_since` reset (R), true peak, 30 s trace, view-only target chips); compact in the inspector. Footer / compact row: Oversampling `1x | 2x | 4x` + "5.7 ms latency" (`latency_text`).
  Reverb (`ui::devices::reverb` + pure `state::reverb_editor`): a custom time/dB tail picture (early reflections, lows/highs decay curves, highs fall faster with damping) with 3 hand-drawn draggable nodes (pre-delay/decay ⇔, damping ⇕; wheel over the tail = decay, double-click resets), a Space box (corner drag = size, right edge = width, wheel = size), a `FreqDisplay` low-cut strip and a wet-output `level_meter`; compact in the inspector. "❄ Freeze" (`param_glyph_toggle`, the stored `freeze` param) in the footer / compact row; frozen = tinted tail + "Frozen · tail held" badge. Duck / Duck rel. knobs (coral), a duck GR bar beside the wet meter (probe reading) + the shared key picker (`compressor::key_picker_with`) in the footer.
  Delay (`ui::devices::delay` + pure `state::delay_editor`): echo taps on a real time axis (`n * echo_ms`, `response::delay_time_seconds` shared with the processor) with a true beat/ms grid, alternating L/R when ping-pong is on, darker per repeat from the feedback filter; the first echo's drag snaps to `Sync` (`TimeChange::Sync`/`Free`), `Sync | Free` + the 12-step note `grid_selector`, `⇄ Ping-pong`, a `FreqDisplay` feedback-filter strip (two x-only nodes); S/P keys toggle sync/ping-pong while focused; compact in the inspector. Taps 2-4 (`DelValues::extra_taps`): bars at their times with faint feedback repeats and a handle (drag ⇔ = shared spread, ⇕ = level, wheel = pan, double-click resets; one step "Delay tap"), `Taps 1|2|3|4` row, Tap spread / Taps knobs.
  Chorus / Saturator / Gain (`ui::devices::{chorus,saturator,gain}` + pure `state::{chorus,saturator,gain}_editor`; shared `ui::devices::{commit_changes, KnobDef, knob_grid, compact_scroll, footer}`, kit `Knob::wheel_step` for 0.5 dB wheels): chorus = both voices' LFO delay scrolling at the real rate (UI phase, repaints only while visible and playing/audio passes, else frozen + 50 % dim; drag ⇕ depth ⇔ rate = one step) + width fan; saturator = mode cards (keys 1/2/3), `saturator_shape` curve scaled to fit (drag ⇕ drive in 0.5 dB, double-click 0, 150 ms morph), test-sine or probe before/after scope, harmonics by DFT of the curve; gain = goniometer of the probe output (⇔ pan ⇕ width, M mono), correlation meter, XL gain, Ø toggles, 900 px centred; compact forms in the inspector.
  Poly Synth (`ui::devices::poly_synth` + pure `state::poly_synth_editor`): OSC (5 wave cards incl. Wavetable: per wavetable osc a panel with table picker, waterfall of frames (current violet, modulation extent/live frame teal; drag ⇕ / wheel = position, Shift fine, double-click resets, a MOD handle dropped on it targets position) + Position knob; one-row docks put the panels in a column right of the cards, wrapped/stacked/compact under them; right-click copy to the other osc; semi/cents readouts dragged ⇕; unison 1-4) → FILTER (`FreqDisplay` with `poly_synth_filter_response_db`, node ⇔ cutoff ⇕ resonance = one Batch, wheel env amount, dashed env peak) → AMP/FILTER ENVELOPE (kit `adsr`, D = one Batch) → OUTPUT (300-point `scope_window` of the probe, flat when idle, held notes); on-screen keyboard via `preview_note_on/off`, A-K/Z-X only while the editor has focus (`SynthKeys`, released on key up/focus loss/close); wraps into two rows below 1000 px, stacks below 560; compact in the inspector.
  Electric Piano (`ui::devices::electric_piano` + pure `state::electric_piano_editor`): tone bars of a held C4 (`partials`: brightness tilts, bell lifts 7-9 pale) over a Mellow ↔ Bright slider bound to brightness, face card (L/XL Brightness/Bell/Decay, S Velocity/Release/Gain), tremolo picture (one note over 3 s from the DSP's envelope, L up / R down × the auto-pan, LFO lines); the keyboard (`ui::devices::keyboard`: `SynthKeys`, `keyboard_row`) is shared with the Poly Synth; compact in the inspector.
  Drum Machine (`ui::devices::drum_machine` + pure `state::drum_machine_editor`): 7 voice strips in `BAND_COLORS` (pad click = audition via `preview_note_on/off` + select, Ctrl/Cmd-click selects only, double-click resets the voice; envelope from the voice's params, 2 summaries, level bar drag ⇔ / wheel = one step), selected voice (kick waveform with its real pitch sweep, dashed amp envelope, L knobs), KIT tone/gain, ← → voices; strips flash 150 ms on the DSP trigger counters (`Flashes` over `probe().readings()`); <1100 px two rows, <700 px compact; compact in the inspector.
- Tests: state unit tests, `tests/ui.rs` drives the whole window headless with `egui_kittest`;
  `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --test ui -- --ignored` renders PNGs (wgpu),
  including automation lanes, the audio editor, an empty project, (`render_sampler_screens`)
  the sampler editors, (`render_sampler_editor_screens`) the Sampler (12-zone felt piano: dock, filter
  envelope, 1280, 540 stacked, compact), (`render_device_editor_screens`) the device editor (reverb, compact
  compressor, poly synth, drum sampler), (`render_project_screens`) the start screen, export dialog/progress/
  result, Locate missing files and the recovery prompt, and (`render_recording_screens`) armed
  tracks with take lanes, a recording in progress (fed by the engine's virtual device) and the
  audio settings dialog, and (`render_phase17_screens`) time signature changes + the signature
  lane, controller lanes (mod wheel, pitch bend), an automatic crossfade and a folder (open,
  folded), (`render_mix_screens`) the Mix tab (empty, analyzing, report, issues, pass running, review) and the Files tab glyphs, (`render_mid_side_match_screens`) an EQ side band + match preview, and (`render_library_screens`) the Library tab (list, filters, preview, pattern
  tooltip, style and form documents, credits, a pattern dragged over a lane).
- Plugin editors (`state::editors` pure logic, `audio::AudioEngine` polls open editors every
  30 ms): Edit button in the inspector (device and track instrument), double-click a plugin
  insert / instrument in the mixer or the instrument name in a track header. Window title
  "<device> — <track>". A gesture = one `SetParam` on release (live meanwhile, shown by the
  inspector); one-off values are batched after 350 ms quiet (>16 params: captured state
  instead); `StateChanged`, close and save capture the live state -> one `SetDeviceState`
  when it differs. States the running instance holds are live in `live::diff` (no reload);
  every reload clears that. Editors opened before the device is loaded wait for the reload.
  The generic editor (inspector sliders) has a filter and "show all" for large plugins.
- Recording (phase 16; `state::record` logic + `ui::record` views, `ui::transport`): arm states
  live in the project (`Project.armed`, saved; phase 19a): the ● button is one undo step "Arm
  'Vox'" (`SetArmedTracks`, plus In 1 for an audio track without input), `prune_record` syncs
  the player mirror `RecordState.armed` after every edit/undo/open (`live::diff` ignores
  arms; kept across device reopen); audio input monitoring already lives in `TrackInput.monitor`;
  ● on track headers and mixer strips (arming an audio track without an input gives it In 1,
  one undo step; armed instrument tracks get the live MIDI). Audio headers: input selector (mono
  / stereo pairs of the open input, or none), speaker monitor button (`ui::record::
  monitor_button`: click on/off, orange when on, Auto = dimmer outlined speaker with an "A",
  right-click Off/Auto/On), input level as a thin vertical meter on the header's left edge
  next to the color strip. Transport: red record button (Ctrl+R start/stop; R = record mode,
  Play records the armed tracks, nothing armed = plays). Toggling record while playing never
  stops the transport: off = stop capturing (`Player::end_recording`), on = punch in on the
  fly from the playhead (no count-in, no relocate; `Player::start_recording` while playing
  skips the count-in). The metronome toggle switches the click for playing and recording
  alike (right-click: only one of them, level); defaults are no click and no count-in
  (`RecordSettings::revision` 2 migrates older prefs), count-in 0/1/2/4 bars, PUNCH (record only inside the loop region; with the
  loop on: loop recording, a take per pass), "MIDI rec" menu (merge / overdub / takes per pass /
  input quantize); position display turns red while recording. `record_plan()` picks start,
  punch-out and pre-roll (pattern mode: the pattern loop, MIDI into the pattern clip, its track
  armed if nothing is). The recording is drawn live over its lanes (waveform from the writer's
  peaks, MIDI notes) and the finished one lands as one undo step "Record" (`finish_recording`);
  MIDI loop passes are merged into the player's copy of the track as they complete (FL overdub:
  heard on the next pass) without touching the document. Takes: T on the header expands take
  lanes (`view.takes`, `RowKind::Take`); click a take = play it for its region, drag = swipe-comp
  that range (snapped), double-click a lane header = play that take everywhere, menu: flatten /
  delete take; the main lane shows only what plays, with a take-count badge; S on a take splits
  its whole group. Audio settings (File menu): output/input device, sample rate, buffer size,
  input on/off, MIDI input, latency offset (ms), status (rates, clock mode, latencies), test tone
  (off by default, switched off on close); persisted in `Prefs.audio` (+ `Prefs.record` for
  metronome/count-in/punch/MIDI options); new devices reopen the player on its thread
  (`AudioEngine::configure`, device lists probed there). The AI context carries the armed
  tracks/metronome/count-in (`recording_context`). Recordings go to the project's `Audio/`, or
  `$XDG_STATE_HOME/cadenza/recordings/untitled-<pid>/` before the first save.
- Audio to MIDI (NE-3, `state::to_midi` + `ui::to_midi`): audio clip menu "Convert to MIDI…" or
  dragging an audio clip onto an instrument track opens the dialog (`start_convert_to_midi`):
  `analyze_clip` on a `cadenza-to-midi` worker (cache `AppState.analysis_cache`, default the
  user cache), options = `AudioToMidiArgs` (Recording | Humming resets the overrides, quantize +
  strength, key Off/Detect/24 keys, pitch bend + range, octave, sensitivity -> re-segments,
  target new track / instrument track), `convert_preview()` (pure, each frame: summary +
  picture), `apply_convert_to_midi` = one undo step + piano roll on the clip. Hum to MIDI
  (`RecordState.hum`, session only): instrument track menu "Hum to MIDI when recording" arms
  the track and shows an "H" header toggle; `record_plan` gives it audio input 1 and
  `finish_recording` runs `to_midi::hum_commands` (last pass on a scratch audio track ->
  `MidiOptions::hum()` -> AddClip on the instrument track) inside the same "Record" batch.
- Note editor (NE-6, `state::note_editor` pure builders + `ui::note_editor`): audio editor
  Waveform | Notes switch (`AudioEditorView.mode`, `.notes: NoteEditorView`), clip menu "Edit
  notes…", inspector "Edit notes". `open_note_editor` -> `NoteAnalyses` (file analysis on a
  worker, shared cache) -> "Detect notes" (`cadenza_notes::clip::detect`) when the clip has none.
  Blobs (level = thickness) on semitone rows, resulting pitch curve (`edited_curve`), sibilants
  hatched, flagged notes amber with a ✂ split; tools Main (⇕ retune snapped to the key / Alt free,
  ⇔ move, double-click split/merge), Vibrato, Drift, Formant, Level, Split (keys 1–6); macros
  column Correct pitch / Quantize time / Leveling / Sibilants / Detection; builders
  `move_pitch`, `move_time` (crossing refused), `tool_edit`, `correct_pitch` (key defaults to
  Detected = `tuning` fit + targets, "E phrygian +33 c (detected)"), `quantize_time`,
  `leveling`, `reset`, `sibilant_gain`, `replace`; `AppState::{detect_notes, split_audio_note,
  merge_audio_notes, remove_audio_notes, note_edit}`. Arrange "♪" on clips with edited notes.
- AI producer chat (phase 12; `state::ai` logic, `ui::chat` view): F10 / View / AI menu / F1.
  Docked right (`PanelKind::Chat`). `ai::worker` is a thread with its own tokio runtime that
  owns the `SessionAgent` (conversation kept across requests) and everything that may block
  (model calls, keyring); the UI sends commands and polls events (`AiChat::poll`) each frame.
  Chat rows: user bubbles, streamed markdown replies (`ai::markdown`: headings, lists, code,
  bold/italic/code spans), collapsed thinking, compact tool rows (✔/✖, expandable args +
  result), notices, actionable errors (missing/rejected key opens the settings), a footer per
  request (status, cost, session total, undo label); quick-action chips from the selection;
  Enter sends, Shift+Enter new line; ☰ menu: new conversation, settings, transcript path.
  **Edit lock decision**: while a request runs, `apply`/`edit_session`/undo/redo/new/open
  are refused with a notice and the edit panels draw disabled (transport stays live);
  plugin-editor values and finished background edits go through `apply_or_queue` and are
  applied after the request. Each request is one undo step "AI: <request>" (document undo
  group opened/closed by the agent however the request ends); Stop (`CancelToken`) ends at
  the next tool boundary or aborts the model call, keeping the edits as that one step.
  Settings (`ai::settings::AiSettings`, persisted in `Prefs.ai`, no secrets): provider
  OpenRouter | OpenAI-compatible (base URL), model (presets + free text), reasoning effort,
  budget per request, optional price for estimates, allow plugins. Keys (`ai::secrets`):
  env var (`OPENROUTER_API_KEY` / `OPENAI_API_KEY`) > nearest `.env` > stored key; stored keys
  go to the OS keyring (`keyring` 4, Secret Service via zbus) or, when no secret store runs,
  `$XDG_CONFIG_HOME/cadenza/credentials.json` (0600); never shown in clear (password field).
  Transcript + files root for unsaved projects: `$XDG_STATE_HOME/cadenza/sessions/chat-<t>-<pid>/`;
  saved projects use their folder. Headless states use an in-memory key store; tests use
  `cadenza_agent::scripted::ScriptedProvider` (no network).
