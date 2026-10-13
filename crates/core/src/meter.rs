//! Time signature changes: bar/beat math over a song whose meter changes on bar lines.
//!
//! Changes are stored by bar index (0 = bar 1), not by tick: a change always starts a bar,
//! and editing an earlier signature moves later bar lines without ever leaving a change
//! in the middle of a bar. Content stays at its ticks (like tempo changes).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tempo::TimeSignature;
use crate::time::Tick;

/// A time signature that starts at bar `bar` (0-based bar index: 0 = bar 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MeterChange {
	/// 0-based bar index (bar 1 = 0; changes always start at index >= 1).
	pub bar: u32,
	pub numerator: u32,
	pub denominator: u32,
}

impl MeterChange {
	pub fn new(bar: u32, signature: TimeSignature) -> Self {
		Self {
			bar,
			numerator: signature.numerator,
			denominator: signature.denominator,
		}
	}

	pub fn signature(&self) -> TimeSignature {
		TimeSignature {
			numerator: self.numerator,
			denominator: self.denominator,
		}
	}
}

/// The bar holding a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarInfo {
	/// 0-based bar index (negative before the song start).
	pub index: i64,
	pub start: Tick,
	pub length: Tick,
	pub signature: TimeSignature,
}

impl BarInfo {
	pub fn end(&self) -> Tick {
		self.start + self.length
	}

	/// Length of one beat (the signature's denominator note) in ticks.
	pub fn beat_length(&self) -> Tick {
		beat_ticks(self.signature)
	}
}

/// Length of one beat (the denominator note value) in ticks.
pub fn beat_ticks(ts: TimeSignature) -> Tick {
	Tick((crate::PPQ * 4 / i64::from(ts.denominator.max(1))).max(1))
}

fn bar_ticks(ts: TimeSignature) -> i64 {
	ts.ticks_per_bar().0.max(1)
}

/// A borrowed view of a song's time signatures: the signature of bar 1 plus changes
/// sorted by bar. Cheap to copy and allocation free (usable on the audio thread).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter<'a> {
	first: TimeSignature,
	changes: &'a [MeterChange],
}

impl<'a> Meter<'a> {
	/// `changes` must be sorted by bar with indexes >= 1 (as the model keeps them).
	pub fn new(first: TimeSignature, changes: &'a [MeterChange]) -> Self {
		Self { first, changes }
	}

	pub fn first(&self) -> TimeSignature {
		self.first
	}

	pub fn changes(&self) -> &'a [MeterChange] {
		self.changes
	}

	pub fn is_constant(&self) -> bool {
		self.changes.is_empty()
	}

	/// Segments: (first bar index, start tick, signature), in order.
	fn segments(&self) -> impl Iterator<Item = (i64, i64, TimeSignature)> + '_ {
		let mut bar = 0i64;
		let mut tick = 0i64;
		let mut ts = self.first;
		let mut first = true;
		let mut i = 0usize;
		std::iter::from_fn(move || {
			if first {
				first = false;
				return Some((0, 0, self.first));
			}
			let c = self.changes.get(i)?;
			i += 1;
			let to = i64::from(c.bar).max(bar);
			tick += (to - bar) * bar_ticks(ts);
			bar = to;
			ts = c.signature();
			Some((bar, tick, ts))
		})
	}

	/// The segment holding bar `index` (index >= 0).
	fn segment_of_bar(&self, index: i64) -> (i64, i64, TimeSignature) {
		let mut found = (0, 0, self.first);
		for s in self.segments() {
			if s.0 > index {
				break;
			}
			found = s;
		}
		found
	}

	/// The segment holding `tick` (tick >= 0).
	fn segment_of_tick(&self, tick: i64) -> (i64, i64, TimeSignature) {
		let mut found = (0, 0, self.first);
		for s in self.segments() {
			if s.1 > tick {
				break;
			}
			found = s;
		}
		found
	}

	/// Signature in effect at bar `index` (0-based; bars before the start use bar 1's).
	pub fn signature_at_bar(&self, index: i64) -> TimeSignature {
		self.segment_of_bar(index.max(0)).2
	}

	/// Signature in effect at `tick`.
	pub fn signature_at(&self, tick: Tick) -> TimeSignature {
		self.segment_of_tick(tick.0.max(0)).2
	}

	/// Start of bar `index` (0-based; negative indexes extend bar 1's signature backwards).
	pub fn bar_start(&self, index: i64) -> Tick {
		if index < 0 {
			return Tick(index * bar_ticks(self.first));
		}
		let (bar, tick, ts) = self.segment_of_bar(index);
		Tick(tick + (index - bar) * bar_ticks(ts))
	}

	/// The bar holding `tick`.
	pub fn bar_at(&self, tick: Tick) -> BarInfo {
		if tick.0 < 0 {
			let len = bar_ticks(self.first);
			let index = tick.0.div_euclid(len);
			return BarInfo {
				index,
				start: Tick(index * len),
				length: Tick(len),
				signature: self.first,
			};
		}
		let (bar, start, ts) = self.segment_of_tick(tick.0);
		let len = bar_ticks(ts);
		let k = (tick.0 - start) / len;
		BarInfo {
			index: bar + k,
			start: Tick(start + k * len),
			length: Tick(len),
			signature: ts,
		}
	}

	/// 0-based index of the bar holding `tick`.
	pub fn bar_index(&self, tick: Tick) -> i64 {
		self.bar_at(tick).index
	}

	/// Start of the bar holding `tick`.
	pub fn bar_floor(&self, tick: Tick) -> Tick {
		self.bar_at(tick).start
	}

	/// `tick` when it is on a bar line, else the start of the next bar.
	pub fn bar_ceil(&self, tick: Tick) -> Tick {
		let b = self.bar_at(tick);
		if b.start == tick { tick } else { b.end() }
	}

	/// Whether `tick` is on a bar line.
	pub fn is_bar_line(&self, tick: Tick) -> bool {
		self.bar_at(tick).start == tick
	}

	/// Position as a fractional 0-based bar index (bar 1 = 0.0, halfway through it = 0.5).
	pub fn bar_position(&self, tick: Tick) -> f64 {
		let b = self.bar_at(tick);
		b.index as f64 + (tick - b.start).0 as f64 / b.length.0 as f64
	}

	/// Inverse of [`Meter::bar_position`] (rounded to the nearest tick).
	pub fn tick_at_bar_position(&self, position: f64) -> Tick {
		let index = position.floor() as i64;
		let start = self.bar_start(index);
		let len = bar_ticks(self.signature_at_bar(index));
		Tick(start.0 + ((position - index as f64) * len as f64).round() as i64)
	}

	/// Bar, beat (both 1-based; beats in the signature's denominator note) and ticks into
	/// the beat.
	pub fn bbt(&self, tick: Tick) -> (i64, i64, i64) {
		let b = self.bar_at(tick);
		let beat = b.beat_length().0;
		let into = (tick - b.start).0;
		(b.index + 1, into / beat + 1, into % beat)
	}

	/// Position of a 1-based bar and 1-based beat (beats in the bar's denominator note;
	/// fractional beats allowed).
	pub fn tick_at_bbt(&self, bar: i64, beat: f64) -> Tick {
		let index = bar - 1;
		let start = self.bar_start(index);
		let unit = beat_ticks(self.signature_at_bar(index)).0 as f64;
		Tick(start.0 + ((beat - 1.0) * unit).round() as i64)
	}

	/// Bar lines in `[from, to)` (ticks), in order.
	pub fn bar_lines(&self, from: Tick, to: Tick) -> impl Iterator<Item = BarInfo> + '_ {
		let mut next = self.bar_at(from);
		if next.start < from {
			next = self.bar_at(next.end());
		}
		std::iter::from_fn(move || {
			if next.start >= to {
				return None;
			}
			let out = next;
			next = self.bar_at(out.end());
			Some(out)
		})
	}

	/// Signature segments overlapping `[from, to)` as (start tick, signature), in order;
	/// the first one may start before `from`.
	pub fn segments_between(
		&self,
		from: Tick,
		to: Tick,
	) -> impl Iterator<Item = (Tick, TimeSignature)> + '_ {
		let from = from.0.max(0);
		let mut items = self.segments().peekable();
		std::iter::from_fn(move || {
			loop {
				let s = items.next()?;
				if s.1 >= to.0 && s.1 > 0 {
					return None;
				}
				let next_start = items.peek().map(|n| n.1);
				if next_start.is_some_and(|n| n <= from) {
					continue;
				}
				return Some((Tick(s.1), s.2));
			}
		})
	}

	/// Tick of the start of every signature change (bar 1 excluded), with its signature.
	pub fn change_ticks(&self) -> impl Iterator<Item = (u32, Tick, TimeSignature)> + '_ {
		self.segments()
			.skip(1)
			.map(|(bar, tick, ts)| (bar as u32, Tick(tick), ts))
	}
}

/// An owned time signature map (the signature of bar 1 plus changes), for holders that
/// outlive the project (graphs, caches).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MeterMap {
	pub first: TimeSignature,
	pub changes: Vec<MeterChange>,
}

impl MeterMap {
	pub fn new(meter: Meter<'_>) -> Self {
		Self {
			first: meter.first(),
			changes: meter.changes().to_vec(),
		}
	}

	pub fn view(&self) -> Meter<'_> {
		Meter::new(self.first, &self.changes)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::PPQ;

	const fn ts(numerator: u32, denominator: u32) -> TimeSignature {
		TimeSignature {
			numerator,
			denominator,
		}
	}

	#[test]
	fn constant_meter() {
		let m = Meter::new(ts(4, 4), &[]);
		assert_eq!(m.bar_start(2), Tick(8 * PPQ));
		assert_eq!(m.bar_index(Tick(8 * PPQ - 1)), 1);
		assert_eq!(m.bbt(Tick(5 * PPQ + 10)), (2, 2, 10));
		assert_eq!(m.bar_ceil(Tick(1)), Tick(4 * PPQ));
		assert_eq!(m.bar_ceil(Tick(4 * PPQ)), Tick(4 * PPQ));
		assert_eq!(m.bar_floor(Tick(-1)), Tick(-4 * PPQ));
		assert_eq!(m.bar_index(Tick(-1)), -1);
	}

	#[test]
	fn changes_start_on_bar_lines() {
		// 4/4 for two bars, 3/4 from bar 3, 7/8 from bar 5.
		let changes = [MeterChange::new(2, ts(3, 4)), MeterChange::new(4, ts(7, 8))];
		let m = Meter::new(ts(4, 4), &changes);
		assert_eq!(m.bar_start(2), Tick(8 * PPQ));
		assert_eq!(m.bar_start(3), Tick(11 * PPQ));
		assert_eq!(m.bar_start(4), Tick(14 * PPQ));
		assert_eq!(m.bar_start(5), Tick(14 * PPQ + 7 * PPQ / 2));
		assert_eq!(m.signature_at(Tick(10 * PPQ)), ts(3, 4));
		assert_eq!(m.signature_at(Tick(14 * PPQ)), ts(7, 8));
		assert_eq!(m.signature_at(Tick(7 * PPQ)), ts(4, 4));
		let b = m.bar_at(Tick(15 * PPQ));
		assert_eq!(
			(b.index, b.start, b.length),
			(4, Tick(14 * PPQ), Tick(7 * PPQ / 2))
		);
		// Beats of 7/8 are eighths.
		assert_eq!(m.bbt(Tick(15 * PPQ)), (5, 3, 0));
		assert_eq!(m.tick_at_bbt(5, 3.0), Tick(15 * PPQ));
		assert!((m.bar_position(Tick(9 * PPQ + PPQ / 2)) - 2.5).abs() < 1e-9);
		assert_eq!(m.tick_at_bar_position(2.5), Tick(9 * PPQ + PPQ / 2));
		let lines: Vec<i64> = m
			.bar_lines(Tick(7 * PPQ), Tick(15 * PPQ))
			.map(|b| b.start.0 / PPQ)
			.collect();
		assert_eq!(lines, vec![8, 11, 14]);
		let changes: Vec<_> = m.change_ticks().collect();
		assert_eq!(changes[0], (2, Tick(8 * PPQ), ts(3, 4)));
		assert_eq!(changes[1], (4, Tick(14 * PPQ), ts(7, 8)));
	}

	#[test]
	fn segments_between_ranges() {
		let changes = [MeterChange::new(2, ts(3, 4)), MeterChange::new(4, ts(7, 8))];
		let m = Meter::new(ts(4, 4), &changes);
		let all: Vec<_> = m.segments_between(Tick(0), Tick(100 * PPQ)).collect();
		assert_eq!(all.len(), 3);
		let mid: Vec<_> = m.segments_between(Tick(9 * PPQ), Tick(12 * PPQ)).collect();
		assert_eq!(mid, vec![(Tick(8 * PPQ), ts(3, 4))]);
		let edge: Vec<_> = m
			.segments_between(Tick(7 * PPQ), Tick(14 * PPQ + 1))
			.collect();
		assert_eq!(edge.len(), 3);
	}

	#[test]
	fn json_shape() {
		let c = MeterChange::new(8, ts(6, 8));
		let json = serde_json::to_string(&c).unwrap();
		assert_eq!(json, r#"{"bar":8,"numerator":6,"denominator":8}"#);
	}
}
