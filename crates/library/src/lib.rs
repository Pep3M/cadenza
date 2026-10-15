//! Cadenza's style library: a catalogued, licensed vocabulary of genre style guides, song
//! forms, chord progressions, grooves and MIDI patterns by role.
//!
//! Packs are folders of JSON files (`pack.json` + `styles/`, `forms/`, `progressions/`,
//! `grooves/`, `patterns/<role>/`); the `core` pack is written by hand and compiled into
//! the binary, user packs load from [`default_library_dir`]. Invalid packs and items are
//! reported, never fatal. [`Library::search`] ranks items by token match plus filters (no
//! embeddings, no network); [`realize`] turns a pattern into notes for a key and a chord
//! progression. Applying anything to a project is the caller's job (MCP `apply_pattern`
//! builds ordinary undoable commands).
//!
//! Design: `docs/design/style-library.md`.

pub mod format;
pub mod import;
pub mod lanes;
pub mod library;
pub mod realize;
pub mod validate;

pub use format::{
	Ending, Feel, Form, Groove, Harmony, Item, ItemKind, MAX_SUMMARY_CHARS, OrderedMap,
	PackManifest, Pattern, Progression, Role, RoleValue, Section, Style, Variant,
};
pub use lanes::{DRUM_LANES, LaneKey, drum_name, drum_note};
pub use library::{
	CORE_PACK, Card, Issue, LIBRARY_DIR_ENV, Library, LoadedItem, Pack, PackOrigin, PackReport,
	Query, card, default_library_dir, normalize, section_name,
};
pub use realize::{RealizeError, RealizeOptions, Realized, is_roman, parse_chords, realize};

#[cfg(test)]
mod tests;
