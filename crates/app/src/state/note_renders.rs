//! Background renders of audio clips with note edits (NE-5): the player plays an edited
//! clip's render when it exists, so after every change the missing renders are made one
//! at a time on a worker and the project is reloaded when each lands.

use std::collections::HashSet;
use std::path::PathBuf;

use cadenza_engine::note_render::{self, PendingRender};

use crate::audio::Waker;
use crate::jobs::Job;

#[derive(Default)]
pub struct NoteRenders {
	job: Option<(PendingRender, Job<Result<PathBuf, String>>)>,
	/// The document changed since the pending renders were listed.
	stale: bool,
	/// Renders that failed (file + edits as JSON), not retried until they change.
	failed: HashSet<(PathBuf, String)>,
}

/// What finishing a job asks of the app.
pub enum Finished {
	Rendered,
	Failed(String),
}

impl NoteRenders {
	pub fn new() -> Self {
		Self {
			stale: true,
			..Self::default()
		}
	}

	/// The document changed: look for missing renders again.
	pub fn invalidate(&mut self) {
		self.stale = true;
	}

	pub fn busy(&self) -> bool {
		self.job.is_some()
	}

	/// The clip being rendered now.
	pub fn rendering(&self) -> Option<cadenza_core::ClipId> {
		self.job.as_ref().map(|(p, _)| p.clip)
	}

	/// Polls the running render and starts the next missing one.
	pub fn tick(
		&mut self,
		project: &cadenza_model::Project,
		waker: Option<Waker>,
	) -> Option<Finished> {
		let mut finished = None;
		if let Some(r) = self.job.as_ref().and_then(|(_, j)| j.poll()) {
			let (pending, _) = self.job.take().expect("a running render");
			finished = Some(match r {
				Some(Ok(_)) => Finished::Rendered,
				Some(Err(e)) => {
					self.failed.insert(key(&pending));
					Finished::Failed(e)
				}
				None => {
					self.failed.insert(key(&pending));
					Finished::Failed("the render stopped unexpectedly".into())
				}
			});
			self.stale = true;
		}
		if self.job.is_none() && self.stale {
			self.stale = false;
			if let Some(next) = note_render::pending(project)
				.into_iter()
				.find(|p| !self.failed.contains(&key(p)))
			{
				let (file, edits) = (next.file.clone(), next.edits.clone());
				let job = Job::spawn("note-render", waker, move || {
					note_render::render(&file, &edits)
						.map(|r| r.path)
						.map_err(|e| e.to_string())
				});
				self.job = Some((next, job));
			}
		}
		finished
	}
}

fn key(p: &PendingRender) -> (PathBuf, String) {
	(
		p.file.clone(),
		serde_json::to_string(&p.edits).unwrap_or_default(),
	)
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use cadenza_model::{ClipContent, Command};
	use cadenza_notes::eval::synth::{Phrase, SynthNote, Vowel, render};

	use crate::state::AppState;

	fn render_dir() -> std::path::PathBuf {
		static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
		DIR.get_or_init(|| {
			let dir = std::env::temp_dir()
				.join(format!("cadenza-app-note-renders-{}", std::process::id()));
			// SAFETY: set once (OnceLock) before any render is looked up.
			unsafe { std::env::set_var(cadenza_engine::note_render::NOTE_RENDERS_ENV, &dir) };
			dir
		})
		.clone()
	}

	#[test]
	fn edited_clips_render_in_the_background_and_reload() {
		let renders = render_dir();
		let dir = tempfile::tempdir().unwrap();
		let wav = dir.path().join("vox.wav");
		let r = render(&Phrase {
			notes: vec![SynthNote::new(57.0, 0.5), SynthNote::new(60.0, 0.5)],
			vowel: Vowel::A,
			sample_rate: 22_050,
			snr_db: None,
			reverb: None,
			seed: 3,
		});
		let buf =
			cadenza_audio_io::AudioBuffer::from_planar(22_050, vec![r.samples.clone()]).unwrap();
		cadenza_audio_io::export(
			&buf,
			&wav,
			&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
		)
		.unwrap();
		let mut s = AppState::headless();
		s.import_audio(&wav);
		let clip = s.project().tracks.last().unwrap().clips[0].id;
		let analysis =
			cadenza_notes::analysis::analyze(&r.samples, 22_050, &Default::default(), &mut |_| {
				true
			})
			.unwrap();
		let mut notes =
			cadenza_notes::clip::detect(&analysis, 1.0, None, &mut cadenza_core::AudioNoteId::new);
		assert_eq!(notes.notes.len(), 2);
		notes.notes[1].pitch = Some(62.0);
		assert!(s.apply(Command::SetAudioNotes {
			clip,
			notes: Some(Box::new(notes)),
		}));
		s.tick(Instant::now());
		assert!(s.note_renders.busy());
		assert_eq!(s.note_renders.rendering(), Some(clip));
		let started = Instant::now();
		while s.note_renders.busy() {
			assert!(
				started.elapsed() < Duration::from_secs(60),
				"render never finished"
			);
			std::thread::sleep(Duration::from_millis(20));
			s.tick(Instant::now());
		}
		assert!(s.reload_pending());
		assert!(cadenza_engine::note_render::pending(s.project()).is_empty());
		let (_, c) = s.project().find_clip(clip).unwrap();
		let ClipContent::Audio {
			file,
			notes: Some(e),
			..
		} = &c.content
		else {
			panic!("notes");
		};
		let path = cadenza_engine::note_render::ready(file, e).unwrap();
		assert!(path.starts_with(&renders));
		// Undo: nothing to render, the clip plays its file.
		s.undo();
		s.tick(Instant::now());
		assert!(!s.note_renders.busy());
	}
}
