//! The export dialog (File → Export, Ctrl+E): settings (persisted per user), the engine
//! job they describe, and the running export (progress, cancel, result).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cadenza_audio_io::{ExportOptions, FlacBitDepth, Metadata, WavSampleFormat};
use cadenza_core::{Tick, TrackId};
use cadenza_engine::{ExportProgress, ExportReport, ExportSpec, Normalize, StemSet};
use cadenza_model::{Project, ProjectLayout};
use serde::{Deserialize, Serialize};

use crate::jobs::Job;

/// What part of the song is exported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeKind {
	#[default]
	Song,
	/// The arrange loop region.
	Loop,
	/// The span of the selected clips.
	Selection,
	/// `from_bar..=to_bar` (1-based).
	Bars,
}

impl RangeKind {
	pub const ALL: [RangeKind; 4] = [Self::Song, Self::Loop, Self::Selection, Self::Bars];

	pub fn label(self) -> &'static str {
		match self {
			Self::Song => "Whole song",
			Self::Loop => "Loop region",
			Self::Selection => "Selected clips",
			Self::Bars => "Bars",
		}
	}
}

/// Which stems are written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StemMode {
	#[default]
	Off,
	Tracks,
	Buses,
	Selected,
}

impl StemMode {
	pub const ALL: [StemMode; 4] = [Self::Off, Self::Tracks, Self::Buses, Self::Selected];

	pub fn label(self) -> &'static str {
		match self {
			Self::Off => "No stems",
			Self::Tracks => "Every track",
			Self::Buses => "Buses + direct tracks",
			Self::Selected => "Chosen tracks",
		}
	}
}

/// File format choices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatChoice {
	Wav16,
	#[default]
	Wav24,
	Wav32f,
	Flac16,
	Flac24,
	Mp3,
}

impl FormatChoice {
	pub fn all() -> Vec<FormatChoice> {
		let mut v = vec![
			Self::Wav16,
			Self::Wav24,
			Self::Wav32f,
			Self::Flac16,
			Self::Flac24,
		];
		if cadenza_engine::MP3_SUPPORTED {
			v.push(Self::Mp3);
		}
		v
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::Wav16 => "WAV 16-bit",
			Self::Wav24 => "WAV 24-bit",
			Self::Wav32f => "WAV 32-bit float",
			Self::Flac16 => "FLAC 16-bit",
			Self::Flac24 => "FLAC 24-bit",
			Self::Mp3 => "MP3",
		}
	}

	/// Formats below 32-bit float are dithered (when dither is on).
	pub fn ditherable(self) -> bool {
		!matches!(self, Self::Wav32f | Self::Mp3)
	}

	pub fn options(self, mp3_kbps: u16, dither: bool) -> ExportOptions {
		let mut o = match self {
			Self::Wav16 => ExportOptions::wav(WavSampleFormat::Int16),
			Self::Wav24 => ExportOptions::wav(WavSampleFormat::Int24),
			Self::Wav32f => ExportOptions::wav(WavSampleFormat::Float32),
			Self::Flac16 => ExportOptions::flac(FlacBitDepth::Bits16),
			Self::Flac24 => ExportOptions::flac(FlacBitDepth::Bits24),
			Self::Mp3 => ExportOptions::mp3(mp3_kbps),
		};
		if !dither {
			o.dither = cadenza_audio_io::Dither::None;
		}
		o
	}
}

/// Normalization choices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeChoice {
	#[default]
	Off,
	Peak,
	Loudness,
}

/// Output sample rates offered (0 = the engine rate).
pub const SAMPLE_RATES: [u32; 5] = [0, 44_100, 48_000, 88_200, 96_000];

/// Every dialog setting (persisted in the user's preferences, except the per-project
/// folder/name/tracks which start from the project).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
	pub range: RangeKind,
	pub from_bar: u32,
	pub to_bar: u32,
	pub tail: bool,
	pub mixdown: bool,
	pub stems: StemMode,
	pub stems_through_master: bool,
	pub stems_pre_fader: bool,
	pub format: FormatChoice,
	pub mp3_kbps: u16,
	pub dither: bool,
	/// 0 = the engine rate.
	pub sample_rate: u32,
	pub normalize: NormalizeChoice,
	pub peak_db: f32,
	pub lufs: f32,
	pub true_peak_db: f32,
	pub normalize_stems_individually: bool,
	pub artist: String,
	pub album: String,
	pub genre: String,
	pub date: String,
	pub comment: String,
}

impl Default for ExportSettings {
	fn default() -> Self {
		Self {
			range: RangeKind::Song,
			from_bar: 1,
			to_bar: 4,
			tail: true,
			mixdown: true,
			stems: StemMode::Off,
			stems_through_master: false,
			stems_pre_fader: false,
			format: FormatChoice::Wav24,
			mp3_kbps: 320,
			dither: true,
			sample_rate: 0,
			normalize: NormalizeChoice::Off,
			peak_db: -1.0,
			lufs: -14.0,
			true_peak_db: -1.0,
			normalize_stems_individually: false,
			artist: String::new(),
			album: String::new(),
			genre: String::new(),
			date: String::new(),
			comment: String::new(),
		}
	}
}

/// A running export.
pub struct RunningExport {
	pub job: Job<Result<ExportReport, String>>,
	pub progress: Arc<Mutex<ExportProgress>>,
	pub cancel: Arc<AtomicBool>,
}

impl RunningExport {
	pub fn progress(&self) -> ExportProgress {
		self.progress
			.lock()
			.map(|p| p.clone())
			.unwrap_or(ExportProgress {
				fraction: 0.0,
				stage: String::new(),
			})
	}

	pub fn cancel(&self) {
		self.cancel.store(true, Ordering::Relaxed);
	}

	pub fn cancelling(&self) -> bool {
		self.cancel.load(Ordering::Relaxed)
	}
}

/// The dialog: open state, settings and the per-project fields.
#[derive(Default)]
pub struct ExportDialog {
	pub open: bool,
	pub settings: ExportSettings,
	pub folder: Option<PathBuf>,
	pub name: String,
	pub title: String,
	/// Tracks for [`StemMode::Selected`].
	pub tracks: BTreeSet<TrackId>,
	pub running: Option<RunningExport>,
	/// The last finished export (or its error), shown until the dialog is closed.
	pub result: Option<Result<ExportReport, String>>,
}

/// The default export folder: the project's `Renders/`, or the user's music folder.
pub fn default_folder(project_path: Option<&Path>) -> PathBuf {
	match project_path {
		Some(p) => ProjectLayout::new(p).renders_dir(),
		None => std::env::var_os("HOME")
			.map(|h| PathBuf::from(h).join("Music"))
			.unwrap_or_else(std::env::temp_dir),
	}
}

/// The ranges the arrange view offers right now (loop region, selected clips).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RangeContext {
	pub loop_range: Option<(Tick, Tick)>,
	pub selection: Option<(Tick, Tick)>,
}

impl ExportDialog {
	/// Opens the dialog for `project` (folder/name/title start from the project the
	/// first time and whenever the project changed).
	pub fn open_for(&mut self, project: &Project, path: Option<&Path>) {
		let folder = default_folder(path);
		let name = path
			.map(cadenza_model::files::project_stem)
			.unwrap_or_else(|| project.name.clone());
		if self.folder.is_none() || self.name.is_empty() || self.title.is_empty() {
			self.folder = Some(folder);
			self.name = name;
			self.title = project.name.clone();
		}
		self.tracks
			.retain(|id| project.tracks.iter().any(|t| t.id == *id));
		self.result = None;
		self.open = true;
	}

	/// Forget per-project fields (new/open project).
	pub fn reset_project(&mut self) {
		self.folder = None;
		self.name.clear();
		self.title.clear();
		self.tracks.clear();
		self.result = None;
	}

	/// The engine job the settings describe, or why they cannot be exported.
	pub fn spec(&self, project: &Project, ranges: RangeContext) -> Result<ExportSpec, String> {
		let s = &self.settings;
		let (start, end) = match s.range {
			RangeKind::Song => (Tick::ZERO, None),
			RangeKind::Loop => {
				let (a, b) = ranges
					.loop_range
					.ok_or("there is no loop region; set one on the ruler or pick another range")?;
				(a, Some(b))
			}
			RangeKind::Selection => {
				let (a, b) = ranges
					.selection
					.ok_or("no clips are selected; select clips or pick another range")?;
				(a, Some(b))
			}
			RangeKind::Bars => {
				if s.from_bar == 0 || s.to_bar < s.from_bar {
					return Err(format!(
						"bars {}..{} are not a range; the first bar is 1 and the last one is included",
						s.from_bar, s.to_bar
					));
				}
				let meter = project.meter();
				(
					meter.bar_start(i64::from(s.from_bar) - 1),
					Some(meter.bar_start(i64::from(s.to_bar))),
				)
			}
		};
		let stems = match s.stems {
			StemMode::Off => StemSet::None,
			StemMode::Tracks => StemSet::Tracks,
			StemMode::Buses => StemSet::Buses,
			StemMode::Selected => {
				if self.tracks.is_empty() {
					return Err("choose the tracks to export as stems".into());
				}
				let ids = project
					.tracks
					.iter()
					.filter(|t| self.tracks.contains(&t.id))
					.map(|t| t.id)
					.collect();
				StemSet::Selected(ids)
			}
		};
		if !s.mixdown && stems == StemSet::None {
			return Err("nothing to export: tick Mixdown or choose stems".into());
		}
		let folder = self
			.folder
			.clone()
			.ok_or("choose a folder to export into")?;
		let name = self.name.trim();
		if name.is_empty() {
			return Err("enter a file name".into());
		}
		let normalize = match s.normalize {
			NormalizeChoice::Off => Normalize::Off,
			NormalizeChoice::Peak => Normalize::Peak { peak_db: s.peak_db },
			NormalizeChoice::Loudness => Normalize::Loudness {
				lufs: s.lufs,
				true_peak_db: s.true_peak_db,
			},
		};
		Ok(ExportSpec {
			start,
			end,
			tail: s.tail,
			mixdown: s.mixdown,
			stems,
			stems_through_master: s.stems_through_master,
			stems_pre_fader: s.stems_pre_fader,
			format: s.format.options(s.mp3_kbps, s.dither),
			sample_rate: (s.sample_rate > 0).then_some(s.sample_rate),
			normalize,
			normalize_stems_individually: s.normalize_stems_individually,
			metadata: Metadata {
				title: self.title.trim().to_owned(),
				artist: s.artist.trim().to_owned(),
				album: s.album.trim().to_owned(),
				genre: s.genre.trim().to_owned(),
				date: s.date.trim().to_owned(),
				track_number: String::new(),
				comment: s.comment.trim().to_owned(),
			},
			folder,
			name: name.to_owned(),
		})
	}

	/// The files the settings would write (for the dialog's preview line).
	pub fn planned_files(&self, project: &Project) -> Vec<String> {
		let ext = cadenza_engine::format_extension(
			&self
				.settings
				.format
				.options(self.settings.mp3_kbps, true)
				.format,
		);
		let name = cadenza_model::files::sanitize_file_name(&self.name);
		let mut out = Vec::new();
		if self.settings.mixdown {
			out.push(format!("{name}.{ext}"));
		}
		let set = match self.settings.stems {
			StemMode::Off => StemSet::None,
			StemMode::Tracks => StemSet::Tracks,
			StemMode::Buses => StemSet::Buses,
			StemMode::Selected => StemSet::Selected(self.tracks.iter().copied().collect()),
		};
		if let Ok(stems) = cadenza_engine::stem_tracks(project, &set) {
			out.extend(stems.into_iter().map(|(_, t)| {
				format!(
					"{name} - {}.{ext}",
					cadenza_model::files::sanitize_file_name(&t)
				)
			}));
		}
		out
	}
}

/// "12.3 MB".
pub fn human_bytes(bytes: u64) -> String {
	let b = bytes as f64;
	if b >= 1024.0 * 1024.0 * 1024.0 {
		format!("{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
	} else if b >= 1024.0 * 1024.0 {
		format!("{:.1} MB", b / (1024.0 * 1024.0))
	} else {
		format!("{:.0} KB", (b / 1024.0).ceil())
	}
}

/// Opens `dir` in the system file manager (detached; errors only matter to the user).
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
	let program = if cfg!(target_os = "macos") {
		"open"
	} else if cfg!(target_os = "windows") {
		"explorer"
	} else {
		"xdg-open"
	};
	std::process::Command::new(program)
		.arg(dir)
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.spawn()
		.map(|_| ())
}

#[cfg(test)]
mod tests {
	use cadenza_model::{Track, TrackKind};

	use super::*;

	fn project() -> Project {
		let mut p = Project::new("Song");
		p.tracks.push(Track::new("Keys", TrackKind::Instrument));
		p.tracks.push(Track::new("FX", TrackKind::Bus));
		p
	}

	#[test]
	fn settings_map_to_an_engine_job() {
		let p = project();
		let mut d = ExportDialog::default();
		d.open_for(&p, Some(Path::new("/music/Song/Song.cadenza.json")));
		assert_eq!(d.folder, Some(PathBuf::from("/music/Song/Renders")));
		assert_eq!(d.name, "Song");
		let spec = d.spec(&p, RangeContext::default()).unwrap();
		assert_eq!((spec.start, spec.end, spec.tail), (Tick::ZERO, None, true));
		assert_eq!(spec.metadata.title, "Song");
		assert_eq!(spec.format, ExportOptions::wav(WavSampleFormat::Int24));
		assert_eq!(d.planned_files(&p), vec!["Song.wav"]);

		d.settings.range = RangeKind::Bars;
		d.settings.from_bar = 3;
		d.settings.to_bar = 4;
		let spec = d.spec(&p, RangeContext::default()).unwrap();
		assert_eq!(
			(spec.start, spec.end),
			(Tick(960 * 8), Some(Tick(960 * 16)))
		);
		d.settings.to_bar = 2;
		assert!(d.spec(&p, RangeContext::default()).is_err());

		d.settings.range = RangeKind::Loop;
		assert!(
			d.spec(&p, RangeContext::default())
				.unwrap_err()
				.contains("loop")
		);
		let ctx = RangeContext {
			loop_range: Some((Tick(960), Tick(1920))),
			selection: None,
		};
		assert_eq!(d.spec(&p, ctx).unwrap().end, Some(Tick(1920)));

		d.settings.range = RangeKind::Song;
		d.settings.stems = StemMode::Buses;
		d.settings.format = FormatChoice::Flac16;
		d.settings.dither = false;
		d.settings.sample_rate = 44_100;
		d.settings.normalize = NormalizeChoice::Loudness;
		let spec = d.spec(&p, ctx).unwrap();
		assert_eq!(spec.stems, StemSet::Buses);
		assert_eq!(spec.format.dither, cadenza_audio_io::Dither::None);
		assert_eq!(spec.sample_rate, Some(44_100));
		assert_eq!(
			spec.normalize,
			Normalize::Loudness {
				lufs: -14.0,
				true_peak_db: -1.0
			}
		);
		assert_eq!(
			d.planned_files(&p),
			vec!["Song.flac", "Song - Keys.flac", "Song - FX.flac"]
		);

		d.settings.stems = StemMode::Selected;
		assert!(d.spec(&p, ctx).unwrap_err().contains("choose the tracks"));
		d.tracks.insert(p.tracks[1].id);
		d.settings.mixdown = false;
		assert_eq!(
			d.spec(&p, ctx).unwrap().stems,
			StemSet::Selected(vec![p.tracks[1].id])
		);
		d.tracks.clear();
		d.settings.stems = StemMode::Off;
		assert!(d.spec(&p, ctx).unwrap_err().contains("nothing to export"));
		assert_eq!(human_bytes(5 * 1024 * 1024 + 1), "5.0 MB");
	}
}
