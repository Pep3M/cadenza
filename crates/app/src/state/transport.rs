//! Transport modes and the arrange loop region (view state, not part of the project).
//!
//! FL Studio-like play modes: in [`PlayMode::Song`] Play runs the whole arrangement
//! (looping the arrange loop region when it is on); in [`PlayMode::Pattern`] it loops
//! the clip open in the piano roll through its track's instrument, inserts and sends,
//! auditioning only that track (the project's mute/solo is not touched). [`config`]
//! turns the view state into what the player needs; `AppState` sends it.

use cadenza_core::{ClipId, Tick, TimeSignature, TrackId};
use cadenza_model::{Clip, Project};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayMode {
	#[default]
	Song,
	Pattern,
}

impl PlayMode {
	pub fn name(self) -> &'static str {
		match self {
			PlayMode::Song => "Song",
			PlayMode::Pattern => "Pattern",
		}
	}

	pub fn toggled(self) -> Self {
		match self {
			PlayMode::Song => PlayMode::Pattern,
			PlayMode::Pattern => PlayMode::Song,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportView {
	pub mode: PlayMode,
	/// Song mode loops `loop_range`.
	pub loop_on: bool,
	/// The arrange loop region (kept while the loop is off).
	pub loop_range: Option<(Tick, Tick)>,
}

/// What the player plays for the current mode.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayConfig {
	pub loop_range: Option<(Tick, Tick)>,
	/// Tracks auditioned alone (`None`: the song as mixed).
	pub audition: Option<Vec<TrackId>>,
	/// Pattern mode is playing this clip.
	pub pattern: Option<ClipId>,
}

/// The span pattern mode loops for `clip` (FL Studio pattern length): its notes rounded
/// up to the next bar (at least one bar), or the clip's own length when that is longer.
/// Notes drawn past the end grow the clip, and the loop follows on the next tick.
pub fn pattern_range(clip: &Clip, ts: TimeSignature) -> (Tick, Tick) {
	let bar = ts.ticks_per_bar().0.max(1);
	let content = clip.notes().iter().map(|n| n.end().0).max().unwrap_or(0);
	let rounded = Tick(((content.max(0) + bar - 1) / bar).max(1) * bar);
	(clip.start, clip.start + rounded.max(clip.length))
}

/// The player configuration for `view`. `piano_clip` is the clip open in the piano roll;
/// pattern mode without a MIDI clip there falls back to the song (second value: true).
/// `audition` only applies while playing (previews of other tracks stay audible).
pub fn config(
	project: &Project,
	view: &TransportView,
	piano_clip: Option<ClipId>,
	playing: bool,
) -> (PlayConfig, bool) {
	let song = PlayConfig {
		loop_range: view.loop_range.filter(|_| view.loop_on),
		audition: None,
		pattern: None,
	};
	if view.mode == PlayMode::Song {
		return (song, false);
	}
	let found = piano_clip
		.and_then(|id| project.find_clip(id))
		.filter(|(_, c)| c.is_midi());
	match found {
		Some((track, clip)) => (
			PlayConfig {
				loop_range: Some(pattern_range(clip, project.time_signature)),
				audition: playing.then(|| vec![track.id]),
				pattern: Some(clip.id),
			},
			false,
		),
		None => (song, true),
	}
}

/// The loop region dragged from `a` to `b` (either order), snapped to `step` (`None`:
/// free); at least one step (or one tick) long. `None` for a click without a drag.
pub fn loop_from_drag(a: Tick, b: Tick, step: Option<Tick>) -> Option<(Tick, Tick)> {
	let (lo, hi) = (a.min(b).max(Tick::ZERO), a.max(b).max(Tick::ZERO));
	let (lo, hi) = match step.filter(|s| s.0 > 0) {
		Some(s) => {
			let lo = Tick(lo.0 / s.0 * s.0);
			let hi = Tick((hi.0 + s.0 - 1) / s.0 * s.0).max(lo + s);
			(lo, hi)
		}
		None => (lo, hi),
	};
	(hi > lo).then_some((lo, hi))
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::{Track, TrackKind};

	#[test]
	fn pattern_mode_loops_the_open_clip_at_least_a_bar() {
		let mut p = Project::new("p");
		let mut t = Track::new("Keys", TrackKind::Instrument);
		let short = Clip::midi("short", Tick(PPQ * 8), Tick(PPQ));
		let long = Clip::midi("long", Tick(PPQ * 16), Tick(PPQ * 6));
		let (short_id, long_id, track) = (short.id, long.id, t.id);
		t.clips = vec![short, long];
		p.tracks.push(t);
		let mut view = TransportView {
			mode: PlayMode::Pattern,
			loop_on: true,
			loop_range: Some((Tick::ZERO, Tick(PPQ * 4))),
		};
		let (c, fell_back) = config(&p, &view, Some(short_id), true);
		assert!(!fell_back);
		assert_eq!(c.loop_range, Some((Tick(PPQ * 8), Tick(PPQ * 12))));
		assert_eq!(c.audition, Some(vec![track]));
		assert_eq!(c.pattern, Some(short_id));
		let (c, _) = config(&p, &view, Some(long_id), false);
		assert_eq!(c.loop_range, Some((Tick(PPQ * 16), Tick(PPQ * 22))));
		assert_eq!(c.audition, None, "no audition while stopped");
		// Notes decide when they reach further than the clip: rounded up to the bar.
		let ts = p.time_signature;
		let mut clip = Clip::midi("c", Tick(PPQ * 4), Tick(PPQ * 2));
		assert_eq!(
			pattern_range(&clip, ts),
			(Tick(PPQ * 4), Tick(PPQ * 8)),
			"empty: a bar"
		);
		clip = clip.with_notes([cadenza_model::Note::new(60, Tick(PPQ * 4), Tick(PPQ), 0.8)]);
		assert_eq!(
			pattern_range(&clip, ts).1,
			Tick(PPQ * 12),
			"5 beats: two bars"
		);
		clip = clip.with_notes([cadenza_model::Note::new(60, Tick::ZERO, Tick(PPQ * 8), 0.8)]);
		assert_eq!(
			pattern_range(&clip, ts).1,
			Tick(PPQ * 12),
			"exactly two bars"
		);
		// No clip open: the song, with its loop region.
		let (c, fell_back) = config(&p, &view, None, true);
		assert!(fell_back);
		assert_eq!(c.loop_range, view.loop_range);
		assert_eq!(c.audition, None);
		view.mode = PlayMode::Song;
		view.loop_on = false;
		let (c, fell_back) = config(&p, &view, Some(short_id), true);
		assert!(!fell_back);
		assert_eq!(c, PlayConfig::default());
	}

	#[test]
	fn loop_drags_snap_and_order() {
		let bar = Tick(PPQ * 4);
		assert_eq!(
			loop_from_drag(Tick(PPQ * 9), Tick(PPQ * 2), Some(bar)),
			Some((Tick::ZERO, Tick(PPQ * 12)))
		);
		assert_eq!(
			loop_from_drag(Tick(100), Tick(100), Some(bar)),
			Some((Tick::ZERO, bar)),
			"at least one step"
		);
		assert_eq!(loop_from_drag(Tick(100), Tick(100), None), None);
		assert_eq!(
			loop_from_drag(Tick(-50), Tick(300), None),
			Some((Tick::ZERO, Tick(300)))
		);
	}
}
