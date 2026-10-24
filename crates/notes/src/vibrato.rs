//! Vibrato detection and removal on a pitch curve.
//!
//! Over a sliding window of ~2 cycles the curve is detrended (a line: drift and slow glides
//! are not vibrato) and fitted with a sinusoid at every rate in 4..8.5 Hz. A frame is
//! vibrato when the best sinusoid has a plausible depth and explains most of what is left.
//! Inside vibrato regions the curve is replaced by its mean over one local period, which
//! removes a sinusoid exactly and keeps lines: the result is what the singer "means", the
//! curve segmentation and pitch centers work on. The difference is the modulation.

/// Rates scanned, Hz.
const RATES: (f64, f64, f64) = (4.0, 8.5, 0.25);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VibratoOptions {
	/// Half window, seconds (0.25 s = two cycles at 4 Hz).
	pub half_window: f64,
	/// Share of the detrended variance the sinusoid must explain.
	pub min_explained: f64,
	/// Peak depth range, cents.
	pub min_depth: f64,
	pub max_depth: f64,
	/// Shortest vibrato region, seconds.
	pub min_length: f64,
	/// The sinusoid's error must be below this share of the best staircase fit's error.
	pub step_ratio: f64,
	/// Share of the zero crossings a sinusoid at the found rate would have.
	pub min_crossings: f64,
}

impl Default for VibratoOptions {
	fn default() -> Self {
		Self {
			half_window: 0.25,
			min_explained: 0.6,
			min_depth: 12.0,
			max_depth: 200.0,
			min_length: 0.35,
			step_ratio: 0.5,
			min_crossings: 0.75,
		}
	}
}

/// Vibrato found at one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VibratoFrame {
	pub rate_hz: f64,
	/// Peak deviation, cents.
	pub depth_cents: f64,
}

/// Per-frame vibrato of a voiced run (`cents`: pitch in cents, every frame voiced).
pub fn detect(cents: &[f64], hop_seconds: f64, opts: &VibratoOptions) -> Vec<Option<VibratoFrame>> {
	let n = cents.len();
	let half = (opts.half_window / hop_seconds).round() as usize;
	let mut out = vec![None; n];
	if n < 2 * half + 1 || half < 4 {
		return out;
	}
	let mut residual = vec![0.0f64; 2 * half + 1];
	for (t, slot) in out.iter_mut().enumerate().take(n - half).skip(half) {
		let w = &cents[t - half..=t + half];
		// Least-squares line through the window.
		let m = w.len() as f64;
		let xm = (m - 1.0) / 2.0;
		let ym = w.iter().sum::<f64>() / m;
		let (mut sxy, mut sxx) = (0.0, 0.0);
		for (i, y) in w.iter().enumerate() {
			let x = i as f64 - xm;
			sxy += x * (y - ym);
			sxx += x * x;
		}
		let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
		let mut total = 0.0;
		for (i, y) in w.iter().enumerate() {
			residual[i] = y - ym - slope * (i as f64 - xm);
			total += residual[i] * residual[i];
		}
		if total <= 0.0 {
			continue;
		}
		let mut best = (0.0, 0.0, 0.0); // explained energy, rate, amplitude
		let mut rate = RATES.0;
		while rate <= RATES.1 + 1e-9 {
			let omega = 2.0 * std::f64::consts::PI * rate * hop_seconds;
			let (mut c, mut s) = (0.0, 0.0);
			for (i, r) in residual.iter().enumerate() {
				let phase = omega * i as f64;
				c += r * phase.cos();
				s += r * phase.sin();
			}
			let amplitude = 2.0 * (c * c + s * s).sqrt() / m;
			let explained = m * amplitude * amplitude / 2.0;
			if explained > best.0 {
				best = (explained, rate, amplitude);
			}
			rate += RATES.2;
		}
		let (explained, rate, depth) = best;
		// Note changes inside the window (steps) also leave a big low-rate residual; vibrato
		// must explain the curve clearly better than a staircase of up to three notes.
		let sine_error = (total - explained).max(0.0);
		// Vibrato alternates: about two zero crossings of the residual per cycle.
		let crossings = residual
			.windows(2)
			.filter(|p| (p[0] < 0.0) != (p[1] < 0.0))
			.count() as f64;
		let expected = 2.0 * rate * m * hop_seconds;
		if explained / total >= opts.min_explained
			&& (opts.min_depth..=opts.max_depth).contains(&depth)
			&& crossings >= opts.min_crossings * expected
			&& sine_error < opts.step_ratio * best_step_error(w)
			&& swings(w) >= opts.min_depth
		{
			*slot = Some(VibratoFrame {
				rate_hz: rate,
				depth_cents: depth,
			});
		}
	}
	// A vibrato frame vouches for the whole window it saw; then drop short regions.
	let mut marked = vec![false; n];
	for (t, v) in out.iter().enumerate() {
		if v.is_some() {
			let period = (1.0 / (v.unwrap().rate_hz * hop_seconds)).round() as usize;
			let reach = period.min(half);
			for m in marked
				.iter_mut()
				.take((t + reach + 1).min(n))
				.skip(t.saturating_sub(reach))
			{
				*m = true;
			}
		}
	}
	let min_frames = (opts.min_length / hop_seconds).round() as usize;
	let mut t = 0;
	while t < n {
		if !marked[t] {
			t += 1;
			continue;
		}
		let start = t;
		while t < n && marked[t] {
			t += 1;
		}
		let keep = t - start >= min_frames;
		// Every frame of a kept region gets the latest detected rate/depth (the first one
		// before any detection).
		let mut current = out[start..t].iter().find_map(|v| *v);
		for v in out.iter_mut().take(t).skip(start) {
			if !keep {
				*v = None;
			} else if v.is_some() {
				current = *v;
			} else {
				*v = current;
			}
		}
	}
	out
}

/// The smaller of the largest rise and the largest fall inside the window: vibrato goes both
/// ways, a run of rising (or falling) notes only one.
fn swings(w: &[f64]) -> f64 {
	let (mut low, mut high) = (w[0], w[0]);
	let (mut rise, mut fall) = (0.0f64, 0.0f64);
	for &v in w {
		rise = rise.max(v - low);
		fall = fall.max(high - v);
		low = low.min(v);
		high = high.max(v);
	}
	rise.min(fall)
}

/// Squared error of the best fit by up to three constant pieces (two jumps): a short run of
/// notes inside the window. Exact DP over the cut points with prefix sums.
fn best_step_error(w: &[f64]) -> f64 {
	let n = w.len();
	let mut s = vec![0.0f64; n + 1];
	let mut q = vec![0.0f64; n + 1];
	for (i, v) in w.iter().enumerate() {
		s[i + 1] = s[i] + v;
		q[i + 1] = q[i] + v * v;
	}
	let sse = |a: usize, b: usize| {
		let (sum, sq, m) = (s[b] - s[a], q[b] - q[a], (b - a) as f64);
		(sq - sum * sum / m).max(0.0)
	};
	// one[j] = best single piece over 0..j; two[j] = best two pieces over 0..j.
	let one: Vec<f64> = (0..=n)
		.map(|j| if j == 0 { 0.0 } else { sse(0, j) })
		.collect();
	let two: Vec<f64> = (0..=n)
		.map(|j| {
			(1..j)
				.map(|k| one[k] + sse(k, j))
				.fold(f64::INFINITY, f64::min)
		})
		.collect();
	(2..n)
		.map(|k| two[k] + sse(k, n))
		.fold(one[n].min(two[n]), f64::min)
}

/// The curve without vibrato: inside vibrato frames, the mean over one local period.
pub fn remove(cents: &[f64], vibrato: &[Option<VibratoFrame>], hop_seconds: f64) -> Vec<f64> {
	let n = cents.len();
	let mut prefix = vec![0.0f64; n + 1];
	for (i, c) in cents.iter().enumerate() {
		prefix[i + 1] = prefix[i] + c;
	}
	(0..n)
		.map(|t| match vibrato[t] {
			None => cents[t],
			Some(v) => {
				let period = (1.0 / (v.rate_hz * hop_seconds)).round().max(2.0) as usize;
				let mut a = t.saturating_sub(period / 2);
				let mut b = (a + period).min(n);
				a = b.saturating_sub(period);
				if b <= a {
					b = (a + 1).min(n);
				}
				(prefix[b] - prefix[a]) / (b - a) as f64
			}
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	const HOP: f64 = 0.005;

	fn curve(seconds: f64, f: impl Fn(f64) -> f64) -> Vec<f64> {
		(0..(seconds / HOP) as usize)
			.map(|i| f(i as f64 * HOP))
			.collect()
	}

	#[test]
	fn finds_vibrato_and_removes_it() {
		let c = curve(1.5, |t| {
			6000.0 + 20.0 * t + 60.0 * (2.0 * std::f64::consts::PI * 5.5 * t).sin()
		});
		let v = detect(&c, HOP, &VibratoOptions::default());
		let found = v.iter().filter(|x| x.is_some()).count();
		assert!(found > c.len() * 9 / 10, "{found} of {}", c.len());
		let mid = v[c.len() / 2].unwrap();
		assert!((mid.rate_hz - 5.5).abs() <= 0.25, "{mid:?}");
		assert!((mid.depth_cents - 60.0).abs() < 8.0, "{mid:?}");
		let q = remove(&c, &v, HOP);
		for (t, v) in q.iter().enumerate().take(c.len() - 60).skip(60) {
			let line = 6000.0 + 20.0 * t as f64 * HOP;
			assert!((v - line).abs() < 6.0, "{t}: {v} vs {line}");
		}
	}

	#[test]
	fn steps_and_glides_are_not_vibrato() {
		let one = curve(1.0, |t| if t < 0.5 { 5194.0 } else { 5289.0 });
		assert!(
			detect(&one, HOP, &VibratoOptions::default())
				.iter()
				.all(Option::is_none)
		);
		let up_down = curve(1.0, |t| {
			if (0.4..0.65).contains(&t) {
				5300.0
			} else {
				5200.0
			}
		});
		assert!(
			detect(&up_down, HOP, &VibratoOptions::default())
				.iter()
				.all(Option::is_none)
		);
		let steps = curve(1.5, |t| 6000.0 + 200.0 * (t / 0.3).floor());
		assert!(
			detect(&steps, HOP, &VibratoOptions::default())
				.iter()
				.all(Option::is_none)
		);
		let glide = curve(1.0, |t| 6000.0 + 400.0 * t);
		assert!(
			detect(&glide, HOP, &VibratoOptions::default())
				.iter()
				.all(Option::is_none)
		);
		let climb = curve(1.5, |t| {
			5000.0 + [0.0, 200.0, 300.0, 500.0, 600.0, 800.0][(t / 0.25) as usize]
		});
		assert!(
			detect(&climb, HOP, &VibratoOptions::default())
				.iter()
				.all(Option::is_none)
		);
		let flat = curve(1.0, |_| 6000.0);
		assert!(
			detect(&flat, HOP, &VibratoOptions::default())
				.iter()
				.all(Option::is_none)
		);
	}
}
