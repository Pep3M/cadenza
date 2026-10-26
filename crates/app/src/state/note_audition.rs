//! Hearing notes while they are dragged in the note editor (NE-9d). The take, its analysis
//! and pitch marks are loaded once per file on a worker; each request renders the stretch
//! of the dragged notes at their provisional pitch (`resynth::audition`: open loop,
//! milliseconds) and the newest result plays once over the output as a snippet. Requests
//! made while one renders are coalesced into the newest.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadenza_model::NoteEdits;
use cadenza_notes::analysis::{Analysis, AnalysisOptions, Cache};
use cadenza_notes::marks::{Marks, pitch_marks};
use cadenza_notes::{clip, resynth};

use crate::audio::Waker;
use crate::jobs::Job;

/// Longest stretch auditioned, seconds.
pub const MAX_SECONDS: f64 = 4.0;
/// Heard before the first note and after the last one, seconds.
pub const LEAD: f64 = 0.02;
pub const TAIL: f64 = 0.06;

/// A take ready to audition.
pub struct Source {
	pub file: PathBuf,
	pub channels: Vec<Vec<f32>>,
	pub analysis: Analysis,
	pub marks: Marks,
}

/// What to hear: the clip's notes with the provisional edits, file seconds `from..to`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
	pub file: PathBuf,
	pub edits: NoteEdits,
	pub from: f64,
	pub to: f64,
	pub gain_db: f32,
}

/// A rendered audition.
#[derive(Debug, Clone)]
pub struct Rendered {
	pub channels: Vec<Vec<f32>>,
	pub rate: u32,
	pub gain_db: f32,
}

#[derive(Default)]
pub struct NoteAudition {
	source: Option<Arc<Source>>,
	loading: Option<(PathBuf, Job<Result<Source, String>>)>,
	job: Option<Job<Result<Rendered, String>>>,
	pending: Option<Request>,
	/// A file that could not be loaded (not retried in this session).
	failed: Option<PathBuf>,
}

impl NoteAudition {
	/// Asks to hear `request` (replacing one not started yet).
	pub fn request(&mut self, request: Request) {
		self.pending = Some(request);
	}

	/// Forgets a request not started yet.
	pub fn cancel(&mut self) {
		self.pending = None;
	}

	/// The request waiting to start.
	pub fn pending(&self) -> Option<&Request> {
		self.pending.as_ref()
	}

	pub fn busy(&self) -> bool {
		self.loading.is_some() || self.job.is_some()
	}

	/// Lands finished work, starts the next and returns an audition to play.
	pub fn tick(&mut self, cache: &Path, waker: Option<Waker>) -> Option<Rendered> {
		let mut ready = None;
		if let Some(r) = self.job.as_ref().and_then(|j| j.poll()) {
			self.job = None;
			if let Some(Ok(r)) = r {
				ready = Some(r);
			}
		}
		if let Some(r) = self.loading.as_ref().and_then(|(_, j)| j.poll()) {
			let (file, _) = self.loading.take().expect("a loading take");
			match r {
				Some(Ok(source)) => self.source = Some(Arc::new(source)),
				_ => {
					self.failed = Some(file);
					self.pending = None;
				}
			}
		}
		if self.busy() {
			return ready;
		}
		let Some(request) = self.pending.take() else {
			return ready;
		};
		if self.failed.as_ref() == Some(&request.file) {
			return ready;
		}
		match &self.source {
			Some(source) if source.file == request.file => {
				let source = source.clone();
				self.job = Some(Job::spawn("note-audition", waker, move || {
					render(&source, &request)
				}));
			}
			_ => {
				let (file, cache) = (request.file.clone(), cache.to_path_buf());
				self.loading = Some((
					file.clone(),
					Job::spawn("note-audition-load", waker, move || load(&file, &cache)),
				));
				self.pending = Some(request);
			}
		}
		ready
	}
}

/// Decodes `file` and analyzes its mono mix through the content cache (the analysis the
/// notes were detected on), then marks its pitch periods.
pub fn load(file: &Path, cache: &Path) -> Result<Source, String> {
	let buffer = cadenza_audio_io::decode_file(file).map_err(|e| e.to_string())?;
	let channels: Vec<&[f32]> = buffer.channels().iter().map(Vec::as_slice).collect();
	let mono = resynth::mono(&channels);
	let analysis = Cache::new(cache)
		.get_or_analyze(
			&mono,
			buffer.sample_rate(),
			&AnalysisOptions::default(),
			&mut |_| true,
		)
		.ok_or("the analysis was cancelled")?;
	let marks = pitch_marks(&mono, &analysis);
	Ok(Source {
		file: file.to_path_buf(),
		channels: buffer.channels().to_vec(),
		analysis,
		marks,
	})
}

/// Renders `request` from `source`.
pub fn render(source: &Source, request: &Request) -> Result<Rendered, String> {
	let channels: Vec<&[f32]> = source.channels.iter().map(Vec::as_slice).collect();
	let channels = resynth::audition(
		&channels,
		&source.analysis,
		&clip::to_segmentation(&request.edits),
		&source.marks,
		&clip::to_edits(&request.edits),
		request.from,
		request.to,
	)
	.map_err(|e| e.to_string())?;
	Ok(Rendered {
		channels,
		rate: source.analysis.sample_rate,
		gain_db: request.gain_db,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::AudioNoteId;
	use cadenza_notes::eval::synth;

	fn take(dir: &Path) -> (PathBuf, Vec<f32>, u32) {
		let case = synth::corpus(22_050)
			.into_iter()
			.max_by_key(|c| c.truth.len())
			.unwrap();
		let path = dir.join("take.wav");
		let spec = hound::WavSpec {
			channels: 1,
			sample_rate: case.sample_rate,
			bits_per_sample: 32,
			sample_format: hound::SampleFormat::Float,
		};
		let mut w = hound::WavWriter::create(&path, spec).unwrap();
		for s in &case.samples {
			w.write_sample(*s).unwrap();
		}
		w.finalize().unwrap();
		(path, case.samples, case.sample_rate)
	}

	#[test]
	fn a_dragged_note_is_heard_at_its_new_pitch_and_requests_coalesce() {
		let dir = tempfile::tempdir().unwrap();
		let (file, samples, rate) = take(dir.path());
		let cache = dir.path().join("cache");
		let analysis = Cache::new(&cache)
			.get_or_analyze(&samples, rate, &AnalysisOptions::default(), &mut |_| true)
			.unwrap();
		let mut edits = clip::detect(&analysis, 1.0, None, &mut AudioNoteId::new);
		let k = edits.notes.len() / 2;
		let note = edits.notes[k].clone();
		edits.notes[k].pitch = Some(note.center + 2.0);
		let request = |edits: &NoteEdits| Request {
			file: file.clone(),
			edits: edits.clone(),
			from: note.start,
			to: note.end,
			gain_db: -3.0,
		};
		let mut a = NoteAudition::default();
		// A first request, then a newer one before anything ran: only the newest renders.
		let mut first = edits.clone();
		first.notes[k].pitch = Some(note.center + 7.0);
		a.request(request(&first));
		a.request(request(&edits));
		let start = std::time::Instant::now();
		let heard = loop {
			if let Some(h) = a.tick(&cache, None) {
				break h;
			}
			assert!(start.elapsed().as_secs() < 20, "no audition");
			std::thread::sleep(std::time::Duration::from_millis(2));
		};
		assert!(!a.busy());
		assert_eq!(heard.rate, rate);
		assert_eq!(heard.gain_db, -3.0);
		let got = cadenza_notes::analysis::analyze(
			&heard.channels[0],
			rate,
			&AnalysisOptions::default(),
			&mut |_| true,
		)
		.unwrap();
		let mut pitches: Vec<f64> = got.frames.iter().filter_map(|f| f.pitch).collect();
		pitches.sort_by(f64::total_cmp);
		let median = pitches[pitches.len() / 2];
		assert!((median - (note.center + 2.0)).abs() < 0.15, "{median}");
		// Nothing more queued.
		assert!(a.tick(&cache, None).is_none());
	}

	#[test]
	fn a_missing_file_is_not_retried() {
		let mut a = NoteAudition::default();
		let dir = tempfile::tempdir().unwrap();
		let r = Request {
			file: dir.path().join("gone.wav"),
			edits: NoteEdits {
				notes: Vec::new(),
				sibilants: Vec::new(),
				sibilant_gain_db: 0.0,
				sensitivity: 1.0,
				user_splits: Vec::new(),
				user_merges: Vec::new(),
			},
			from: 0.0,
			to: 1.0,
			gain_db: 0.0,
		};
		a.request(r.clone());
		let start = std::time::Instant::now();
		while a.busy() || a.pending.is_some() {
			assert!(a.tick(dir.path(), None).is_none());
			assert!(start.elapsed().as_secs() < 10);
			std::thread::sleep(std::time::Duration::from_millis(2));
		}
		a.request(r);
		assert!(a.tick(dir.path(), None).is_none());
		assert!(!a.busy());
	}
}
