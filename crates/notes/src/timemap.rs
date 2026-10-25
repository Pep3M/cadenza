//! Piecewise-linear time map between a recording and its edited render (NE-4): note moves
//! and stretches, attacks kept at their own speed.

use crate::NotesError;

/// Output seconds <-> source seconds. Without anchors it is the identity; past the last
/// anchor (and before the first) time runs at speed 1.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimeMap {
	/// (source, output) seconds, both strictly increasing.
	anchors: Vec<(f64, f64)>,
}

impl TimeMap {
	pub fn identity() -> Self {
		Self::default()
	}

	/// `anchors`: (source, output) seconds. Duplicates of an identical pair are dropped;
	/// both columns must then strictly increase.
	pub fn new(mut anchors: Vec<(f64, f64)>) -> Result<Self, NotesError> {
		if anchors
			.iter()
			.any(|(s, o)| !s.is_finite() || !o.is_finite())
		{
			return Err(NotesError::Edit("time edits must be finite seconds".into()));
		}
		anchors.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
		anchors.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
		for w in anchors.windows(2) {
			let ((s0, o0), (s1, o1)) = (w[0], w[1]);
			if !(s1 > s0 && o1 > o0) {
				return Err(NotesError::Edit(format!(
					"the time edits cross: source {s0:.3} s -> {o0:.3} s comes before \
					 {s1:.3} s -> {o1:.3} s; a note cannot move past its neighbour, shorten or \
					 move the neighbour too"
				)));
			}
		}
		let identity = anchors.iter().all(|(s, o)| (s - o).abs() < 1e-12);
		Ok(Self {
			anchors: if identity { Vec::new() } else { anchors },
		})
	}

	pub fn is_identity(&self) -> bool {
		self.anchors.is_empty()
	}

	/// Source seconds heard at `output` seconds.
	pub fn to_source(&self, output: f64) -> f64 {
		interpolate(&self.anchors, output, |a| a.1, |a| a.0)
	}

	/// Output seconds where `source` seconds are heard.
	pub fn to_output(&self, source: f64) -> f64 {
		interpolate(&self.anchors, source, |a| a.0, |a| a.1)
	}
}

fn interpolate(
	anchors: &[(f64, f64)],
	x: f64,
	from: impl Fn(&(f64, f64)) -> f64,
	to: impl Fn(&(f64, f64)) -> f64,
) -> f64 {
	let Some(first) = anchors.first() else {
		return x;
	};
	let i = anchors.partition_point(|a| from(a) <= x);
	if i == 0 {
		return to(first) + (x - from(first));
	}
	let a = &anchors[i - 1];
	if i == anchors.len() {
		return to(a) + (x - from(a));
	}
	let b = &anchors[i];
	to(a) + (x - from(a)) * (to(b) - to(a)) / (from(b) - from(a))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn maps_both_ways_and_runs_at_speed_one_outside() {
		let m = TimeMap::new(vec![(1.0, 1.0), (2.0, 3.0), (3.0, 3.5)]).unwrap();
		assert!(!m.is_identity());
		assert_eq!(m.to_source(0.5), 0.5);
		assert_eq!(m.to_source(2.0), 1.5);
		assert_eq!(m.to_output(1.5), 2.0);
		assert_eq!(m.to_source(3.25), 2.5);
		assert_eq!(m.to_source(4.0), 3.5);
		assert_eq!(m.to_output(3.5), 4.0);
	}

	#[test]
	fn crossing_edits_are_refused_and_identity_collapses() {
		let err = TimeMap::new(vec![(1.0, 2.0), (2.0, 1.5)]).unwrap_err();
		assert!(err.to_string().contains("neighbour"), "{err}");
		assert!(
			TimeMap::new(vec![(1.0, 1.0), (2.0, 2.0)])
				.unwrap()
				.is_identity()
		);
	}
}
