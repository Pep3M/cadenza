//! Track renders for freeze, bounce in place and consolidation: one track (or some of its
//! clips) rendered alone, before its fader, to a WAV file.
//!
//! The render is a synthetic project holding only that track at 0 dB, centered, with no
//! sends, routed straight to a clean master: what the track's fader receives (instrument,
//! clips, inserts and their parameter automation). Played back through the track's own
//! fader it sounds like the original.

use std::path::Path;

use cadenza_core::{ClipId, ParamTarget, Tick, TrackId};
use cadenza_model::{Channel, Project, Track};

use crate::analysis::MixReport;
use crate::error::EngineError;
use crate::render::{RenderOptions, render_to_wav};

/// What to render.
#[derive(Debug, Clone, PartialEq)]
pub enum BounceScope {
	/// The whole track: instrument + clips + inserts (freeze, bounce in place).
	Track,
	/// Only these clips of the track, without inserts (consolidation; an instrument track
	/// keeps its instrument).
	Clips(Vec<ClipId>),
}

/// The synthetic one-track project rendered for `track`.
pub fn bounce_project(
	project: &Project,
	track: TrackId,
	scope: &BounceScope,
) -> Result<Project, EngineError> {
	let original = project
		.track(track)
		.ok_or_else(|| EngineError::Bounce(format!("track {track} does not exist")))?;
	let mut t: Track = original.clone();
	t.channel = Channel {
		inserts: original.channel.inserts.clone(),
		..Channel::default()
	};
	t.folder = None;
	t.freeze = None;
	t.input = None;
	for d in t.instrument.iter_mut().chain(t.channel.inserts.iter_mut()) {
		// Keys and extra-output buses live on other tracks: not part of this render.
		d.sidechain = None;
		for r in &mut d.aux_outputs {
			r.bus = None;
		}
	}
	if let BounceScope::Clips(ids) = scope {
		t.clips.retain(|c| ids.contains(&c.id));
		t.channel.inserts.clear();
		if t.clips.is_empty() {
			return Err(EngineError::Bounce(
				"none of the clips to render are on the track".into(),
			));
		}
	}
	let mut p = Project::new(format!("{} render", original.name));
	p.tempo = project.tempo.clone();
	p.time_signature = project.time_signature;
	p.time_signature_changes = project.time_signature_changes.clone();
	p.automation = project
		.automation
		.iter()
		.filter(|l| {
			l.address.track == Some(track)
				&& matches!(l.address.target, ParamTarget::Device { .. })
				&& matches!(scope, BounceScope::Track)
		})
		.cloned()
		.collect();
	p.tracks.push(t);
	Ok(p)
}

/// Renders `track` (per `scope`) from `start` to its end plus device tails into a 32-bit
/// float stereo WAV at `path`.
pub fn render_track(
	project: &Project,
	opts: &RenderOptions,
	track: TrackId,
	scope: &BounceScope,
	start: Tick,
	path: &Path,
) -> Result<MixReport, EngineError> {
	let p = bounce_project(project, track, scope)?;
	let opts = RenderOptions {
		start,
		end: None,
		..opts.clone()
	};
	if let Some(dir) = path.parent() {
		std::fs::create_dir_all(dir)
			.map_err(|e| EngineError::Bounce(format!("cannot create '{}': {e}", dir.display())))?;
	}
	render_to_wav(&p, &opts, path)
}
