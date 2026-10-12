//! Real-audio support for Cadenza: decoding (WAV, AIFF, FLAC, MP3, OGG/Vorbis,
//! M4A AAC/ALAC, CAF via symphonia), high-quality sample-rate conversion (rubato
//! sinc), a decoded-audio cache, multi-resolution waveform peaks, realtime-safe clip
//! DSP (fades, crossfades, gain, region rendering) and export (WAV/FLAC with TPDF
//! dither, MP3 behind the `mp3` feature). Offline time-stretch and pitch-shift live
//! behind the `stretch` feature.
//!
//! Only [`clip`] is meant for the audio thread; everything else allocates and/or
//! does I/O and must run on a worker or UI thread.

pub mod buffer;
pub mod cache;
pub mod clip;
pub mod decode;
mod error;
pub mod export;
pub mod peaks;
pub mod resample;
#[cfg(feature = "stretch")]
pub mod stretch;

pub use buffer::{AudioBuffer, stereo_downmix_gains};
pub use cache::{AudioCache, CacheKey, CacheStats};
pub use clip::{
	ClipRegion, Fade, FadeCurve, FadeDirection, apply_fade, apply_gain, apply_gain_ramp, crossfade,
	render_clip,
};
pub use decode::{AudioFile, AudioFormat, AudioInfo, decode_file, decode_to_buffer, probe};
pub use error::{AudioIoError, Result};
pub use export::{Dither, ExportFormat, ExportOptions, FlacBitDepth, WavSampleFormat, export};
pub use peaks::{DEFAULT_RESOLUTIONS, MinMax, PeakLevel, WaveformPeaks};
pub use resample::resample;
#[cfg(feature = "stretch")]
pub use stretch::{pitch_shift, stretch_and_shift, time_stretch};
