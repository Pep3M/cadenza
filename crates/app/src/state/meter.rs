//! Time signature edits (pure builders): the transport field, the signature lane.

use cadenza_core::{Tick, TimeSignature};
use cadenza_model::{Command, Project};

/// The bar index (0-based) where the signature in effect at `tick` starts: 0 for the
/// song's first signature, else the bar of the change that governs `tick`.
pub fn governing_bar(project: &Project, tick: Tick) -> u32 {
	let index = project.meter().bar_index(Tick(tick.0.max(0)));
	project
		.time_signature_changes
		.iter()
		.rev()
		.find(|c| i64::from(c.bar) <= index)
		.map_or(0, |c| c.bar)
}

/// Sets the signature in effect at `tick` (the segment it lies in), like the BPM field.
pub fn set_at(project: &Project, tick: Tick, ts: TimeSignature) -> Option<Command> {
	let bar = governing_bar(project, tick);
	(project.meter().signature_at_bar(i64::from(bar)) != ts).then_some(Command::SetTimeSignature {
		time_signature: ts,
		bar,
	})
}

/// Adds (or replaces) a change at the bar holding `tick`; a change equal to the
/// signature already in effect there adds nothing.
pub fn add_change(project: &Project, tick: Tick, ts: TimeSignature) -> Option<Command> {
	let meter = project.meter();
	let bar = meter.bar_index(Tick(tick.0.max(0)));
	let bar = u32::try_from(bar).ok()?;
	if bar == 0 {
		return set_at(project, Tick::ZERO, ts);
	}
	let existing = project.time_signature_changes.iter().any(|c| c.bar == bar);
	if !existing && meter.signature_at_bar(i64::from(bar)) == ts {
		return None;
	}
	Some(Command::SetTimeSignature {
		time_signature: ts,
		bar,
	})
}

/// Moves the change at bar `from` to bar `to` (one undo step); `None` when nothing moves
/// or `to` is bar 1.
pub fn move_change(project: &Project, from: u32, to: u32) -> Option<Command> {
	if from == to || from == 0 || to == 0 {
		return None;
	}
	let change = project
		.time_signature_changes
		.iter()
		.find(|c| c.bar == from)?;
	Some(Command::Batch {
		label: format!("Move time signature to bar {}", to + 1),
		commands: vec![
			Command::RemoveTimeSignature { bar: from },
			Command::SetTimeSignature {
				time_signature: change.signature(),
				bar: to,
			},
		],
	})
}

/// Removes the change at bar `bar` (bar 1 cannot be removed).
pub fn remove_change(project: &Project, bar: u32) -> Option<Command> {
	(bar > 0 && project.time_signature_changes.iter().any(|c| c.bar == bar))
		.then_some(Command::RemoveTimeSignature { bar })
}

/// Common signatures offered by the lane's menu.
pub const PRESETS: [(u32, u32); 9] = [
	(4, 4),
	(3, 4),
	(2, 4),
	(5, 4),
	(6, 8),
	(7, 8),
	(9, 8),
	(12, 8),
	(2, 2),
];

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{MeterChange, PPQ};
	use cadenza_model::Document;

	fn ts(numerator: u32, denominator: u32) -> TimeSignature {
		TimeSignature {
			numerator,
			denominator,
		}
	}

	#[test]
	fn edits_follow_the_segment_and_bars() {
		let mut doc = Document::new(Project::new("m"));
		// Bar 3 (index 2) at beat 8.
		let cmd = add_change(doc.project(), Tick(PPQ * 9), ts(3, 4)).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(
			doc.project().time_signature_changes,
			vec![MeterChange::new(2, ts(3, 4))]
		);
		assert!(add_change(doc.project(), Tick(PPQ * 12), ts(3, 4)).is_none());
		assert_eq!(governing_bar(doc.project(), Tick(PPQ * 20)), 2);
		assert_eq!(governing_bar(doc.project(), Tick(PPQ * 2)), 0);
		// The transport field edits the change under the playhead.
		let cmd = set_at(doc.project(), Tick(PPQ * 20), ts(5, 4)).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(
			doc.project().time_signature_changes[0].signature(),
			ts(5, 4)
		);
		assert_eq!(doc.project().time_signature, ts(4, 4));
		let cmd = move_change(doc.project(), 2, 4).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(doc.project().time_signature_changes[0].bar, 4);
		doc.undo().unwrap();
		assert_eq!(doc.project().time_signature_changes[0].bar, 2);
		assert!(move_change(doc.project(), 2, 0).is_none());
		assert!(remove_change(doc.project(), 0).is_none());
		let cmd = remove_change(doc.project(), 2).unwrap();
		doc.execute(cmd).unwrap();
		assert!(doc.project().time_signature_changes.is_empty());
	}
}
