//! Serializes `f32` values with their shortest decimal form (`-0.35`, not
//! `-0.3499999940395355`): JSON values built through `serde_json::Value` widen `f32` to
//! `f64` otherwise, which costs the AI tokens and reads as noise.

use serde::Serializer;

/// The `f64` with the same shortest decimal representation as `x` (0 for non-finite).
pub fn widen(x: f32) -> f64 {
	if x.is_finite() {
		x.to_string().parse().unwrap_or(f64::from(x))
	} else {
		0.0
	}
}

pub fn f32<S: Serializer>(x: &f32, s: S) -> Result<S::Ok, S::Error> {
	s.serialize_f64(widen(*x))
}

pub fn opt_f32<S: Serializer>(x: &Option<f32>, s: S) -> Result<S::Ok, S::Error> {
	match x {
		Some(v) => s.serialize_some(&widen(*v)),
		None => s.serialize_none(),
	}
}

pub fn vec_f32<S: Serializer>(x: &[f32], s: S) -> Result<S::Ok, S::Error> {
	s.collect_seq(x.iter().map(|v| widen(*v)))
}

#[cfg(test)]
mod tests {
	#[test]
	fn short_forms_survive_a_value_round_trip() {
		#[derive(serde::Serialize)]
		struct T {
			#[serde(serialize_with = "super::f32")]
			a: f32,
			#[serde(serialize_with = "super::opt_f32")]
			b: Option<f32>,
			#[serde(serialize_with = "super::vec_f32")]
			c: Vec<f32>,
		}
		let v = serde_json::to_value(T {
			a: -0.35,
			b: Some(1.4),
			c: vec![9.1, f32::NAN],
		})
		.unwrap();
		assert_eq!(v.to_string(), r#"{"a":-0.35,"b":1.4,"c":[9.1,0.0]}"#);
	}
}
