//! MPE (MIDI Polyphonic Expression) on import: a file whose MPE Configuration Message
//! (RPN 6 on channel 1 or 16) announces a zone plays each note on a member channel of its
//! own, bending it with that channel's pitch bend (and shaping it with its channel pressure
//! and CC74 timbre). Those become the notes' own curves (`Note.bend`, `pressure`, `timbre`), the notes move to the zone's master channel (one instrument, and a member
//! channel 10 is not a drum kit), and the member channels' events leave the lanes.

use std::collections::BTreeMap;

use cadenza_core::Tick;
use cadenza_model::{
	MAX_NOTE_BEND, MAX_NOTE_BEND_POINTS, NoteBendPoint, NoteExpression, NoteValuePoint,
};

use crate::parsed::{MpeLayout, MpeZone, OtherEvent, ParsedMidiFile, ParsedOtherEvent};

/// Bend range of a member channel unless RPN 0 says otherwise (MPE default).
const MEMBER_RANGE: f32 = 48.0;
/// Bend range of a master channel unless RPN 0 says otherwise (MPE default).
const MASTER_RANGE: f32 = 2.0;
/// MPE's per-note timbre ("slide") controller.
const TIMBRE_CC: u8 = 74;
/// The channel pitch-bend range Cadenza's instruments play.
const PLAYED_RANGE: f32 = 2.0;

/// RPN / NRPN selection and data entry controllers: configuration, never a lane here.
fn is_parameter_cc(controller: u8) -> bool {
	matches!(controller, 6 | 38 | 98 | 99 | 100 | 101)
}

/// Reads the file's MPE layout: zones from MCMs and bend ranges from RPN 0, following
/// each channel's RPN selection in time order across all tracks.
pub fn layout(parsed: &ParsedMidiFile) -> Option<MpeLayout> {
	let mut events: Vec<&ParsedOtherEvent> =
		parsed.tracks.iter().flat_map(|t| &t.other_events).collect();
	events.sort_by_key(|e| e.tick);
	let mut selected = [(127u8, 127u8); 16];
	let mut ranges: [Option<f32>; 16] = [None; 16];
	let mut lower: Option<u8> = None;
	let mut upper: Option<u8> = None;
	for e in events {
		let OtherEvent::Controller {
			channel,
			controller,
			value,
		} = e.event
		else {
			continue;
		};
		let ch = usize::from(channel & 15);
		match controller {
			101 => selected[ch].0 = value,
			100 => selected[ch].1 = value,
			6 => match selected[ch] {
				(0, 6) if channel == 0 => lower = Some(value.min(15)),
				(0, 6) if channel == 15 => upper = Some(value.min(15)),
				(0, 0) => ranges[ch] = Some(f32::from(value)),
				_ => {}
			},
			38 if selected[ch] == (0, 0) => {
				ranges[ch] =
					Some(ranges[ch].unwrap_or(0.0).trunc() + f32::from(value.min(99)) / 100.0);
			}
			_ => {}
		}
	}
	let mut zones = Vec::new();
	let lower_n = lower.unwrap_or(0);
	if lower_n > 0 {
		zones.push(MpeZone {
			master: 0,
			members: (1..=lower_n).collect(),
		});
	}
	if let Some(n) = upper.filter(|&n| n > 0) {
		// The upper zone gives way to the lower one where they overlap (MPE 1.0).
		let members: Vec<u8> = (15 - n.min(14)..15)
			.rev()
			.filter(|&c| c > lower_n)
			.collect();
		if !members.is_empty() {
			zones.push(MpeZone {
				master: 15,
				members,
			});
		}
	}
	if zones.is_empty() {
		return None;
	}
	let mut bend_ranges = [MEMBER_RANGE; 16];
	for z in &zones {
		bend_ranges[usize::from(z.master)] = MASTER_RANGE;
	}
	for (ch, r) in ranges.iter().enumerate() {
		if let Some(r) = r {
			bend_ranges[ch] = *r;
		}
	}
	Some(MpeLayout { zones, bend_ranges })
}

/// Turns an MPE file's member-channel bends into note curves (see the module docs); a
/// file without an MPE zone is left as it is.
pub fn apply(parsed: &mut ParsedMidiFile) {
	let Some(mpe) = layout(parsed) else {
		return;
	};
	let mut bent = 0usize;
	let mut expressive = 0usize;
	let mut notes = 0usize;
	let mut dropped = 0usize;
	let mut master_bends = false;
	for track in &mut parsed.tracks {
		// Each member channel's bends (-1..1), pressure and timbre (0..1), in time order.
		let mut bends: BTreeMap<u8, Vec<(Tick, f32)>> = BTreeMap::new();
		let mut pressure: BTreeMap<u8, Vec<(Tick, f32)>> = BTreeMap::new();
		let mut timbre: BTreeMap<u8, Vec<(Tick, f32)>> = BTreeMap::new();
		for e in &track.other_events {
			let channel = e.event.channel();
			if mpe.master_of(channel).is_none() {
				continue;
			}
			match e.event {
				OtherEvent::PitchBend { bend, .. } => {
					let v = if bend < 0 {
						f32::from(bend) / 8192.0
					} else {
						f32::from(bend) / 8191.0
					};
					bends.entry(channel).or_default().push((e.tick, v));
				}
				OtherEvent::ChannelPressure { value, .. } => pressure
					.entry(channel)
					.or_default()
					.push((e.tick, f32::from(value.min(127)) / 127.0)),
				OtherEvent::Controller {
					controller: TIMBRE_CC,
					value,
					..
				} => {
					// 64 is MPE's neutral timbre: exactly the model's resting 0.5.
					let v = if value == 64 {
						0.5
					} else {
						f32::from(value.min(127)) / 127.0
					};
					timbre.entry(channel).or_default().push((e.tick, v))
				}
				_ => {}
			}
		}
		for list in bends
			.values_mut()
			.chain(pressure.values_mut())
			.chain(timbre.values_mut())
		{
			list.sort_by_key(|(t, _)| *t);
		}
		for note in &mut track.notes {
			let Some(master) = mpe.master_of(note.channel) else {
				continue;
			};
			notes += 1;
			let range = mpe.bend_ranges[usize::from(note.channel)];
			let (start, end) = (note.start, note.start + note.length);
			if let Some(list) = bends.get(&note.channel) {
				note.bend = held_curve(list, start, end, 0.0, |v| {
					(v * range).clamp(-MAX_NOTE_BEND, MAX_NOTE_BEND)
				})
				.into_iter()
				.map(|(tick, semitones)| NoteBendPoint { tick, semitones })
				.collect();
				bent += usize::from(!note.bend.is_empty());
			}
			let values =
				|list: Option<&Vec<(Tick, f32)>>, kind: NoteExpression| -> Vec<NoteValuePoint> {
					list.map(|l| {
						held_curve(l, start, end, kind.resting(), |v| v.clamp(0.0, 1.0))
							.into_iter()
							.map(|(tick, value)| NoteValuePoint { tick, value })
							.collect()
					})
					.unwrap_or_default()
				};
			note.pressure = values(pressure.get(&note.channel), NoteExpression::Pressure);
			note.timbre = values(timbre.get(&note.channel), NoteExpression::Timbre);
			expressive += usize::from(!note.pressure.is_empty() || !note.timbre.is_empty());
			note.channel = master;
		}
		track.notes.sort_by_key(|n| (n.start, n.pitch));
		track.other_events.retain(|e| {
			let channel = e.event.channel();
			let member = mpe.master_of(channel).is_some();
			match e.event {
				OtherEvent::Controller { controller, .. } if is_parameter_cc(controller) => {
					!(member || mpe.zones.iter().any(|z| z.master == channel))
				}
				OtherEvent::PitchBend { .. } if member => false,
				OtherEvent::PitchBend { .. } => {
					master_bends |= mpe.zones.iter().any(|z| z.master == channel);
					true
				}
				OtherEvent::ProgramChange { .. } => true,
				OtherEvent::ChannelPressure { .. }
				| OtherEvent::Controller {
					controller: TIMBRE_CC,
					..
				} if member => false,
				_ if member => {
					dropped += 1;
					false
				}
				_ => true,
			}
		});
		if mpe.master_of(crate::parsed::GM_DRUM_CHANNEL).is_some() {
			track.is_drum = track
				.notes
				.iter()
				.any(|n| n.channel == crate::parsed::GM_DRUM_CHANNEL);
		}
	}
	parsed.warnings.push(format!(
		"MPE file: {notes} note(s) on member channels imported on one instrument, {bent} with their own pitch curve, {expressive} with their own pressure or timbre"
	));
	if dropped > 0 {
		parsed.warnings.push(format!(
			"MPE: {dropped} per-note controller event(s) (other than CC74 timbre) have no per-note lane in Cadenza and were dropped"
		));
	}
	for z in &mpe.zones {
		let range = mpe.bend_ranges[usize::from(z.master)];
		if master_bends && (range - PLAYED_RANGE).abs() > 1e-3 {
			parsed.warnings.push(format!(
				"MPE: the master channel's pitch-bend range is {range} semitones; Cadenza plays its pitch-bend lane at ±{PLAYED_RANGE}"
			));
		}
	}
	parsed.mpe = Some(mpe);
}

/// One note's curve from its member channel's events: the value in effect at the note-on
/// (events sent at or before it), then every change before its end, as (ticks from the
/// note's start, `map(value)`), dropping values that repeat the held one (`resting` before
/// the first).
fn held_curve(
	events: &[(Tick, f32)],
	start: Tick,
	end: Tick,
	resting: f32,
	map: impl Fn(f32) -> f32,
) -> Vec<(Tick, f32)> {
	let i = events.partition_point(|(t, _)| *t <= start);
	let first = (i > 0).then(|| (start, events[i - 1].1));
	let inside = events[i..].iter().copied().take_while(|(t, _)| *t < end);
	let mut held = resting;
	let mut out: Vec<(Tick, f32)> = Vec::new();
	for (t, v) in first.into_iter().chain(inside) {
		let value = map(v);
		if (value - held).abs() <= 1e-3 {
			continue;
		}
		held = value;
		let tick = t - start;
		match out.last_mut() {
			Some((last, v)) if *last == tick => *v = value,
			_ => out.push((tick, value)),
		}
		if out.len() == MAX_NOTE_BEND_POINTS {
			break;
		}
	}
	out
}
