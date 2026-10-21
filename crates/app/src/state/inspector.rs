//! Inspector tabs (Track | Clip | Device): what each tab shows for the selection, which
//! tab is active and how it follows the selection. View state, not in the project.

use crate::i18n::{tr, trf};
use cadenza_core::{ClipId, Tick, TimeSignature, TrackId};
use cadenza_model::{ClipContent, Project};

use super::selection::{DeviceRef, Focus, Selection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InspectorTab {
	Track,
	Clip,
	Device,
}

impl InspectorTab {
	pub const ALL: [Self; 3] = [Self::Track, Self::Clip, Self::Device];

	pub fn label(self) -> &'static str {
		match self {
			Self::Track => tr("Track"),
			Self::Clip => tr("Clip"),
			Self::Device => tr("Device"),
		}
	}

	pub fn index(self) -> usize {
		Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
	}
}

/// The tab a selection asks for: the most specific selected thing.
pub fn tab_for(focus: Focus) -> InspectorTab {
	match focus {
		Focus::Device(_) => InspectorTab::Device,
		Focus::Clip(_) => InspectorTab::Clip,
		Focus::Track(_) | Focus::Master | Focus::Nothing => InspectorTab::Track,
	}
}

/// The active tab: follows the selection, until the user picks another tab (which then
/// holds until the selection changes again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectorView {
	pub tab: InspectorTab,
	/// The selection the tab last followed.
	seen: Option<Focus>,
	/// The history footer is folded.
	pub history_folded: bool,
}

impl Default for InspectorView {
	fn default() -> Self {
		Self {
			tab: InspectorTab::Track,
			seen: None,
			history_folded: false,
		}
	}
}

impl InspectorView {
	/// Switches to the selection's tab when the selection changed since the last call.
	pub fn follow(&mut self, focus: Focus) -> InspectorTab {
		if self.seen != Some(focus) {
			self.seen = Some(focus);
			self.tab = tab_for(focus);
		}
		self.tab
	}

	/// A click on a tab.
	pub fn pick(&mut self, tab: InspectorTab) {
		self.tab = tab;
	}
}

/// What each tab edits for a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Targets {
	/// `Some(None)` = the master channel.
	pub track: Option<Option<TrackId>>,
	pub clip: Option<ClipId>,
	/// The selected device, else the current track's instrument.
	pub device: Option<DeviceRef>,
	/// Selected clips (several: the Clip tab asks for one).
	pub clips: usize,
}

impl Targets {
	pub fn of(selection: &Selection, project: &Project) -> Self {
		let track = match (selection.track, selection.master) {
			(Some(t), _) if project.track(t).is_some() => Some(Some(t)),
			(None, true) => Some(None),
			_ => None,
		};
		let clip = (selection.clips.len() == 1)
			.then(|| selection.clips.first().copied())
			.flatten()
			.filter(|c| project.find_clip(*c).is_some());
		let device = selection.device.or_else(|| {
			let t = project.track(selection.track?)?;
			t.instrument.as_ref().map(|d| DeviceRef {
				track: Some(t.id),
				device: d.id,
			})
		});
		Self {
			track,
			clip,
			device,
			clips: selection.clips.len(),
		}
	}

	pub fn has(&self, tab: InspectorTab) -> bool {
		match tab {
			InspectorTab::Track => self.track.is_some(),
			InspectorTab::Clip => self.clip.is_some(),
			InspectorTab::Device => self.device.is_some(),
		}
	}

	/// What to do to fill an empty tab (also its tooltip).
	pub fn hint(&self, tab: InspectorTab) -> String {
		match tab {
			InspectorTab::Track => {
				tr("Select a track header or a mixer strip to edit its channel").into()
			}
			InspectorTab::Clip if self.clips > 1 => trf(
				"{clips} clips selected: click one to edit its properties",
				&[("clips", &self.clips)],
			),
			InspectorTab::Clip => tr("Select a clip in the arrange to edit it here").into(),
			InspectorTab::Device => tr(
				"Select an insert or an instrument (mixer, track header, Track tab) to edit it \
				 here",
			)
			.into(),
		}
	}
}

/// Note count, pitch and velocity ranges of a MIDI clip.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NoteStats {
	pub count: usize,
	/// Lowest and highest pitch.
	pub pitch: Option<(u8, u8)>,
	/// Softest and loudest velocity (0..1).
	pub velocity: Option<(f32, f32)>,
}

/// [`NoteStats`] of a MIDI clip (`None` for audio).
pub fn note_stats(content: &ClipContent) -> Option<NoteStats> {
	let ClipContent::Midi { notes, .. } = content else {
		return None;
	};
	let mut stats = NoteStats {
		count: notes.len(),
		..NoteStats::default()
	};
	for n in notes {
		stats.pitch = Some(stats.pitch.map_or((n.pitch, n.pitch), |(lo, hi)| {
			(lo.min(n.pitch), hi.max(n.pitch))
		}));
		stats.velocity = Some(stats.velocity.map_or((n.velocity, n.velocity), |(lo, hi)| {
			(lo.min(n.velocity), hi.max(n.velocity))
		}));
	}
	Some(stats)
}

/// A duration as bars.beats.ticks in `ts` (0-based counts), e.g. `4.0.000`, `0.3.480`.
pub fn format_length(length: Tick, ts: TimeSignature) -> String {
	let (bar, beat) = units(ts);
	let t = length.0.max(0);
	format!("{}.{}.{:03}", t / bar, (t % bar) / beat, t % beat)
}

/// Parses [`format_length`] text: `bars[.beats[.ticks]]` (fractions allowed in the last
/// part, `"2.2"` = two bars and two beats). `None` for unreadable or non-positive text.
pub fn parse_length(text: &str, ts: TimeSignature) -> Option<Tick> {
	let (bar, beat) = units(ts);
	let parts = parts(text)?;
	let scale = [bar as f64, beat as f64, 1.0];
	let ticks: f64 = parts.iter().zip(scale).map(|(v, s)| v * s).sum();
	let ticks = ticks.round() as i64;
	(ticks > 0).then_some(Tick(ticks))
}

/// Parses a 1-based position `bar[.beat[.ticks]]` (as [`super::timeline::format_bbt`]
/// writes it) through `meter`; `None` before bar 1 or for unreadable text.
pub fn parse_position(text: &str, meter: cadenza_core::Meter<'_>) -> Option<Tick> {
	let parts = parts(text)?;
	let bar = parts[0];
	let beat = parts.get(1).copied().unwrap_or(1.0);
	let ticks = parts.get(2).copied().unwrap_or(0.0);
	if bar < 1.0 || bar.fract() != 0.0 || beat < 1.0 {
		return None;
	}
	let at = meter.tick_at_bbt(bar as i64, beat).0 + ticks.round() as i64;
	Some(Tick(at.max(0)))
}

fn parts(text: &str) -> Option<Vec<f64>> {
	let text = text.trim();
	if text.is_empty() {
		return None;
	}
	let parts: Vec<f64> = text
		.split(['.', ':'])
		.map(|p| p.trim().parse::<f64>().ok().filter(|v| *v >= 0.0))
		.collect::<Option<_>>()?;
	(parts.len() <= 3).then_some(parts)
}

fn units(ts: TimeSignature) -> (i64, i64) {
	let beat = (cadenza_core::PPQ * 4 / i64::from(ts.denominator.max(1))).max(1);
	(beat * i64::from(ts.numerator.max(1)), beat)
}

/// History rows made by the AI (badged in the footer).
pub fn is_ai_step(label: &str) -> bool {
	label.starts_with("AI:") || label.starts_with("AI ")
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::{Clip, Device, Note, Track, TrackKind};

	const FOUR: TimeSignature = TimeSignature {
		numerator: 4,
		denominator: 4,
	};

	#[test]
	fn the_tab_follows_the_selection_until_the_user_picks_one() {
		let t = TrackId::new();
		let c = ClipId::new();
		let mut v = InspectorView::default();
		assert_eq!(v.follow(Focus::Nothing), InspectorTab::Track);
		assert_eq!(v.follow(Focus::Clip(c)), InspectorTab::Clip);
		// A picked tab holds while the selection stays.
		v.pick(InspectorTab::Track);
		assert_eq!(v.follow(Focus::Clip(c)), InspectorTab::Track);
		let d = DeviceRef {
			track: Some(t),
			device: cadenza_core::DeviceId::new(),
		};
		assert_eq!(v.follow(Focus::Device(d)), InspectorTab::Device);
		assert_eq!(v.follow(Focus::Track(t)), InspectorTab::Track);
		assert_eq!(v.follow(Focus::Master), InspectorTab::Track);
		// Back to the same clip: a new selection, so it follows again.
		assert_eq!(v.follow(Focus::Clip(c)), InspectorTab::Clip);
	}

	#[test]
	fn targets_fall_back_to_the_track_instrument_and_ask_for_one_clip() {
		let mut p = Project::new("p");
		let mut keys = Track::new("Keys", TrackKind::Instrument);
		let piano = Device::builtin("electric_piano", "Electric Piano");
		keys.instrument = Some(piano.clone());
		let a = Clip::midi("a", Tick::ZERO, Tick(PPQ * 4));
		let b = Clip::midi("b", Tick(PPQ * 4), Tick(PPQ * 4));
		let (ka, kb) = (a.id, b.id);
		keys.clips.extend([a, b]);
		let kid = keys.id;
		p.tracks.push(keys);
		let mut s = Selection::default();
		let none = Targets::of(&s, &p);
		assert!(InspectorTab::ALL.iter().all(|t| !none.has(*t)));
		s.click_clip(kid, ka, false);
		let one = Targets::of(&s, &p);
		assert_eq!(one.clip, Some(ka));
		assert_eq!(one.track, Some(Some(kid)));
		assert_eq!(
			one.device,
			Some(DeviceRef {
				track: Some(kid),
				device: piano.id
			}),
			"the track's instrument"
		);
		s.click_clip(kid, kb, true);
		let two = Targets::of(&s, &p);
		assert!(!two.has(InspectorTab::Clip));
		assert!(two.hint(InspectorTab::Clip).starts_with("2 clips"));
		s.select_master();
		let master = Targets::of(&s, &p);
		assert_eq!(master.track, Some(None));
		assert_eq!(master.device, None);
	}

	#[test]
	fn lengths_and_positions_read_and_write_bars_beats_ticks() {
		assert_eq!(format_length(Tick(PPQ * 16), FOUR), "4.0.000");
		assert_eq!(format_length(Tick(PPQ * 3 + 480), FOUR), "0.3.480");
		assert_eq!(parse_length("4.0.000", FOUR), Some(Tick(PPQ * 16)));
		assert_eq!(parse_length("2.2", FOUR), Some(Tick(PPQ * 10)));
		assert_eq!(parse_length("0.0.0", FOUR), None);
		assert_eq!(parse_length("x", FOUR), None);
		let eight = TimeSignature {
			numerator: 6,
			denominator: 8,
		};
		assert_eq!(format_length(Tick(PPQ * 3), eight), "1.0.000");
		let meter = cadenza_core::Meter::new(FOUR, &[]);
		assert_eq!(parse_position("5.1.000", meter), Some(Tick(PPQ * 16)));
		assert_eq!(parse_position("2.3", meter), Some(Tick(PPQ * 6)));
		assert_eq!(parse_position("1.1.240", meter), Some(Tick(240)));
		assert_eq!(parse_position("0.1", meter), None);
		assert_eq!(parse_position("1.2.3.4", meter), None);
	}

	#[test]
	fn note_stats_cover_pitch_and_velocity() {
		let mut c = Clip::midi("c", Tick::ZERO, Tick(PPQ * 4));
		assert_eq!(note_stats(&c.content), Some(NoteStats::default()));
		if let ClipContent::Midi { notes, .. } = &mut c.content {
			notes.push(Note::new(60, Tick::ZERO, Tick(PPQ), 0.5));
			notes.push(Note::new(72, Tick(PPQ), Tick(PPQ), 0.9));
			notes.push(Note::new(55, Tick(PPQ * 2), Tick(PPQ), 0.7));
		}
		assert_eq!(
			note_stats(&c.content),
			Some(NoteStats {
				count: 3,
				pitch: Some((55, 72)),
				velocity: Some((0.5, 0.9)),
			})
		);
		assert!(is_ai_step("AI: add drums") && is_ai_step("AI mix pass"));
		assert!(!is_ai_step("Add track 'AIR'"));
	}
}
