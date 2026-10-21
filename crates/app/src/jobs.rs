//! Work that must never run on the UI thread: decoding audio for waveforms, plugin
//! scans, renders and native file dialogs. Each runs on its own thread and is polled
//! once per frame.

use crate::i18n::tr;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use cadenza_audio_io::WaveformPeaks;

use crate::audio::Waker;

/// A one-shot background computation.
pub struct Job<T> {
	rx: Receiver<T>,
	pub label: String,
	pub started: Instant,
}

impl<T: Send + 'static> Job<T> {
	pub fn spawn(
		label: impl Into<String>,
		waker: Option<Waker>,
		work: impl FnOnce() -> T + Send + 'static,
	) -> Self {
		let (tx, rx) = mpsc::channel();
		let label = label.into();
		// If the thread cannot start, `tx` is dropped and `poll` reports the job as lost.
		let _ = thread::Builder::new()
			.name(format!("cadenza-job-{label}"))
			.spawn(move || {
				let _ = tx.send(work());
				if let Some(w) = waker {
					w();
				}
			});
		Self {
			rx,
			label,
			started: Instant::now(),
		}
	}

	/// The result once finished; `Some(None)` if the worker died without one.
	pub fn poll(&self) -> Option<Option<T>> {
		match self.rx.try_recv() {
			Ok(v) => Some(Some(v)),
			Err(mpsc::TryRecvError::Empty) => None,
			Err(mpsc::TryRecvError::Disconnected) => Some(None),
		}
	}
}

/// Waveform state of one audio file.
#[derive(Clone)]
pub enum Peaks {
	Pending,
	Ready(Arc<WaveformPeaks>),
	Failed(String),
}

/// Decoded samples of one file, for drawing finer than the finest peak level (the
/// audio editor zoomed in).
#[derive(Clone)]
pub enum Samples {
	Pending,
	Ready(Arc<cadenza_audio_io::AudioBuffer>),
	Failed,
}

type PeakResult = (
	PathBuf,
	Result<(WaveformPeaks, Option<Arc<cadenza_audio_io::AudioBuffer>>), String>,
);

/// Decodes audio files on a worker thread and caches their waveform peaks by path;
/// keeps the samples of the one file the audio editor shows.
pub struct PeakCache {
	entries: HashMap<PathBuf, Peaks>,
	samples: Option<(PathBuf, Samples)>,
	requests: Sender<(PathBuf, bool)>,
	results: Receiver<PeakResult>,
}

impl PeakCache {
	pub fn new(waker: Option<Waker>) -> Self {
		let (requests, rx) = mpsc::channel::<(PathBuf, bool)>();
		let (tx, results) = mpsc::channel::<PeakResult>();
		let _ = thread::Builder::new()
			.name("cadenza-app-peaks".into())
			.spawn(move || {
				for (path, keep) in rx {
					let peaks = cadenza_audio_io::decode_file(&path)
						.map(|buf| {
							let peaks = WaveformPeaks::compute_default(&buf);
							(peaks, keep.then(|| Arc::new(buf)))
						})
						.map_err(|e| e.to_string());
					if tx.send((path, peaks)).is_err() {
						break;
					}
					if let Some(w) = &waker {
						w();
					}
				}
			});
		Self {
			entries: HashMap::new(),
			samples: None,
			requests,
			results,
		}
	}

	/// The peaks of `path`, queueing the computation on first use.
	pub fn get(&mut self, path: &Path) -> Peaks {
		if let Some(p) = self.entries.get(path) {
			return p.clone();
		}
		self.entries.insert(path.to_path_buf(), Peaks::Pending);
		let _ = self.requests.send((path.to_path_buf(), false));
		Peaks::Pending
	}

	/// The decoded samples of `path` (one file at a time), decoding on first use.
	pub fn samples(&mut self, path: &Path) -> Samples {
		match &self.samples {
			Some((p, s)) if p == path => s.clone(),
			_ => {
				self.samples = Some((path.to_path_buf(), Samples::Pending));
				let _ = self.requests.send((path.to_path_buf(), true));
				Samples::Pending
			}
		}
	}

	/// Collects finished computations. Returns true when something new arrived.
	pub fn poll(&mut self) -> bool {
		let mut any = false;
		while let Ok((path, result)) = self.results.try_recv() {
			if let Some((p, s)) = &mut self.samples
				&& *p == path
				&& matches!(s, Samples::Pending)
			{
				match &result {
					Ok((_, Some(buf))) => *s = Samples::Ready(buf.clone()),
					Err(_) => *s = Samples::Failed,
					Ok((_, None)) => {}
				}
			}
			let entry = match result {
				Ok((p, _)) => Peaks::Ready(Arc::new(p)),
				Err(e) => Peaks::Failed(e),
			};
			self.entries.insert(path, entry);
			any = true;
		}
		any
	}
}

/// What a native file dialog is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
	Open,
	SaveAs,
	ImportAudio,
	ImportMidi,
	/// The export dialog's destination folder.
	ExportFolder,
	/// Where to extract a project archive being opened.
	UnpackFolder,
	/// Folder searched for missing files.
	RelinkFolder,
	/// Project archive to write.
	PackArchive,
	/// A reference track for the mix assistant.
	MixReference,
	/// A reference for an EQ's Match EQ.
	MatchReference,
	/// A drum sampler pad's sample (`InstrumentView::dialog`).
	PadSample,
	/// One more layer of a drum sampler pad.
	PadLayer,
	/// A folder of drum samples mapped onto the pads by name.
	KitFolder,
}

impl FileAction {
	fn title(self) -> &'static str {
		match self {
			Self::Open => tr("Open project"),
			Self::SaveAs => tr("Save project as"),
			Self::ImportAudio => tr("Import audio file"),
			Self::ImportMidi => tr("Import MIDI file"),
			Self::ExportFolder => tr("Export into folder"),
			Self::UnpackFolder => tr("Extract the project archive into"),
			Self::RelinkFolder => tr("Search for missing files in"),
			Self::PackArchive => tr("Export project archive"),
			Self::MixReference => tr("Choose a reference track"),
			Self::MatchReference => tr("Match the EQ to an audio file"),
			Self::PadSample => tr("Load a sample into the pad"),
			Self::PadLayer => tr("Add a layer to the pad"),
			Self::KitFolder => tr("Map a folder of drum samples onto the pads"),
		}
	}
}

/// Shows a native (xdg-desktop-portal on Linux) file dialog off the UI thread.
pub fn file_dialog(
	action: FileAction,
	start_dir: Option<PathBuf>,
	suggested_name: String,
	waker: Option<Waker>,
) -> Job<Option<PathBuf>> {
	Job::spawn("file-dialog", waker, move || {
		let mut dialog = rfd::FileDialog::new().set_title(action.title());
		if let Some(dir) = start_dir {
			dialog = dialog.set_directory(dir);
		}
		match action {
			FileAction::Open => dialog
				.add_filter(
					tr("Cadenza project or archive"),
					&["json", "zip", "cadenzapack"],
				)
				.pick_file(),
			FileAction::SaveAs => dialog
				.add_filter("Cadenza project", &["json"])
				.set_file_name(suggested_name)
				.save_file(),
			FileAction::ImportAudio
			| FileAction::MixReference
			| FileAction::MatchReference
			| FileAction::PadSample
			| FileAction::PadLayer => dialog
				.add_filter("Audio", crate::state::AUDIO_EXTENSIONS)
				.pick_file(),
			FileAction::ImportMidi => dialog.add_filter("MIDI", &["mid", "midi"]).pick_file(),
			FileAction::ExportFolder
			| FileAction::UnpackFolder
			| FileAction::RelinkFolder
			| FileAction::KitFolder => dialog.pick_folder(),
			FileAction::PackArchive => dialog
				.add_filter("Cadenza project archive", &["zip", "cadenzapack"])
				.set_file_name(suggested_name)
				.save_file(),
		}
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	#[test]
	fn jobs_report_their_result() {
		let job = Job::spawn("t", None, || 42);
		let deadline = Instant::now() + Duration::from_secs(5);
		loop {
			if let Some(r) = job.poll() {
				assert_eq!(r, Some(42));
				break;
			}
			assert!(Instant::now() < deadline);
			thread::sleep(Duration::from_millis(5));
		}
	}

	#[test]
	fn peaks_are_computed_off_thread_and_cached() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("tone.wav");
		let spec = cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32);
		let samples: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
		let buf =
			cadenza_audio_io::AudioBuffer::from_planar(48_000, vec![samples.clone(), samples])
				.unwrap();
		cadenza_audio_io::export(&buf, &path, &spec).unwrap();

		let mut cache = PeakCache::new(None);
		assert!(matches!(cache.get(&path), Peaks::Pending));
		let deadline = Instant::now() + Duration::from_secs(10);
		while !cache.poll() {
			assert!(Instant::now() < deadline, "peaks never arrived");
			thread::sleep(Duration::from_millis(5));
		}
		match cache.get(&path) {
			Peaks::Ready(p) => {
				assert_eq!(p.frames, 4800);
				let top = p.levels[0].range(0, 0, 4800);
				assert!((top.max - 0.5).abs() < 0.01);
			}
			_ => panic!("expected peaks"),
		}
		assert!(matches!(
			cache.get(&dir.path().join("missing.wav")),
			Peaks::Pending
		));
		while !cache.poll() {
			thread::sleep(Duration::from_millis(5));
		}
		assert!(matches!(
			cache.get(&dir.path().join("missing.wav")),
			Peaks::Failed(_)
		));
	}
}
