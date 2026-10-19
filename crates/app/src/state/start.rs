//! The start screen's data: what each recent project holds (tempo, signature, tracks,
//! a thumbnail of its clips, when it was saved), read on a worker so the page never waits
//! on the disk, plus the page's own inputs (the producer prompt).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cadenza_core::Tick;
use cadenza_model::{Project, TrackKind};

use crate::jobs::Job;

/// Rows a thumbnail shows at most (tracks past it are folded into the last row).
pub const THUMB_ROWS: usize = 8;

/// One clip of a thumbnail: its row and span as fractions of the song (0..1).
#[derive(Debug, Clone, PartialEq)]
pub struct ThumbClip {
	pub row: usize,
	pub start: f32,
	pub end: f32,
	/// The track color (`#RRGGBB`).
	pub color: Option<String>,
}

/// What the start screen shows of a recent project.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentInfo {
	pub name: String,
	pub bpm: f64,
	pub signature: String,
	pub style: Option<String>,
	pub tracks: usize,
	pub modified: Option<SystemTime>,
	pub rows: usize,
	pub clips: Vec<ThumbClip>,
}

/// Reads `path` (a `.cadenza.json`) into its start screen summary.
pub fn recent_info(path: &Path) -> Result<RecentInfo, String> {
	let project = Project::load_json(path).map_err(|e| e.to_string())?;
	let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
	Ok(summarize(&project, modified))
}

/// The summary of `project` (pure: the thumbnail spans the clips that play).
pub fn summarize(project: &Project, modified: Option<SystemTime>) -> RecentInfo {
	let tracks: Vec<_> = project
		.tracks
		.iter()
		.filter(|t| !matches!(t.kind, TrackKind::Folder | TrackKind::Bus))
		.filter(|t| !t.clips.is_empty())
		.collect();
	let end = tracks
		.iter()
		.flat_map(|t| t.clips.iter().map(|c| c.end()))
		.max()
		.unwrap_or(Tick::ZERO)
		.0
		.max(1) as f32;
	let clips = tracks
		.iter()
		.enumerate()
		.flat_map(|(i, t)| {
			t.clips
				.iter()
				.filter(|c| c.plays())
				.map(move |c| ThumbClip {
					row: i.min(THUMB_ROWS - 1),
					start: c.start.0 as f32 / end,
					end: c.end().0 as f32 / end,
					color: t.color.clone(),
				})
		})
		.collect();
	let ts = project.time_signature;
	RecentInfo {
		name: project.name.clone(),
		bpm: project.tempo.bpm_at(Tick::ZERO),
		signature: format!("{}/{}", ts.numerator, ts.denominator),
		style: project.meta.style.clone(),
		tracks: project
			.tracks
			.iter()
			.filter(|t| t.kind != TrackKind::Folder)
			.count(),
		modified,
		rows: tracks.len().min(THUMB_ROWS),
		clips,
	}
}

/// "12 min ago", "Yesterday", "3 days ago", "2 weeks ago" (no time zone needed).
pub fn when(modified: SystemTime, now: SystemTime) -> String {
	let day = 86_400;
	let secs = now.duration_since(modified).map_or(0, |d| d.as_secs());
	match secs {
		0..60 => "Just now".into(),
		60..3600 => format!("{} min ago", secs / 60),
		3600..86_400 => format!("{} h ago", secs / 3600),
		86_400..172_800 => "Yesterday".into(),
		s if s < 7 * day => format!("{} days ago", s / day),
		s if s < 14 * day => "1 week ago".into(),
		s if s < 60 * day => format!("{} weeks ago", s / (7 * day)),
		s => format!("{} months ago", s / (30 * day)),
	}
}

/// Summaries read by one worker run.
type Loaded = Vec<(PathBuf, Result<RecentInfo, String>)>;

/// Recent project summaries: loaded once per path on a worker, kept for the session.
#[derive(Default)]
pub struct StartScreen {
	infos: HashMap<PathBuf, Result<RecentInfo, String>>,
	job: Option<Job<Loaded>>,
	/// The "Describe it to the producer" text.
	pub prompt: String,
	/// The style card lists every style, not just the first few.
	pub all_styles: bool,
}

impl StartScreen {
	/// The summary of `path`, once loaded.
	pub fn info(&self, path: &Path) -> Option<&Result<RecentInfo, String>> {
		self.infos.get(path)
	}

	/// Starts loading the paths of `recent` not known yet (one worker at a time) and takes
	/// in finished results; true while something is still loading.
	pub fn update(&mut self, recent: &[PathBuf]) -> bool {
		if let Some(job) = &self.job {
			match job.poll() {
				Some(Some(results)) => {
					self.infos.extend(results);
					self.job = None;
				}
				Some(None) => self.job = None,
				None => return true,
			}
		}
		let missing: Vec<PathBuf> = recent
			.iter()
			.filter(|p| !self.infos.contains_key(*p))
			.cloned()
			.collect();
		if missing.is_empty() {
			return false;
		}
		self.job = Some(Job::spawn("recent-projects", None, move || {
			missing
				.into_iter()
				.map(|p| {
					let info = recent_info(&p);
					(p, info)
				})
				.collect()
		}));
		true
	}

	/// Forgets `path` so it is read again (after a save).
	pub fn invalidate(&mut self, path: &Path) {
		self.infos.remove(path);
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use cadenza_core::PPQ;
	use cadenza_model::{Clip, Track};

	use super::*;

	#[test]
	fn summaries_span_the_song_and_skip_buses() {
		let mut p = Project::new("Sketch");
		let mut keys = Track::new("Keys", TrackKind::Instrument).with_color("#E3B24F");
		keys.clips.push(Clip::midi("A", Tick::ZERO, Tick(PPQ * 4)));
		keys.clips
			.push(Clip::midi("B", Tick(PPQ * 4), Tick(PPQ * 4)));
		p.tracks.push(keys);
		p.tracks.push(Track::new("Reverb", TrackKind::Bus));
		let info = summarize(&p, None);
		assert_eq!(info.tracks, 2);
		assert_eq!(info.rows, 1);
		assert_eq!(info.signature, "4/4");
		assert_eq!(info.clips.len(), 2);
		assert_eq!(info.clips[1].start, 0.5);
		assert_eq!(info.clips[1].end, 1.0);
		assert_eq!(info.clips[0].color.as_deref(), Some("#E3B24F"));
	}

	#[test]
	fn dates_read_like_a_file_manager() {
		let day = 86_400;
		let now = SystemTime::UNIX_EPOCH + Duration::from_secs(day * 400 + 15 * 3600);
		let at = |secs: u64| now - Duration::from_secs(secs);
		assert_eq!(when(at(30), now), "Just now");
		assert_eq!(when(at(2 * 3600), now), "2 h ago");
		assert_eq!(when(at(day), now), "Yesterday");
		assert_eq!(when(at(3 * day), now), "3 days ago");
		assert_eq!(when(at(14 * day), now), "2 weeks ago");
		assert_eq!(when(at(90 * day), now), "3 months ago");
	}

	#[test]
	fn recent_projects_load_on_a_worker() {
		let tmp = tempfile::tempdir().unwrap();
		let file = tmp.path().join("Sketch").join("Sketch.cadenza.json");
		Project::new("Sketch").save_json(&file).unwrap();
		let gone = tmp.path().join("gone.cadenza.json");
		let recent = vec![file.clone(), gone.clone()];
		let mut s = StartScreen::default();
		for _ in 0..500 {
			if !s.update(&recent) {
				break;
			}
			std::thread::sleep(Duration::from_millis(5));
		}
		assert_eq!(s.info(&file).unwrap().as_ref().unwrap().name, "Sketch");
		assert!(s.info(&gone).unwrap().is_err());
	}
}
