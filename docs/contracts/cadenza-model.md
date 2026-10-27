# cadenza-model — contract

Moved from CLAUDE.md (kept in sync with the code: update it when the contract changes).
Core types, `Command` and `Document` are in CLAUDE.md; this file holds the rest.

Sample zones (phase 13, `model::sample`): typed structured state of the sample-based built-ins
(not the opaque `state`, which stays for plugins), all fields but sample/keys optional in JSON
(format_version 1). Samples are absolute paths like audio clips; `SampleRef.name` keeps the
original file name so a later "collect project files" can relocate/rename them. Validation:
keys 0..=127, low <= high, velocity 0..=1, tune ±48, gain -60..=24, end > start, loop_end >
loop_start, crossfade 0..=10 s, groups <= 16, at most 1024 zones. `root_from_file_name`
("Piano C3", "pad_F#2", "Strings_60"; 60 = C4), `zone_summaries` (get_project lists zones).

Clip fades are in **seconds**, not beats: like `offset_seconds` they shape the recording
(de-click, natural decay) and must not stretch with tempo changes; AI tools accept beats too
and convert at the clip edge. AddClip/SetClipFades reject a fade (or fade_in + fade_out)
longer than the clip at the current tempo; later resizes/tempo changes are not rejected —
the engine scales fades down proportionally to fit.

Time signatures (phase 17): changes are stored by **bar index**, not tick, so a change always
starts a bar and editing an earlier signature only moves later bar lines (content stays at its
ticks, like tempo changes). `cadenza_core::Meter<'_>` (`Project::meter()`, allocation free:
`bar_at/bar_start/bar_floor/bar_ceil/bar_position/tick_at_bar_position/bbt/tick_at_bbt/
bar_lines/segments_between/change_ticks/signature_at`) is the single bar/beat math (beats are
the denominator note); `MeterMap` owns a copy. Summary: `time_signature` (bar 1) +
`time_signature_changes [{bar (1-based), beat, time_signature}]`, marker bars and
`length_bars` follow the changes. At most 1024 changes, bar index 1..=100000.

Controller lanes (phase 17, `model::controller`): MIDI semantics, each point is one event whose
value holds until the next (FL event editor). `Controller` serializes as a string and parses
names ("sustain", "mod wheel", "bend", "aftertouch", "CC 74"); `default_value` (bend center,
CC7 100, CC10 center, CC11 full, else 0) is the resting value; `is_switch` (CC64..69). Pure
helpers: `window` (split/trim: the value in effect at the cut becomes a point at 0), `ramp`
(MIDI-quantized, curved), `splice`. `takes::split_clip` carries lanes; validation: ticks >= 0,
values in range, one lane per controller, <= 100000 points (points past the clip end are kept
but never played). Summary: `controllers: ["cc1 (12 events, 0.00..1.00)"]`.

Folders (phase 17): `TrackKind::Folder` tracks hold other tracks (`Track::folder`, nesting
allowed, no cycles); they carry no audio (no clips/devices/sends: rejected with actionable
errors). Their volume is a dB offset for every track inside (VCA), their mute/solo apply to
them. `Project::folders_of` (innermost first) / `folder_contents` (any depth, display order).
Removing a folder moves its tracks up to its parent (undo restores). Summary: `folder`.

Linked clips (T39, `model::links`): clips sharing `Clip::link` hold identical MIDI content
(validated on AddClip / `UpdateClip { link }` (double option, `null` = unique) and on load);
AddNotes/RemoveNotes/UpdateNotes/SetControllerLane on one copy are applied to every copy in
the same step (`apply::sync_link`), so undo restores all. Lengths may differ: shared notes
must start inside the longest copy (`validate::content_span`), shorter copies just stop
earlier (engine/export skip notes past a clip's end; load validation does not bound linked
notes). Builders: `copy_clip` (linked copy; an unlinked source joins `new_link`),
`make_unique` / `unique_commands` (also frees the last remaining copy; a copy holding shared
notes past its end is replaced by a trimmed one with the same id), `link_count`;
`takes::split_clip` unlinks first. Summary: `link: [other clip ids]`; MCP `duplicate_clip
{ linked }` (default false), notes/controller tool descriptions mention linked copies.
Implemented extras: `ResetParam` (inverse of a first `SetParam`), `UpdateSend`, optional `index`
on AddTrack/InsertDevice/AddSend, `UpdateClip` can move a clip to another track. Instruments are
removed with `SetInstrument { device: null }`. Removing a track drops its lanes and sends that
target it and re-routes its dependants to Master (undo restores all). MIDI clips only on
Instrument tracks, audio clips only on Audio tracks. Device param ranges are NOT validated
here (model doesn't know devices) — callers with registry access (mcp) must validate.

Takes (phase 16, `model::takes`): clips sharing a `Take::group` are alternative takes of one
stretch of a track (loop recording passes, a recording over existing clips); only the active
take plays (`Clip::plays()`; the engine skips the others, `edits::overlaps`/rubber bands ignore
them). `UpdateClip { take }` (double option) changes membership; pure builders return ordinary
commands: `select_take(clip)` (Batch "Select take"), `split_clip(.., right_id, right_take)` (shared
with the app's S split), `split_group(group, at, new_group)` (split comping: each part keeps every
take), `comp_range(clip, from, to)` (swipe: splits at both ends + selects, Batch "Comp take", built
on a scratch `Document`), `flatten_group` (keep what plays). `Track::input` is edited with
`UpdateTrack { input }` (Audio tracks only, channels 0..=63). Both fields are optional in JSON
(format_version stays 1); summaries show `input` and `take`.

Project files (phase 14, `model::files`): a portable project is a folder
`<Name>/<Name>.cdz` + `Audio/` (recordings, imports, processed copies) + `Samples/`
(collected zone files) + `Renders/` (exports) + `.cadenza/` (autosaves, session marker; safe to
delete). Project files are `.cdz` (JSON text) and nothing else: `load_json`/`save_json`/
`save_json_absolute` refuse other names (`ModelError::NotAProjectFile`, `files::is_project_file`,
`PROJECT_EXTENSION`); archives are `.cdzpack` (`ARCHIVE_EXTENSION`), archives from before 0.2
(`.cadenzapack`/`.cadenza.zip`/`.zip`) still unpack, their `<Name>.cadenza.json`
(`LEGACY_PROJECT_SUFFIX`) extracted as `<Name>.cdz`. In memory every file path is absolute; `save_json(path)` stores paths inside the folder
holding the file relative to it (`/` separators) and `load_json` resolves them against that
folder, so moving the folder keeps working; other paths stay absolute and old absolute
projects load unchanged (format_version stays 1). `save_json` is atomic (temp + fsync +
rename, dir fsync) and keeps the previous file as `<file>.bak` (`backup_path`,
`write_atomic`); `save_json_absolute` (autosaves) writes absolute paths without a backup.
`file_refs()` lists audio clip files and sample zones (`FileRef { path, name, used_by }`),
`missing_files()` the absent ones, `relink_commands(fn)` builds the `UpdateAudioClip` /
`SetSampleZones` commands that repoint references (zones keep their original names);
`ProjectLayout` names the folders, `sanitize_file_name`, `project_stem`.
