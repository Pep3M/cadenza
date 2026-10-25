//! "Convert to MIDI…" on an audio clip (NE-3): the clip's file is analyzed and segmented on
//! a worker (`cadenza_mcp::tools::notes::analyze_clip`, cached per content), the dialog
//! previews the conversion while its options change (pure, cheap) and Convert writes it as
//! ONE undo step, then opens the new clip in the piano roll.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};

use cadenza_core::{ClipId, TrackId};
use cadenza_engine::{RecordOptions, RecordedAudio, Recording, record_commands};
use cadenza_mcp::tools::notes::{
	Algorithm, AudioToMidiArgs, AudioToMidiOutput, ClipNotes, ConvertMode, analyze_clip,
	conversion_command, convert, default_cache_dir, detect_for, midi_options,
};
use cadenza_model::{Command, Document, Project, Track, TrackKind};
use cadenza_notes::midi::MidiOptions;

use super::AppState;
use crate::i18n::{tr, trf, trn};

/// Where a conversion is.
#[derive(Debug, Clone)]
pub enum ConvertPhase {
	Analyzing,
	Ready(Box<ClipNotes>),
	Failed(String),
}

/// One preview note for the dialog's picture: (song beat, length, pitch).
pub type PreviewNote = (f64, f64, u8);

/// What Convert would write with the current options.
#[derive(Debug, Clone)]
pub struct ConvertPreview {
	pub summary: AudioToMidiOutput,
	pub notes: Vec<PreviewNote>,
	/// The audio clip's span, song beats.
	pub span: (f64, f64),
	command: Command,
}

pub struct ConvertToMidi {
	pub clip: ClipId,
	pub clip_name: String,
	/// The options (`clip_id` fixed); edited by the dialog.
	pub args: AudioToMidiArgs,
	/// Sensitivity the current analysis was segmented with.
	analyzed_sensitivity: f64,
	pub phase: ConvertPhase,
	rx: Option<Receiver<Result<ClipNotes, String>>>,
}

impl ConvertToMidi {
	/// Resets the per-option overrides so `mode`'s defaults apply.
	pub fn set_mode(&mut self, mode: ConvertMode) {
		self.args = AudioToMidiArgs {
			clip_id: self.clip,
			mode,
			algorithm: self.args.algorithm,
			track_id: self.args.track_id,
			sensitivity: self.args.sensitivity,
			..AudioToMidiArgs::default()
		};
	}

	pub fn sensitivity(&self) -> f64 {
		self.args.sensitivity.unwrap_or(1.0)
	}
}

/// Instrument tracks a conversion can write to.
pub fn target_tracks(project: &cadenza_model::Project) -> Vec<(TrackId, String)> {
	project
		.tracks
		.iter()
		.filter(|t| t.kind == TrackKind::Instrument)
		.map(|t| (t.id, t.name.clone()))
		.collect()
}

impl AppState {
	pub fn convert_to_midi(&self) -> Option<&ConvertToMidi> {
		self.to_midi.as_ref()
	}

	pub fn convert_to_midi_mut(&mut self) -> Option<&mut ConvertToMidi> {
		self.to_midi.as_mut()
	}

	/// Opens the dialog for an audio clip and starts its analysis.
	pub fn start_convert_to_midi(&mut self, clip: ClipId) {
		let Some((_, c)) = self.project().find_clip(clip) else {
			return;
		};
		if c.is_midi() {
			return;
		}
		let clip_name = c.name.clone();
		self.to_midi = Some(ConvertToMidi {
			clip,
			clip_name,
			args: AudioToMidiArgs {
				clip_id: clip,
				..AudioToMidiArgs::default()
			},
			analyzed_sensitivity: 1.0,
			phase: ConvertPhase::Analyzing,
			rx: None,
		});
		self.analyze_for_midi(1.0);
	}

	/// (Re)analyzes with `sensitivity` on a worker; the file analysis itself is cached, so a
	/// new sensitivity only segments again.
	fn analyze_for_midi(&mut self, sensitivity: f64) {
		let project = self.project().clone();
		let waker = self.waker.clone();
		let cache = self
			.analysis_cache
			.clone()
			.unwrap_or_else(default_cache_dir);
		let Some(c) = &mut self.to_midi else {
			return;
		};
		let clip = c.clip;
		// The polyphonic notes do not depend on the sensitivity: kept from the last run.
		let (poly, drums) = match &c.phase {
			ConvertPhase::Ready(notes) => (notes.poly.clone(), notes.drums.clone()),
			_ => (None, None),
		};
		let (tx, rx) = mpsc::channel();
		let spawned = std::thread::Builder::new()
			.name("cadenza-to-midi".into())
			.spawn(move || {
				let made = analyze_clip(&project, clip, sensitivity, &cache).and_then(|mut n| {
					n.poly = poly;
					n.drums = drums;
					detect_for(&project, &mut n, Algorithm::Auto)?;
					Ok(n)
				});
				let _ = tx.send(made.map_err(|e| e.to_string()));
				if let Some(w) = &waker {
					w();
				}
			});
		c.analyzed_sensitivity = sensitivity;
		match spawned {
			Ok(_) => {
				c.phase = ConvertPhase::Analyzing;
				c.rx = Some(rx);
			}
			Err(e) => {
				c.phase = ConvertPhase::Failed(trf("Cannot start the analysis: {e}", &[("e", &e)]))
			}
		}
	}

	/// The conversion with the dialog's options on the current document (`None` while
	/// analyzing), or why it cannot be made.
	pub fn convert_preview(&self) -> Option<Result<ConvertPreview, String>> {
		let c = self.to_midi.as_ref()?;
		let ConvertPhase::Ready(notes) = &c.phase else {
			return None;
		};
		let project = self.project();
		let made = midi_options(&c.args).and_then(|opts| {
			let conversion = convert(
				project,
				notes,
				&opts,
				c.args.algorithm,
				c.args.drum_note.map(|p| p.0),
			);
			let (command, summary) = conversion_command(
				project,
				notes,
				&conversion,
				c.args.track_id,
				c.args.algorithm,
			)?;
			Ok((conversion, command, summary))
		});
		Some(match made {
			Ok((conversion, command, summary)) => {
				let span = project.find_clip(c.clip).map_or((0.0, 0.0), |(_, clip)| {
					(clip.start.beats(), (clip.start + clip.length).beats())
				});
				Ok(ConvertPreview {
					summary,
					notes: conversion
						.notes
						.iter()
						.map(|n| (n.start, n.length, n.pitch))
						.collect(),
					span,
					command,
				})
			}
			Err(e) => Err(e.to_string()),
		})
	}

	/// Writes the preview as ONE undo step and opens the new clip in the piano roll.
	pub fn apply_convert_to_midi(&mut self) -> bool {
		let Some(Ok(preview)) = self.convert_preview() else {
			return false;
		};
		if !self.apply(preview.command) {
			return false;
		}
		self.to_midi = None;
		self.selection
			.click_clip(preview.summary.track_id, preview.summary.clip_id, false);
		self.open_piano_roll(preview.summary.clip_id);
		self.notices.info(trn(
			"Converted to MIDI: {n} note",
			"Converted to MIDI: {n} notes",
			preview.summary.note_count,
		));
		true
	}

	pub fn cancel_convert_to_midi(&mut self) {
		self.to_midi = None;
	}

	/// Collects the worker's result and re-segments when the sensitivity changed (each frame).
	pub(super) fn poll_convert_to_midi(&mut self) {
		let Some(c) = &mut self.to_midi else {
			return;
		};
		if let Some(rx) = &c.rx {
			match rx.try_recv() {
				Ok(r) => {
					c.phase = match r {
						Ok(notes) => ConvertPhase::Ready(Box::new(notes)),
						Err(e) => ConvertPhase::Failed(e),
					};
					c.rx = None;
				}
				Err(mpsc::TryRecvError::Empty) => return,
				Err(mpsc::TryRecvError::Disconnected) => {
					c.phase = ConvertPhase::Failed(tr("the analysis thread stopped").into());
					c.rx = None;
				}
			}
		}
		let wanted = c.sensitivity();
		if (wanted - c.analyzed_sensitivity).abs() > 1e-9 && c.rx.is_none() {
			self.analyze_for_midi(wanted);
		}
	}
}

/// Splits the hum-to-MIDI tracks out of a finished recording: returns the recording
/// without their audio, the commands writing each one's notes (a MIDI clip from its last
/// pass, hum settings) and warnings. The recorded files stay in `Audio/`.
pub(super) fn hum_commands(
	project: &Project,
	rec: &Recording,
	hum: &BTreeSet<TrackId>,
	cache: &Path,
) -> (Recording, Vec<Command>, Vec<String>) {
	let is_hum = |id: TrackId| {
		hum.contains(&id)
			&& project
				.track(id)
				.is_some_and(|t| t.kind == TrackKind::Instrument)
	};
	let (hummed, rest): (Vec<RecordedAudio>, Vec<RecordedAudio>) =
		rec.audio.iter().cloned().partition(|a| is_hum(a.track));
	let mut remaining = rec.clone();
	remaining.audio = rest;
	let (mut commands, mut warnings) = (Vec::new(), Vec::new());
	for audio in hummed {
		let name = project
			.track(audio.track)
			.map_or_else(String::new, |t| t.name.clone());
		// The pass as an audio clip on a scratch audio track, converted from there.
		let mut doc = Document::new(project.clone());
		let scratch = Track::new(name.clone(), TrackKind::Audio);
		let scratch_id = scratch.id;
		if doc
			.execute(Command::AddTrack {
				track: scratch,
				index: None,
			})
			.is_err()
		{
			continue;
		}
		let one = Recording {
			audio: vec![RecordedAudio {
				track: scratch_id,
				..audio.clone()
			}],
			midi_tracks: Vec::new(),
			midi: Vec::new(),
			..rec.clone()
		};
		let Some(cmd) = record_commands(doc.project(), &one, &RecordOptions::default()) else {
			continue;
		};
		if doc.execute(cmd).is_err() {
			continue;
		}
		let Some(clip) = doc
			.project()
			.track(scratch_id)
			.and_then(|t| t.clips.iter().filter(|c| c.plays()).max_by_key(|c| c.start))
			.map(|c| c.id)
		else {
			continue;
		};
		let made = analyze_clip(doc.project(), clip, 1.0, cache).and_then(|notes| {
			let conversion = convert(
				doc.project(),
				&notes,
				&MidiOptions::hum(),
				Algorithm::Mono,
				None,
			);
			conversion_command(
				doc.project(),
				&notes,
				&conversion,
				Some(audio.track),
				Algorithm::Mono,
			)
		});
		match made {
			Ok((
				Command::Batch {
					commands: inner, ..
				},
				_,
			)) => commands.extend(inner),
			Ok((cmd, _)) => commands.push(cmd),
			Err(e) => warnings.push(trf(
				"Hum to MIDI on '{track}': {e}",
				&[("track", &name), ("e", &e)],
			)),
		}
	}
	(remaining, commands, warnings)
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_notes::eval::synth::{Phrase, SynthNote, Vowel, render};

	fn wait_ready(s: &mut AppState) {
		let start = std::time::Instant::now();
		while matches!(
			s.convert_to_midi().map(|c| &c.phase),
			Some(ConvertPhase::Analyzing)
		) {
			assert!(start.elapsed().as_secs() < 30, "analysis timed out");
			std::thread::sleep(std::time::Duration::from_millis(5));
			s.poll_convert_to_midi();
		}
	}

	#[test]
	fn converts_an_audio_clip_in_one_step_and_opens_it() {
		let dir = tempfile::tempdir().unwrap();
		let wav = dir.path().join("hum.wav");
		let r = render(&Phrase {
			notes: vec![
				SynthNote::new(57.0, 0.5),
				SynthNote::new(60.0, 0.5),
				SynthNote::new(64.0, 0.75),
			],
			vowel: Vowel::U,
			sample_rate: 22_050,
			snr_db: None,
			reverb: None,
			seed: 1,
		});
		let buf = cadenza_audio_io::AudioBuffer::from_planar(22_050, vec![r.samples]).unwrap();
		cadenza_audio_io::export(
			&buf,
			&wav,
			&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
		)
		.unwrap();
		let mut s = AppState::headless();
		s.analysis_cache = Some(dir.path().join("cache"));
		s.import_audio(&wav);
		let audio = s.project().tracks.last().unwrap();
		let (audio_track, clip) = (audio.id, audio.clips[0].id);

		s.start_convert_to_midi(clip);
		wait_ready(&mut s);
		let p = s.convert_preview().unwrap().unwrap();
		assert_eq!(
			p.notes.iter().map(|n| n.2).collect::<Vec<_>>(),
			[57, 60, 64]
		);
		assert!(p.summary.key.is_none());
		// Options change the preview without touching the document.
		s.convert_to_midi_mut().unwrap().set_mode(ConvertMode::Hum);
		let p = s.convert_preview().unwrap().unwrap();
		assert!(p.summary.key.is_some());
		assert_eq!(s.document().history_labels().len(), 1);
		// A new sensitivity segments again.
		s.convert_to_midi_mut().unwrap().args.sensitivity = Some(1.5);
		s.poll_convert_to_midi();
		wait_ready(&mut s);
		assert!(s.convert_preview().unwrap().is_ok());

		assert!(s.apply_convert_to_midi());
		assert!(s.convert_to_midi().is_none());
		let project = s.project();
		let at = project
			.tracks
			.iter()
			.position(|t| t.id == audio_track)
			.unwrap();
		let midi = &project.tracks[at + 1];
		assert_eq!(midi.kind, TrackKind::Instrument);
		assert_eq!(midi.clips[0].notes().len(), 3);
		assert!(s.selection.clips.contains(&midi.clips[0].id));
		assert_eq!(s.document().history_labels().len(), 2, "one undo step");

		// MIDI clips have nothing to convert.
		let midi_clip = s.project().tracks[at + 1].clips[0].id;
		s.start_convert_to_midi(midi_clip);
		assert!(s.convert_to_midi().is_none());
	}
}
