//! Offline time-stretch and pitch-shift using Signalsmith Stretch (MIT), a
//! phase-vocoder with multi-channel phase coherence and transient handling.

use signalsmith_stretch::Stretch;

use crate::buffer::AudioBuffer;
use crate::error::{AudioIoError, Result};

/// Accepted range for [`time_stretch`] ratios.
pub const STRETCH_RATIO_RANGE: std::ops::RangeInclusive<f64> = 0.05..=20.0;
/// Accepted range for [`pitch_shift`] semitones.
pub const PITCH_SEMITONES_RANGE: std::ops::RangeInclusive<f32> = -48.0..=48.0;

/// Changes duration without changing pitch. `ratio` is output length / input length
/// (2.0 = twice as long / half speed, 0.5 = twice as fast). The output has exactly
/// `round(frames * ratio)` frames and stays time-aligned (no added latency).
pub fn time_stretch(buffer: &AudioBuffer, ratio: f64) -> Result<AudioBuffer> {
	stretch_and_shift(buffer, ratio, 0.0)
}

/// Transposes by `semitones` keeping the duration.
pub fn pitch_shift(buffer: &AudioBuffer, semitones: f32) -> Result<AudioBuffer> {
	stretch_and_shift(buffer, 1.0, semitones)
}

/// Combined stretch (see [`time_stretch`]) and transposition in one pass, which
/// sounds better than chaining both.
pub fn stretch_and_shift(buffer: &AudioBuffer, ratio: f64, semitones: f32) -> Result<AudioBuffer> {
	if !ratio.is_finite() || !STRETCH_RATIO_RANGE.contains(&ratio) {
		return Err(AudioIoError::InvalidArgument(format!(
			"stretch ratio must be between {} and {} (output length / input length), got {ratio}",
			STRETCH_RATIO_RANGE.start(),
			STRETCH_RATIO_RANGE.end()
		)));
	}
	if !semitones.is_finite() || !PITCH_SEMITONES_RANGE.contains(&semitones) {
		return Err(AudioIoError::InvalidArgument(format!(
			"pitch shift must be between {} and {} semitones, got {semitones}",
			PITCH_SEMITONES_RANGE.start(),
			PITCH_SEMITONES_RANGE.end()
		)));
	}
	let n_ch = buffer.num_channels();
	let in_frames = buffer.frames();
	let out_frames = (in_frames as f64 * ratio).round() as usize;
	if in_frames == 0 || out_frames == 0 {
		return AudioBuffer::silence(buffer.sample_rate(), n_ch, out_frames);
	}
	let channels_u32 = u32::try_from(n_ch)
		.map_err(|_| AudioIoError::InvalidArgument("too many channels".into()))?;
	let mut stretch = Stretch::preset_default(channels_u32, buffer.sample_rate());
	if semitones != 0.0 {
		stretch.set_transpose_factor_semitones(semitones, None);
	}
	let mut input = buffer.to_interleaved();
	// `exact` needs an output of at least twice the algorithm latency; pad short
	// inputs with silence (keeping the ratio) and trim the result afterwards.
	let min_out = 2 * stretch.output_latency() + 1;
	let (padded_in, padded_out) = if out_frames < min_out {
		let padded_in = (min_out as f64 / ratio).ceil() as usize;
		let padded_out = (padded_in as f64 * ratio).round() as usize;
		(padded_in.max(in_frames), padded_out.max(min_out))
	} else {
		(in_frames, out_frames)
	};
	input.resize(padded_in * n_ch, 0.0);
	let mut output = vec![0.0f32; padded_out * n_ch];
	if !stretch.exact(&input, &mut output) {
		return Err(AudioIoError::Stretch(format!(
			"Signalsmith Stretch rejected {in_frames} -> {out_frames} frames"
		)));
	}
	output.truncate(out_frames * n_ch);
	AudioBuffer::from_interleaved(buffer.sample_rate(), n_ch, &output)
}
