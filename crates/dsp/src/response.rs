//! Pure, allocation-light functions that compute the static pictures (EQ
//! curves, filter responses, saturator transfer curve, delay sync steps,
//! compressor transfer curve) device editors draw, from plain parameter
//! values. Every function here reuses the real processor's own coefficient
//! or shaping code (see each item's doc), so the picture and the sound
//! cannot drift apart.

pub use crate::effects::compressor::{
	KEY_HP_OFF_HZ, KEY_LP_OFF_HZ, compressor_curve_db, compressor_key_response_db,
};
pub use crate::effects::delay::{
	DelayTap, DelayTaps, MAX_TAPS as DELAY_MAX_TAPS, delay_sync_steps, delay_taps,
	delay_time_seconds,
};
pub use crate::effects::eq::{
	DYN_BANDS, EqBands, EqChannel, N_BANDS, channel_param_index, dynamic_band_gain,
	dynamic_band_gain_at, dynamic_param_indices, eq_band_response_db, eq_response_db,
	eq_response_with_offsets_db,
};
pub use crate::effects::limiter::{
	OS_LATENCY as LIMITER_OS_LATENCY, OVERSAMPLE_FACTORS, limiter_latency_samples,
};
pub use crate::effects::reverb::{DUCK_FLOOR_DB, DUCK_FULL_DB, reverb_duck_db};
pub use crate::effects::saturator::{Mode as SaturatorMode, saturator_shape};
pub use crate::instruments::poly_synth::{WAVE_WAVETABLE, poly_synth_filter_response_db};
pub use crate::instruments::sampler::sampler_filter_response_db;
pub use crate::util::svf::{SvfOutput, svf_magnitude_db};
pub use crate::util::wavetable::{
	FRAMES as WAVETABLE_FRAMES, N_TABLES as WAVETABLE_COUNT,
	TABLE_DESCRIPTIONS as WAVETABLE_DESCRIPTIONS, TABLE_NAMES as WAVETABLE_NAMES, wavetable_frame,
};
