//! Standard MIDI File import: SMF type 0/1 -> [`ParsedMidiFile`] -> an undoable
//! [`cadenza_model::Command::Batch`].

use std::collections::{BTreeMap, VecDeque};

use cadenza_core::{Meter, MeterChange, Tick, TimeSignature};
use cadenza_model::{
	Clip, ClipContent, Command, Controller, ControllerLane, ControllerPoint, Device, DeviceKind,
	Note, Track, TrackKind,
};
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use crate::error::{MidiIoError, Result};
use crate::ids::{IdAllocator, RandomIds};
use crate::parsed::{
	GM_DRUM_CHANNEL, OtherEvent, ParsedMarker, ParsedMidiFile, ParsedNote, ParsedOtherEvent,
	ParsedTempoPoint, ParsedTrack,
};

/// Where the imported content lands.
#[derive(Debug, Clone)]
pub enum ImportTarget {
	/// Creates one new instrument track (or more, with `split_by_channel`) per source track.
	NewTracks,
	/// Adds a single clip, merging every channel, to an existing track at `position`.
	ExistingTrack {
		track: cadenza_core::TrackId,
		position: Tick,
	},
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
	/// Splits each source track further, one Cadenza track per MIDI channel it uses.
	pub split_by_channel: bool,
	pub target: ImportTarget,
}

impl Default for ImportOptions {
	fn default() -> Self {
		Self {
			split_by_channel: false,
			target: ImportTarget::NewTracks,
		}
	}
}

pub struct ImportResult {
	/// Ready to execute through `Document::execute` (or hand to the AI as a single
	/// undoable step).
	pub command: Command,
	pub warnings: Vec<String>,
	/// Kept so nothing (CCs, pitch bend, program changes) is lost even though the
	/// model can't represent it yet.
	pub parsed: ParsedMidiFile,
}

/// Parses `bytes` and builds the import command with randomly generated ids.
pub fn import_smf(bytes: &[u8], options: &ImportOptions) -> Result<ImportResult> {
	import_smf_with_ids(bytes, options, &mut RandomIds)
}

/// Same as [`import_smf`], but lets the caller control every id the resulting
/// command creates (e.g. for deterministic tests, or to keep ids stable across a
/// re-import).
pub fn import_smf_with_ids(
	bytes: &[u8],
	options: &ImportOptions,
	ids: &mut dyn IdAllocator,
) -> Result<ImportResult> {
	let parsed = parse_smf(bytes)?;
	let mut warnings = parsed.warnings.clone();
	let command = build_import_command(&parsed, options, ids, &mut warnings);
	Ok(ImportResult {
		command,
		warnings,
		parsed,
	})
}

/// Parses a standard MIDI file into the crate's intermediate representation, without
/// touching the Cadenza project model. Useful on its own to inspect CC/pitch-bend data
/// the model can't hold yet.
pub fn parse_smf(bytes: &[u8]) -> Result<ParsedMidiFile> {
	let smf = Smf::parse(bytes).map_err(|e| MidiIoError::Parse(e.to_string()))?;
	if smf.tracks.is_empty() {
		return Err(MidiIoError::EmptyFile);
	}
	let source_ppq = match smf.header.timing {
		Timing::Metrical(ppq) => ppq.as_int() as i64,
		Timing::Timecode(..) => return Err(MidiIoError::UnsupportedTiming),
	};

	let mut tracks = Vec::with_capacity(smf.tracks.len());
	let mut tempo_map = Vec::new();
	let mut time_signatures = Vec::new();
	let mut markers = Vec::new();
	let mut warnings = Vec::new();

	for (track_index, track_events) in smf.tracks.iter().enumerate() {
		let mut abs_tick: i64 = 0;
		let mut name = None;
		let mut notes = Vec::new();
		let mut other_events = Vec::new();
		let mut is_drum = false;
		// FIFO per (channel, pitch) so overlapping same-pitch notes pair in arrival order.
		let mut open: BTreeMap<(u8, u8), VecDeque<(i64, f32)>> = BTreeMap::new();
		let mut dropped_note_offs = 0u32;

		for event in track_events {
			abs_tick += event.delta.as_int() as i64;
			match event.kind {
				TrackEventKind::Midi { channel, message } => {
					let channel = channel.as_int();
					if channel == GM_DRUM_CHANNEL {
						is_drum = true;
					}
					match message {
						MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
							open.entry((channel, key.as_int()))
								.or_default()
								.push_back((abs_tick, vel.as_int() as f32 / 127.0));
						}
						MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
							let opened = open
								.get_mut(&(channel, key.as_int()))
								.and_then(|queue| queue.pop_front());
							match opened {
								Some((start, velocity)) => notes.push(ParsedNote {
									channel,
									pitch: key.as_int(),
									velocity,
									start: Tick(start),
									length: Tick((abs_tick - start).max(1)),
								}),
								None => dropped_note_offs += 1,
							}
						}
						MidiMessage::Controller { controller, value } => {
							other_events.push(ParsedOtherEvent {
								tick: Tick(abs_tick),
								event: OtherEvent::Controller {
									channel,
									controller: controller.as_int(),
									value: value.as_int(),
								},
							});
						}
						MidiMessage::ProgramChange { program } => {
							other_events.push(ParsedOtherEvent {
								tick: Tick(abs_tick),
								event: OtherEvent::ProgramChange {
									channel,
									program: program.as_int(),
								},
							});
						}
						MidiMessage::PitchBend { bend } => {
							other_events.push(ParsedOtherEvent {
								tick: Tick(abs_tick),
								event: OtherEvent::PitchBend {
									channel,
									bend: bend.as_int(),
								},
							});
						}
						MidiMessage::ChannelAftertouch { vel } => {
							other_events.push(ParsedOtherEvent {
								tick: Tick(abs_tick),
								event: OtherEvent::ChannelPressure {
									channel,
									value: vel.as_int(),
								},
							});
						}
						MidiMessage::Aftertouch { .. } => {
							// Polyphonic aftertouch has no lane in the model; dropped.
						}
					}
				}
				TrackEventKind::Meta(meta) => match meta {
					MetaMessage::Tempo(microseconds_per_quarter) => {
						let bpm = 60_000_000.0 / microseconds_per_quarter.as_int() as f64;
						tempo_map.push((abs_tick, bpm));
					}
					MetaMessage::TimeSignature(numerator, denom_pow2, ..) => {
						time_signatures.push((
							abs_tick,
							TimeSignature {
								numerator: numerator as u32,
								denominator: 1u32 << denom_pow2,
							},
						));
					}
					MetaMessage::TrackName(bytes) if !bytes.is_empty() => {
						name = Some(String::from_utf8_lossy(bytes).trim().to_string());
					}
					MetaMessage::Marker(bytes) if !bytes.is_empty() => {
						markers.push((abs_tick, String::from_utf8_lossy(bytes).trim().to_string()));
					}
					_ => {}
				},
				TrackEventKind::SysEx(_) | TrackEventKind::Escape(_) => {}
			}
		}

		let mut unterminated = 0u32;
		for ((channel, pitch), queue) in open {
			for (start, velocity) in queue {
				notes.push(ParsedNote {
					channel,
					pitch,
					velocity,
					start: Tick(start),
					length: Tick((abs_tick - start).max(1)),
				});
				unterminated += 1;
			}
		}
		if unterminated > 0 {
			warnings.push(format!(
				"track {track_index}: {unterminated} note(s) had no note-off and were closed at the end of the track"
			));
		}
		if dropped_note_offs > 0 {
			warnings.push(format!(
				"track {track_index}: {dropped_note_offs} note-off message(s) had no matching note-on and were dropped"
			));
		}

		notes.sort_by_key(|n| (n.start, n.pitch));
		tracks.push(ParsedTrack {
			name,
			notes,
			is_drum,
			other_events,
		});
	}

	let programs = tracks
		.iter()
		.flat_map(|t| &t.other_events)
		.filter(|e| matches!(e.event, OtherEvent::ProgramChange { .. }))
		.count();
	if programs > 0 {
		warnings.push(format!(
			"{programs} program-change event(s) have no Cadenza equivalent (pick the instrument or preset instead); kept in ParsedMidiFile::other_events"
		));
	}

	let rescale = |tick: i64| -> Tick {
		Tick(((tick as f64) * cadenza_core::PPQ as f64 / source_ppq as f64).round() as i64)
	};
	for track in &mut tracks {
		for note in &mut track.notes {
			note.start = rescale(note.start.0);
			note.length = Tick(rescale(note.length.0).0.max(1));
		}
		for event in &mut track.other_events {
			event.tick = rescale(event.tick.0);
		}
	}
	let tempo_map = tempo_map
		.into_iter()
		.map(|(tick, bpm)| ParsedTempoPoint {
			tick: rescale(tick),
			bpm,
		})
		.collect();
	let time_signatures = time_signatures
		.into_iter()
		.map(|(tick, sig)| (rescale(tick), sig))
		.collect();
	let markers = markers
		.into_iter()
		.map(|(tick, name)| ParsedMarker {
			tick: rescale(tick),
			name,
		})
		.collect();

	Ok(ParsedMidiFile {
		tracks,
		tempo_map,
		time_signatures,
		markers,
		warnings,
	})
}

/// One group of notes that will become a single Cadenza instrument track + clip.
struct NoteGroup {
	name: String,
	is_drum: bool,
	notes: Vec<ParsedNote>,
	controllers: Vec<ControllerLane>,
}

/// Controller lanes from parsed channel events (absolute ticks; `channel` = only that
/// channel), CC/bend/pressure values normalized like the model.
pub fn controller_lanes(events: &[ParsedOtherEvent], channel: Option<u8>) -> Vec<ControllerLane> {
	let mut lanes: BTreeMap<Controller, Vec<ControllerPoint>> = BTreeMap::new();
	for e in events
		.iter()
		.filter(|e| channel.is_none_or(|c| e.event.channel() == c))
	{
		let (controller, value) = match e.event {
			OtherEvent::Controller {
				controller, value, ..
			} => (
				Controller::Cc(controller.min(127)),
				f32::from(value.min(127)) / 127.0,
			),
			OtherEvent::PitchBend { bend, .. } => {
				let v = if bend < 0 {
					f32::from(bend) / 8192.0
				} else {
					f32::from(bend) / 8191.0
				};
				(Controller::PitchBend, v.clamp(-1.0, 1.0))
			}
			OtherEvent::ChannelPressure { value, .. } => (
				Controller::ChannelPressure,
				f32::from(value.min(127)) / 127.0,
			),
			OtherEvent::ProgramChange { .. } => continue,
		};
		let points = lanes.entry(controller).or_default();
		match points.last_mut() {
			Some(last) if last.tick == e.tick => last.value = value,
			_ => points.push(ControllerPoint::new(e.tick, value)),
		}
	}
	lanes
		.into_iter()
		.map(|(controller, mut points)| {
			points.sort_by_key(|p| p.tick);
			ControllerLane { controller, points }
		})
		.collect()
}

fn lanes_end(lanes: &[ControllerLane]) -> Tick {
	lanes
		.iter()
		.filter_map(|l| l.points.last())
		.map(|p| p.tick + Tick(1))
		.max()
		.unwrap_or(Tick::ZERO)
}

fn note_groups(parsed: &ParsedMidiFile, split_by_channel: bool) -> Vec<NoteGroup> {
	let mut groups = Vec::new();
	for (index, track) in parsed.tracks.iter().enumerate() {
		if track.notes.is_empty() {
			continue;
		}
		let default_name = || format!("MIDI track {}", index + 1);
		if !split_by_channel {
			groups.push(NoteGroup {
				name: track.name.clone().unwrap_or_else(default_name),
				is_drum: track.is_drum,
				notes: track.notes.clone(),
				controllers: controller_lanes(&track.other_events, None),
			});
			continue;
		}
		let mut by_channel: BTreeMap<u8, Vec<ParsedNote>> = BTreeMap::new();
		for note in &track.notes {
			by_channel
				.entry(note.channel)
				.or_default()
				.push(note.clone());
		}
		let base_name = track.name.clone().unwrap_or_else(default_name);
		let single_channel = by_channel.len() == 1;
		for (channel, notes) in by_channel {
			let name = if single_channel {
				base_name.clone()
			} else {
				format!("{base_name} (ch {})", channel + 1)
			};
			groups.push(NoteGroup {
				name,
				is_drum: channel == GM_DRUM_CHANNEL,
				notes,
				controllers: controller_lanes(&track.other_events, Some(channel)),
			});
		}
	}
	groups
}

fn builtin_instrument(is_drum: bool) -> Device {
	Device {
		id: cadenza_core::DeviceId::new(),
		kind: DeviceKind::Builtin {
			id: if is_drum {
				"drum_machine"
			} else {
				"poly_synth"
			}
			.to_string(),
		},
		name: if is_drum {
			"Drum Machine"
		} else {
			"Poly Synth"
		}
		.to_string(),
		bypass: false,
		params: Default::default(),
		state: None,
		sidechain: None,
		aux_outputs: Vec::new(),
		zones: Vec::new(),
	}
}

fn clip_length(notes: &[ParsedNote]) -> Tick {
	notes
		.iter()
		.map(|n| n.start + n.length)
		.max()
		.unwrap_or(Tick(cadenza_core::PPQ))
}

/// Bar-indexed time signatures from the file's (tick, signature) events: the signature of
/// bar 1 (`None` when the file has none) and the changes after it. A change that does not
/// fall on a bar line of the signatures before it moves to the next bar line (warned);
/// repeated signatures are dropped.
pub fn meter_changes(
	signatures: &[(Tick, TimeSignature)],
	warnings: &mut Vec<String>,
) -> (Option<TimeSignature>, Vec<MeterChange>) {
	let mut sorted = signatures.to_vec();
	sorted.sort_by_key(|(tick, _)| *tick);
	let mut first: Option<TimeSignature> = None;
	let mut changes: Vec<MeterChange> = Vec::new();
	for (tick, sig) in sorted {
		if tick <= Tick::ZERO {
			first = Some(sig);
			continue;
		}
		let base = first.unwrap_or_default();
		let meter = Meter::new(base, &changes);
		if meter.signature_at(tick) == sig {
			continue;
		}
		let bar = meter.bar_at(tick);
		let index = if bar.start == tick {
			bar.index
		} else {
			warnings.push(format!(
				"time signature {}/{} at beat {} is not on a bar line; moved to bar {}",
				sig.numerator,
				sig.denominator,
				tick.beats(),
				bar.index + 2
			));
			bar.index + 1
		};
		let index = index.clamp(1, i64::from(u32::MAX)) as u32;
		match changes.last_mut() {
			Some(last) if last.bar == index => *last = MeterChange::new(index, sig),
			_ => changes.push(MeterChange::new(index, sig)),
		}
	}
	if first.is_none() && !changes.is_empty() {
		first = Some(TimeSignature::default());
	}
	(first, changes)
}

/// Builds the undoable command that applies `parsed` to a project, per `options`.
pub fn build_import_command(
	parsed: &ParsedMidiFile,
	options: &ImportOptions,
	ids: &mut dyn IdAllocator,
	warnings: &mut Vec<String>,
) -> Command {
	let mut commands = Vec::new();

	for point in &parsed.tempo_map {
		commands.push(Command::SetTempo {
			tick: point.tick,
			bpm: point.bpm,
		});
	}
	let (first, changes) = meter_changes(&parsed.time_signatures, warnings);
	if let Some(first) = first {
		commands.push(Command::SetTimeSignature {
			time_signature: first,
			bar: 0,
		});
	}
	for change in changes {
		commands.push(Command::SetTimeSignature {
			time_signature: change.signature(),
			bar: change.bar,
		});
	}
	for marker in &parsed.markers {
		commands.push(Command::AddMarker {
			marker: cadenza_model::Marker {
				tick: marker.tick,
				name: marker.name.clone(),
			},
		});
	}

	match &options.target {
		ImportTarget::NewTracks => {
			for group in note_groups(parsed, options.split_by_channel) {
				let clip_id = ids.clip_id();
				let notes: Vec<Note> = group
					.notes
					.iter()
					.map(|n| Note {
						id: ids.note_id(),
						pitch: n.pitch,
						velocity: n.velocity,
						start: n.start,
						length: n.length,
					})
					.collect();
				let clip = Clip {
					id: clip_id,
					name: group.name.clone(),
					start: Tick::ZERO,
					length: clip_length(&group.notes).max(lanes_end(&group.controllers)),
					content: ClipContent::Midi {
						notes,
						controllers: group.controllers,
					},
					take: None,
				};
				let track = Track {
					id: ids.track_id(),
					name: group.name,
					kind: TrackKind::Instrument,
					color: None,
					instrument: Some(builtin_instrument(group.is_drum)),
					clips: vec![clip],
					channel: Default::default(),
					input: None,
				};
				commands.push(Command::AddTrack { track, index: None });
			}
		}
		ImportTarget::ExistingTrack { track, position } => {
			let total_tracks = parsed.tracks.iter().filter(|t| !t.notes.is_empty()).count();
			if total_tracks > 1 {
				warnings.push(format!(
					"importing into an existing track merges all {total_tracks} source tracks/channels into a single clip"
				));
			}
			let all_notes: Vec<ParsedNote> =
				parsed.tracks.iter().flat_map(|t| t.notes.clone()).collect();
			let notes: Vec<Note> = all_notes
				.iter()
				.map(|n| Note {
					id: ids.note_id(),
					pitch: n.pitch,
					velocity: n.velocity,
					start: n.start,
					length: n.length,
				})
				.collect();
			let all_events: Vec<ParsedOtherEvent> = parsed
				.tracks
				.iter()
				.flat_map(|t| t.other_events.clone())
				.collect();
			let controllers = controller_lanes(&all_events, None);
			let clip = Clip {
				id: ids.clip_id(),
				name: "Imported MIDI".to_string(),
				start: *position,
				length: clip_length(&all_notes).max(lanes_end(&controllers)),
				content: ClipContent::Midi { notes, controllers },
				take: None,
			};
			commands.push(Command::AddClip {
				track: *track,
				clip,
			});
		}
	}

	Command::Batch {
		label: "Import MIDI file".to_string(),
		commands,
	}
}
