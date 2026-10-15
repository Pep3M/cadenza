//! Sample zones of the built-in samplers: model zones -> DSP zones with their audio
//! decoded and resampled to the engine rate through the shared [`cadenza_audio_io::AudioCache`] (at compile
//! time, never on the audio thread), plus helpers to map a new sample automatically.

use std::path::Path;
use std::sync::Arc;

use cadenza_audio_io::AudioBuffer;
use cadenza_dsp::{SampleData, Zone};
use cadenza_model::{SampleZone, root_from_file_name};

use crate::error::EngineError;

/// A cached decode shared with a sampler (no copy of the frames).
pub(crate) struct CachedSample(pub Arc<AudioBuffer>);

impl SampleData for CachedSample {
	fn sample_rate(&self) -> f32 {
		self.0.sample_rate() as f32
	}

	fn num_channels(&self) -> usize {
		self.0.num_channels()
	}

	fn channel(&self, index: usize) -> &[f32] {
		self.0.channel(index)
	}
}

/// The DSP zone of `zone` playing `sample` (`None`: silent, the file is missing).
pub(crate) fn dsp_zone(zone: &SampleZone, sample: Option<Arc<AudioBuffer>>) -> Zone {
	Zone {
		sample: sample.map(|s| Arc::new(CachedSample(s)) as Arc<dyn SampleData>),
		key_low: zone.key_low,
		key_high: zone.key_high,
		root_key: zone.root_key,
		vel_low: zone.vel_low,
		vel_high: zone.vel_high,
		tune: zone.tune,
		gain_db: zone.gain_db,
		pan: zone.pan,
		start: zone.start,
		end: zone.end,
		loop_mode: match zone.loop_mode {
			cadenza_model::LoopMode::Off => cadenza_dsp::LoopMode::Off,
			cadenza_model::LoopMode::Forward => cadenza_dsp::LoopMode::Forward,
			cadenza_model::LoopMode::PingPong => cadenza_dsp::LoopMode::PingPong,
		},
		loop_start: zone.loop_start,
		loop_end: zone.loop_end,
		loop_crossfade: zone.loop_crossfade,
		reverse: zone.reverse,
		attack_ms: zone.attack_ms,
		decay_ms: zone.decay_ms,
		choke: zone.choke,
		output: zone.output,
		round_robin: zone.round_robin,
	}
}

/// Root key of an audio file for auto-mapping: the note its name announces ("Piano C3",
/// "Strings_60"), else the detected pitch of a clearly pitched sound, else C4 (60).
/// `samples` are the decoded file (any rate) when available.
pub fn detect_root(path: &Path, samples: Option<&AudioBuffer>) -> u8 {
	let name = path
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();
	if let Some(root) = root_from_file_name(&name) {
		return root;
	}
	samples
		.and_then(|buf| {
			let mono = buf.to_mono();
			cadenza_dsp::util::pitch::detect_pitch(mono.channel(0), buf.sample_rate() as f32)
		})
		.and_then(cadenza_dsp::util::pitch::hz_to_midi)
		.unwrap_or(60)
}

/// One sample mapped across the whole keyboard at its root key ([`detect_root`]): the
/// sampler's auto-map. Fails when the file cannot be read.
pub fn auto_zone(path: &Path) -> Result<SampleZone, EngineError> {
	let decoded = cadenza_audio_io::decode_file(path).map_err(|e| {
		EngineError::Device(format!(
			"cannot load sample '{}': {e}; use a readable audio file (WAV, AIFF, FLAC, MP3, OGG)",
			path.display()
		))
	})?;
	let root = detect_root(path, Some(&decoded));
	Ok(SampleZone::full_keyboard(path, root))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roots_from_names_pitch_or_default() {
		let sr = 44_100;
		let tone: Vec<f32> = (0..sr / 2)
			.map(|n| (std::f32::consts::TAU * 220.0 * n as f32 / sr as f32).sin() * 0.5)
			.collect();
		let buf = AudioBuffer::from_planar(sr, vec![tone]).unwrap();
		assert_eq!(detect_root(Path::new("/x/Lead G2.wav"), Some(&buf)), 43);
		assert_eq!(detect_root(Path::new("/x/lead.wav"), Some(&buf)), 57);
		assert_eq!(detect_root(Path::new("/x/lead.wav"), None), 60);
	}
}
