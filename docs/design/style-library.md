# Style library: patterns, song forms and style guides

Status: design draft (2026-10-12); **SL-1, SL-2, SL-4 and the SMF part of SL-3 shipped** (2026-10-13, phase 18a): the
`cadenza-library` crate with the embedded `core` pack and the four MCP tools. See "What
shipped" at the end.

This is a separate roadmap from the main build phases (phase 11, 12, 13, 13b... in
`CLAUDE.md`). Its steps are numbered **SL-1 to SL-5** ("style library") so they never get
confused with those phases.

## Goal

Give the AI (and the user) a catalogued, licensed library of musical *vocabulary*: genre
style guides, song forms, and MIDI patterns by role. The model stops inventing grooves from
theory and instead **chooses, combines and varies** idiomatic material, like a producer
reaching for their MIDI packs. "Latin pop" should come out with a dembow, a tresillo bass
and a guitar tumbao, not with generic block chords on every beat.

Two consumers, one catalog:

- **The LLM**: needs the right information at the lowest token cost, on demand.
- **The user**: browses, previews and drags the same items in the app's browser.

Both go through the same command API: applying a pattern produces ordinary undoable
`Command`s (`AddClip`, `AddMarker`, ...). The library is never a special path.

## Design principles for the LLM

1. **Progressive disclosure.** Three levels, each much cheaper than the next:
   - *card* (search result): id + one-line summary + key facts, about 30-40 tokens;
   - *detail* (`library_get`): a style guide, a form, or a pattern in compact notation,
     about 150-700 tokens;
   - *apply*: realizes the item into the project and returns ids plus one summary line.
     The model never has to read the notes it just wrote.
2. **Relative, not absolute.** Pitched patterns are stored relative to the chord (root,
   third, fifth...) or to the scale (degrees), so one pattern fits any key and progression.
   Fewer items, and no transposition work for the model.
3. **Readable notation.** Patterns use step grids, the same notation `write_drum_pattern`
   already accepts. The model can read a pattern at a glance, edit it, or write a variation
   of its own.
4. **The style guide is the plan.** One `library_get` of a style gives the model tempo,
   feel, form, progressions per section, roles with pattern ids per section, sounds and mix
   targets. That is "an idea of what we want" in one compact document. After that, the
   model mostly issues `apply_*` calls.
5. **Hard budgets, enforced by the validator**, so packs from the community cannot bloat
   the context: card summary ≤ 120 chars; pattern detail ≤ 1,200 chars; style guide ≤ 3,500
   chars; search returns ≤ 8 cards by default (max 25).
6. **Few tools.** The agent already exposes 62 tools, and every tool's schema is paid on
   every request. The library adds **four**, and kits reuse the existing preset tools.

## Item kinds

| Kind | What it is | Typical size (detail) |
|---|---|---|
| `style` | Genre style guide: tempo, feel, harmony, form refs, roles with pattern ids per section, sounds, mix, do/don't | 400-700 tokens |
| `form` | Song structure: sections with bars, energy, active roles, harmony label | 100-250 tokens |
| `pattern` | A MIDI pattern for one role (drums, perc, bass, comping, arp, lead/hook, pad, fill) | 60-300 tokens |
| `progression` | Named chord progression in roman numerals, with mode and use | 30-60 tokens |
| `groove` | Timing/velocity template (per-step offsets and accents) extracted from human performances, applicable to any pattern | 60-120 tokens |
| `kit` | A drum_sampler/sampler preset (sample mapping) shipped in the pack; exposed through `list_presets`/`load_preset` | n/a |

## On disk

```
<library root>/
  <pack id>/
    pack.json              # manifest: id, name, version, license, attribution, source, homepage
    styles/*.json
    forms/*.json
    patterns/<role>/*.json
    progressions/*.json
    grooves/*.json
    midi/*.mid             # optional originals (imported material)
    kits/<name>/kit.json   # sampler/drum_sampler preset
    kits/<name>/*.wav|flac
```

Library roots, in search order: the built-in `core` pack (small, embedded in the binary
with `include_str!`), then `$CADENZA_LIBRARY_DIR`, else `$XDG_DATA_HOME/cadenza/library`
(same resolution as `default_preset_dir`). Item ids are `<pack>/<slug>`, for example
`core/dembow_basic`. Ids are stable: projects and transcripts may refer to them.

All files are JSON (serde, like the rest of the project). An index (`index.json`, cards
only) is rebuilt per pack when a file's mtime changes, so search never parses every pattern.

### pack.json

```json
{
  "format_version": 1,
  "id": "groove_midi",
  "name": "Groove MIDI (Magenta)",
  "version": "1.0.0",
  "license": "CC-BY-4.0",
  "attribution": "Groove MIDI Dataset, Google LLC (Gillick et al., 2019)",
  "source": "https://magenta.tensorflow.org/datasets/groove",
  "redistributable": true
}
```

`license` is an SPDX id. The app shows credits per pack. A pack with
`"redistributable": false` (non-commercial or unclear terms) can be installed by the user
but is never bundled. CC-BY attribution applies to redistributing the pack, not to songs
made with it. The browser says so.

## Pattern notation

Step grids, one string per lane, one character per step:

| Char | Meaning |
|---|---|
| `X` | accent (velocity 1.0) |
| `x` | normal (0.8) |
| `o` | ghost (0.45) |
| `=` | tie: extends the previous note on this lane by one step |
| `.` or `-` | rest |
| space, `\|` | ignored (readability: bar and beat separators) |

The lane key decides the pitch:

| Lane key | Pitch | Used by |
|---|---|---|
| `kick`, `snare`, `rim`, `closed_hat`... | GM drum / drum_sampler pad (same names as `write_drum_pattern`) | drums, perc |
| `R` `3` `5` `7` `9` `11` `13` | chord tone of the chord sounding at that step (its real quality: b3, b7...) | bass, comping, arps |
| `chord` | the whole chord, voiced with the pattern's `voicing` | comping, stabs |
| `bass` | the chord's bass note (slash bass aware) | bass |
| `app` | chromatic approach: a semitone below the next chord's root | bass |
| `^1` ... `^7`, `^b3`, `^#4` | scale degree of the key | hooks, leads, counter-lines |
| `C4`, `60` | absolute pitch | anything that cannot be relative |

An octave suffix shifts by octaves: `R-1`, `5+1`, `^3+1`. Each role has a register
(`bass`: octave 2, `comping`: around C4, `lead`: around C5) that the pattern can override
with `"octave"`.

Imported material that does not fit a grid (expressive solos, rubato) keeps its original in
`midi/` with `"harmony": "absolute"` and its `key`. `apply_pattern` transposes it to the
target key. `library_get` returns a 16th-grid rendering of it as a preview, never raw events.

### Pattern file

```json
{
  "id": "dembow_basic",
  "kind": "pattern",
  "name": "Dembow basic",
  "summary": "Classic dembow: four-on-floor kick, snare on the 'a' of 1 and the '&' of 2, straight 16th hats.",
  "role": "drums",
  "genres": ["reggaeton", "latin_pop", "dembow"],
  "sections": ["verse", "chorus"],
  "energy": 3,
  "feel": "straight",
  "meter": "4/4",
  "bars": 1,
  "tempo": [88, 102],
  "family": "dembow",
  "variant": "main",
  "harmony": "none",
  "steps_per_beat": 4,
  "lanes": {
    "kick":       "x...x...x...x...",
    "snare":      "...x..x....x..x.",
    "closed_hat": "x.x.x.x.x.x.x.x.",
    "shaker":     "oxoxoxoxoxoxoxox"
  },
  "pairs_with": ["core/bass_tresillo_808"],
  "tags": ["dembow", "tresillo"]
}
```

A chord-relative bass:

```json
{
  "id": "bass_tresillo_808",
  "kind": "pattern",
  "name": "Tresillo 808 bass",
  "summary": "Sub bass on the tresillo (3+3+2) following chord roots, fifth pickup at the end of the bar.",
  "role": "bass",
  "genres": ["latin_pop", "reggaeton"],
  "sections": ["verse", "chorus"],
  "energy": 3,
  "bars": 1,
  "harmony": "chord",
  "steps_per_beat": 4,
  "lanes": {
    "R": "x=====x=====x=..",
    "5": "..............x."
  },
  "pairs_with": ["core/dembow_basic"]
}
```

Fields shared by every item: `id`, `kind`, `name`, `summary` (the card line, written for an
LLM choosing items), `genres`, `tags`. Pattern-specific fields: `role`, `sections`,
`energy` (1-5), `feel` (`straight` | `swing:<0..1>` | `triplet`), `meter`, `bars`, `tempo`
range, `harmony` (`none` | `chord` | `scale` | `absolute` + `key`), `voicing` (for
`chord` lanes), `family` + `variant` (`main` | `variation` | `fill` | `intro` | `ending`),
`pairs_with` (patterns that lock with this one), `groove` (default groove id).

`family` + `variant` let the engine do the variation work that LLMs forget:
`apply_pattern { fill_every: 4 }` swaps the last bar of every 4 for the family's fill, and
`vary: true` alternates `main` and `variation` bars.

## Style guide

```json
{
  "id": "latin_pop",
  "kind": "style",
  "name": "Latin pop (reggaeton-pop)",
  "aliases": ["pop latino", "reggaeton pop", "urbano pop"],
  "summary": "Mid-tempo dembow pop: minor keys, 4-chord loops, nylon guitar and plucks over an 808 tresillo bass.",
  "tempo": { "min": 88, "max": 102, "typical": 94 },
  "feel": "straight",
  "meter": "4/4",
  "keys": ["minor (most)", "major"],
  "harmony": {
    "loops": ["core/prog_i_VI_III_VII", "core/prog_vi_IV_I_V", "core/prog_i_iv_VII_III"],
    "harmonic_rhythm_beats": 4,
    "notes": "One loop for the whole song is idiomatic; contrast comes from arrangement, not new chords. Bridge may switch to iv or VI-led loop."
  },
  "forms": ["core/form_latin_pop_radio", "core/form_latin_pop_short"],
  "roles": {
    "drums":   { "verse": ["core/dembow_minimal"], "pre": ["core/dembow_minimal", "core/perc_riser"], "chorus": ["core/dembow_basic"], "fill": "dembow family fill on bar 8 of each section" },
    "perc":    { "any": ["core/perc_bongo_tumbao", "core/shaker_16ths"] },
    "bass":    { "verse": ["core/bass_tresillo_808"], "chorus": ["core/bass_tresillo_808"], "register": "C1-C3" },
    "guitar":  { "verse": ["core/nylon_arp_tresillo"], "chorus": ["core/guitar_tumbao"], "sound": "nylon guitar or bright pluck" },
    "keys":    { "chorus": ["core/pad_long_chords"], "sound": "warm pad or soft piano, low in the mix" },
    "hook":    { "post_chorus": ["core/hook_pluck_minor_pent"], "notes": "short motif, 2 bars, repeated with a variation" }
  },
  "sounds": { "kit": "core/kit_reggaeton", "bass": "sampler: 808 sub", "presets": ["sampler: Nylon Guitar"] },
  "mix": { "lufs": -9, "notes": "Kick and 808 dominate the lows (sidechain the 808 lightly to the kick); guitar panned slightly, vocal space 1-4 kHz; plate reverb short." },
  "do": ["Drop the kick for the pre-chorus last bar", "Thin out the verse (half the roles)", "Add a perc fill or riser into every chorus"],
  "dont": ["Block chords on every beat", "Whole-note bass under a dembow", "Same drum pattern in every section"]
}
```

The `do`/`dont` lists hold most of the "taste": they are the mistakes a beginner makes
in this genre. Validators keep the whole guide under the 3,500-char budget.

## Song form

```json
{
  "id": "form_latin_pop_radio",
  "kind": "form",
  "name": "Latin pop radio form",
  "summary": "Intro-V-Pre-C-Post-V-Pre-C-Post-Bridge-C-Outro, about 3:10 at 94 bpm.",
  "genres": ["latin_pop"],
  "sections": [
    { "name": "Intro",       "bars": 4, "energy": 2, "roles": ["guitar", "perc"] },
    { "name": "Verse 1",     "bars": 8, "energy": 2, "roles": ["drums", "bass", "guitar"] },
    { "name": "Pre",         "bars": 4, "energy": 3, "roles": ["drums", "bass", "keys"], "ending": "drop" },
    { "name": "Chorus 1",    "bars": 8, "energy": 4, "roles": ["drums", "perc", "bass", "guitar", "keys"] },
    { "name": "Post-chorus", "bars": 4, "energy": 4, "roles": ["drums", "bass", "hook"] },
    { "name": "Verse 2",     "bars": 8, "energy": 3, "roles": ["drums", "perc", "bass", "guitar"] },
    { "name": "Pre",         "bars": 4, "energy": 3, "roles": ["drums", "bass", "keys"], "ending": "drop" },
    { "name": "Chorus 2",    "bars": 8, "energy": 5, "roles": ["drums", "perc", "bass", "guitar", "keys", "hook"] },
    { "name": "Bridge",      "bars": 8, "energy": 2, "roles": ["keys", "guitar"] },
    { "name": "Chorus 3",    "bars": 8, "energy": 5, "roles": ["drums", "perc", "bass", "guitar", "keys", "hook"] },
    { "name": "Outro",       "bars": 4, "energy": 2, "roles": ["guitar", "perc"] }
  ]
}
```

`ending` (`drop` | `fill` | `riser` | `none`) tells `apply_pattern`/the agent what to do in
the section's last bar. Forms derived from SALAMI/Harmonix/Chordonomicon statistics are
generated offline by a script and stored like any other form: the source datasets are
never shipped.

## MCP tools (four)

| Tool | Args (abridged) | Returns |
|---|---|---|
| `library_search` | `query?`, `kind?`, `genre?`, `role?`, `section?`, `energy?`, `limit?` (8) | cards: `{id, kind, summary, facts}` where facts = a few key fields (`role`, `bars`, `energy`, `tempo`) |
| `library_get` | `ids` (≤ 5) | details: style guide, form, or pattern in grid notation |
| `apply_pattern` | `pattern`, `track_id`, `start_beat`, `bars?`, `key?`, `chords?` (symbols or roman + key), `beats_per_chord?`, `fill_every?`, `vary?`, `groove?`, `velocity?`, `octave?` | `{clip_id, length_beats, summary, warnings}` |
| `apply_form` | `form`, `start_bar?`, `tempo?` | markers added (one Batch) + section table `{name, start_beat, bars, energy, roles}` |

Behavior notes:

- `apply_pattern` builds one `AddClip` (a `Batch` when it spans several clips), so it is
  one undo step and live playback picks it up through `set_track_clips` as today.
- Drum lanes reuse the `write_drum_pattern` lane mapping and its drum_sampler warnings
  ("lane 'shaker' has no sample").
- `library_search` ranks by token match over id/name/aliases/tags/genres plus filters.
  No embeddings, no network, no vendor dependency.
- Errors are actionable, as everywhere else: "pattern 'core/bass_x' needs chords: pass
  `chords` (e.g. "Am F C G") or `key` + roman numerals".
- Kits are presets: packs register their kits with the preset catalog, so
  `list_presets`/`load_preset` already find them and no new tool is needed.

## Agent integration (context and tokens)

- **System prompt**: three lines, not a catalog. "For a genre request, `library_search`
  kind=style for it and `library_get` the guide before deciding the song; prefer
  `apply_form` + `apply_pattern` from the guide over writing grooves note by note; vary
  with `fill_every`/`vary`, then edit by hand only what makes the song unique (melody,
  hook)."
- **Typical session cost**: 1 search (~300 tokens) + 1 style guide (~600) + 1 form (~200)
  + ~10 `apply_pattern` results (~60 each). The model reads about 1.7k tokens of library
  output instead of producing thousands of `add_notes` tokens, and spends its output on
  decisions rather than notes.
- **Compaction**: the hand-off note written past `compact_at_input_tokens` keeps the style
  and form ids in use. They are stable, so after compaction the model re-fetches only what
  it needs instead of carrying the guide in history.
- **Session agent** (app chat): context already includes the selection; when the selected
  clip came from a library pattern, the clip name carries the id (for example
  `Dembow basic [core/dembow_basic]`). "Make this more energetic" can then
  `library_search { family, energy: 4 }` directly.

## App

Browser gets a **Library** tab: search box + filters (kind, genre, role, section,
energy), cards with summary and pack license, click previews (auditioned through the
existing preview path, over the current tempo and a default progression), drag & drop onto
a track/lane (`dnd::Payload::Pattern` -> the same `apply_pattern` builder), styles and
forms open as read-only documents with "Apply form". A credits view lists every installed
pack and its attribution.

## Implementation plan (SL-1 to SL-5)

Independent of the main phases. Each step ships on its own and leaves the tree green.

- **SL-1** **`cadenza-library` crate** (depends on core, theory, model): formats, pack loader +
   index cache, validator (schema, budgets, lane keys, license fields), search, and
   `realize(pattern, context) -> Vec<NoteSpec>` (pure, unit-tested). Embedded `core` pack,
   hand-written: 3-4 styles (latin_pop, pop, trap, house), forms, progressions, ~40
   patterns. Latin idioms (dembow, clave 3-2/2-3, tresillo, tumbao, montuno, bongo
   martillo) are written by hand: no open dataset covers them cleanly.
- **SL-2** **MCP tools** (`tools/library.rs`) + agent prompt lines + a budget test (search/get
   outputs stay under their caps for the core pack).
- **SL-3** **Importers** (CLI `cadenza library import ...`, never at runtime):
   - SMF -> pattern: quantize to the grid, keep the timing deviations as a `groove`,
     role from GM channel 10 / program, chord-relative conversion through `cadenza-theory`;
   - Groove MIDI Dataset (CC BY 4.0): its metadata (style, bpm, beat/fill) maps to
     `genres`, `tempo`, `variant`; grooves extracted per style;
   - free-midi-chords (MIT): progressions by mode;
   - LMMS projects (CC licensed, `.mmp`/`.mmpz` XML): patterns + form from the song
     editor;
   - forms from SALAMI (CC0) statistics.
- **SL-4** **App Library tab**, drag & drop, credits.
- **SL-5** **Kits**: packs with drum_sampler/sampler kits; SFZ subset import later.

## Open questions

- Should the project remember its style (`Project.meta.style`, optional, format stays 1)?
  It would let any session re-fetch the guide without guessing. Not needed for SL-1.
- Who reviews the hand-written latin patterns? A musician's pass would be worth more than
  any dataset.

## What shipped

- **SL-1** (`crates/library`): formats (`format.rs`; `deny_unknown_fields` so typos are
  reported), lane keys and step grids (`lanes.rs`), pack loader (`library.rs`: embedded
  `core` pack generated by `build.rs` from `packs/core/**/*.json`, user packs from
  `$CADENZA_LIBRARY_DIR` or `$XDG_DATA_HOME/cadenza/library/<pack>/`), validator
  (`validate.rs`), search and `realize` (`realize.rs`). The core pack has 7 styles
  (latin_pop, pop, trap, house, lofi, neo_soul, salsa), 9 forms, 14 progressions, 4 grooves
  and 58 patterns. Deviations from the draft:
  - Lane key `ant` (the next chord's root, played early) was added for tumbao basses and
    montuno anticipations; a lane starting with `=` continues the note the same lane held at
    the end of the previous bar (ties over the bar line).
  - Registers are role centers: the chord root (or key tonic) lands in
    `[center - 6, center + 5]` (bass C2..B2, comping/pad around C4, arp E4, lead C5);
    `octave` on a pattern (and on `apply_pattern`) shifts it by octaves. Chord tones a chord
    lacks fall back to the key's diatonic tone, else a plain default (a missing 7th becomes
    the octave, never a foreign seventh).
  - Item slugs may contain upper-case letters (`prog_i_VI_III_VII`); pack ids, genres, tags
    and sections are lower-case.
  - No on-disk `index.json`: packs are parsed once at startup (fast at this size) and the
    cards are built in memory.
  - Budgets also cover forms (2,000 chars), progressions (600) and grooves (1,200).
- **SL-2** (`crates/mcp/src/tools/library.rs`): `library_search`, `library_get`,
  `apply_pattern`, `apply_form` as designed, plus `Project.meta.style` (the open question:
  answered yes; set by `apply_form`, shown in the project summary). `chords` also accepts a
  progression id (read in `key`'s root with the progression's mode). Drum warnings also cover
  the built-in `drum_machine` (notes it has no sound for; since phase 18b it plays every GM
  percussion note 35-81, so this only fires for notes outside the kit).
- **SL-3** (partial): `cadenza library import <file.mid>` (SMF -> pattern + groove in a user
  pack: quantize, groove from timing deviations and velocities, role from GM channel 10 /
  program / register, chord-relative or scale-degree lanes through `cadenza-theory`, key
  inferred when not given), `cadenza library validate <dir>` and `cadenza library list`.
  The dataset importers (Groove MIDI, free-midi-chords, LMMS, SALAMI) are not done: nothing
  is downloaded.
- **SL-4** (`crates/app/src/state/library.rs`, `ui/library.rs`): the browser's Library tab
  as designed (search + kind/genre/role/section/energy filters, cards with summary and pack
  license, click previews through the stopped-transport note preview path at the song
  tempo over the default progression, drag and drop with `Payload::Pattern` through the
  same `build_pattern_clip` builder, read-only style/form documents with "Apply form",
  credits). Double-click writes a pattern at the playhead; dropping below the tracks adds a
  track with a fitting built-in instrument.
- Not done: SL-5 (kits), the dataset importers of SL-3, an on-disk index cache.
