# cadenza-library — contract

Moved from CLAUDE.md (kept in sync with the code: update it when the contract changes).
Scope: (phase 18a, design `docs/design/style-library.md`)

Packs = folders of JSON (`pack.json` manifest with SPDX `license`/`attribution`/
`redistributable` + `styles/ forms/ progressions/ grooves/ patterns/<role>/`); item ids
`<pack>/<slug>`. The hand-written `core` pack (`crates/library/packs/core`, CC0) is compiled
in by `build.rs`; user packs load from `$CADENZA_LIBRARY_DIR` or
`$XDG_DATA_HOME/cadenza/library/<pack>/` (`Library::load_default`). Invalid packs/items are
reported in `Library::reports()` (`PackReport { issues }`) and skipped, never fatal;
`check_references` reports dangling ids. `validate` enforces the budgets (summary <= 120
chars; compact JSON: pattern 1,200, style 3,500, form 2,000, progression 600, groove 1,200),
lane keys, step strings (`X x o = . -`, length divides the pattern), license fields.
Patterns: `role`, `harmony` (none | chord | scale | absolute + key), `steps_per_beat`,
`bars`, `meter`, `feel`, `family` + `variant` (main | variation | fill | intro | ending),
`lanes` (ordered): drum names (`DRUM_LANES`, GM superset of `write_drum_pattern`'s, e.g.
shaker 70, claves 75, hi_bongo 60, open_conga 63), chord tones `R 3 5 7 9 11 13` (+/-n
octaves), `chord` (voiced with `voicing`, voice-led), `bass`, `app` (semitone below the next
root), `ant` (next root, anticipated), scale degrees `^1..^7` (`^b3`), absolute notes.
`realize(pattern, &RealizeOptions { bars, key, chords, beats_per_chord, fill_every, vary,
fill, variation, groove, velocity, octave })` is pure -> `Realized { notes: Vec<NoteSpec>
(beats), length_beats, warnings }`; `RealizeError` texts are actionable. `Library::search(
&Query { text, kind, genre, role, section, energy, family })` ranks token matches over
id/name/aliases/genres/tags/family/summary (accents folded, `section_name` maps "Chorus 2"
-> chorus, "Pre-chorus" -> pre; fills demoted unless asked); `card(item)` -> `{id, kind,
name, summary, facts}`; `family_member(pattern, variant)`; `parse_chords` (symbols or roman
numerals with a key, mixable).
MCP (`tools::library`; `Session::library()` loads `Library::load_default` on first use,
`set_library` for tests/the app): `library_search { query, kind, genre, role, section, energy,
family, limit (8, max 25) }` -> `{cards: [{id, kind, summary, facts}], total}`; `library_get {
ids (<= 5) }` -> items as stored + pack `license` (unknown ids suggest the closest);
`apply_pattern { pattern, track_id, start_beat, bars, key, chords (symbols | roman + key |
progression id + key), beats_per_chord, fill_every, vary, groove ("none" drops the
default), velocity, octave }` -> one `AddClip` named `Name [pack/id]` + `{clip_id,
length_beats, summary, warnings}` (drum lanes go through `compose::drum_lane_warnings`, shared
with `write_drum_pattern`: drum_sampler pads without samples, notes drum_machine cannot play;
meter mismatch; pitched pattern on a drum kit); `build_pattern_clip(project, library, args)`
is the pure builder (the app's drag and drop uses it). `apply_form { form, start_bar, tempo,
style }` -> one `Batch` "Apply form '<name>'" (markers replacing ones at the same tick,
SetTempo, `SetProjectMeta` with the style: explicit or the one style listing the form) +
section table; `build_form` is pure. `Project.meta: ProjectMeta { style }` (optional in
JSON, format_version 1; `Command::SetProjectMeta`, no sound: `live::diff` ignores it) and
`ProjectSummary.style`. Budget test: `search_and_get_outputs_stay_under_their_caps`. Agent
prompts carry the library lines; hand-off notes keep the library ids.
Import (`library::import::import_pattern(notes, &ImportOptions)`, used by `cadenza library
import <file.mid> [--pack dir] [--track n | --channel n] [--role] [--key] [--chords
--beats-per-chord] [--steps-per-beat] [--bars] [--genre] [--force]`): quantizes to the grid,
per-step timing deviations + velocity ratios become a `Groove` (`<slug>_groove`, skipped when
the source was already quantized), role from GM channel 10 / program family / register,
pitched notes as chord tones when the chords explain at least half of them, else scale
degrees of the given or inferred (Krumhansl) key; lanes are found by realizing candidates, so
realizing the result over the same chords gives the quantized notes back. Writes
`patterns/<role>/<slug>.json`, `grooves/`, `midi/<slug>.mid` into the pack (default
`<library dir>/user`, created with `LicenseRef-user-imports`, not redistributable) and
re-validates it. `cadenza library validate <pack or library folder>` exits 1 on problems;
`list [--items] [--json]`. Nothing is downloaded.
