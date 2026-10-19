//! Pure, allocation-light functions that compute the static pictures (EQ
//! curves, filter responses, saturator transfer curve, delay sync steps,
//! compressor transfer curve) device editors draw, from plain parameter
//! values. Every function here reuses the real processor's own coefficient
//! or shaping code (see each item's doc), so the picture and the sound
//! cannot drift apart.

pub use crate::effects::compressor::compressor_curve_db;
pub use crate::effects::delay::{delay_sync_steps, delay_time_seconds};
pub use crate::effects::eq::{EqBands, N_BANDS, eq_band_response_db, eq_response_db};
pub use crate::effects::saturator::{Mode as SaturatorMode, saturator_shape};
pub use crate::instruments::poly_synth::poly_synth_filter_response_db;
pub use crate::instruments::sampler::sampler_filter_response_db;
pub use crate::util::svf::{SvfOutput, svf_magnitude_db};
