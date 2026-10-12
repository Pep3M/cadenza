//! Vertical layout of the arrange view: track rows, the automation lanes expanded under
//! them and the master row with its lanes. Positions are content pixels (before the
//! vertical scroll), so hit-testing and painting share one source of truth.

use cadenza_core::{ParamAddress, TrackId};
use cadenza_model::Project;

use super::ViewState;
use super::automation::LANE_HEIGHT;

/// Height of the master row (it only holds the automation toggle).
pub const MASTER_ROW_HEIGHT: f32 = 26.0;

/// Height of a take lane (expanded takes under a track).
pub const TAKE_LANE_HEIGHT: f32 = 30.0;

/// Number of take lanes of `track` (highest lane + 1; 0 without takes).
pub fn take_lanes(track: &cadenza_model::Track) -> u32 {
	track
		.clips
		.iter()
		.filter_map(|c| c.take.map(|t| t.lane + 1))
		.max()
		.unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
	/// The clip lane of a track (`index` in display order).
	Track {
		id: TrackId,
		index: usize,
	},
	/// Take lane `lane` of a track (its takes stacked, expanded by the header's T).
	Take {
		track: TrackId,
		lane: u32,
	},
	/// An automation lane under its channel (`None` owner = master).
	Lane {
		owner: Option<TrackId>,
		address: ParamAddress,
	},
	Master,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
	pub kind: RowKind,
	pub top: f32,
	pub height: f32,
}

impl Row {
	pub fn bottom(&self) -> f32 {
		self.top + self.height
	}

	pub fn contains(&self, y: f32) -> bool {
		y >= self.top && y < self.bottom()
	}
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rows {
	pub rows: Vec<Row>,
	/// Bottom of the last row.
	pub height: f32,
}

impl Rows {
	pub fn new(project: &Project, view: &ViewState) -> Self {
		let mut rows = Vec::new();
		let mut y = 0.0;
		let auto = &view.automation;
		let mut push = |kind: RowKind, height: f32, rows: &mut Vec<Row>| {
			rows.push(Row {
				kind,
				top: y,
				height,
			});
			y += height;
		};
		for (index, t) in project.tracks.iter().enumerate() {
			push(
				RowKind::Track { id: t.id, index },
				view.track_height,
				&mut rows,
			);
			if view.takes.contains(&t.id) {
				for lane in 0..take_lanes(t) {
					push(
						RowKind::Take { track: t.id, lane },
						TAKE_LANE_HEIGHT,
						&mut rows,
					);
				}
			}
			if auto.is_open(Some(t.id)) {
				for address in auto.visible(project, Some(t.id)) {
					push(
						RowKind::Lane {
							owner: Some(t.id),
							address,
						},
						LANE_HEIGHT,
						&mut rows,
					);
				}
			}
		}
		let master_lanes = !auto.visible(project, None).is_empty();
		if auto.master || master_lanes {
			push(RowKind::Master, MASTER_ROW_HEIGHT, &mut rows);
			if auto.is_open(None) {
				for address in auto.visible(project, None) {
					push(
						RowKind::Lane {
							owner: None,
							address,
						},
						LANE_HEIGHT,
						&mut rows,
					);
				}
			}
		}
		Self { rows, height: y }
	}

	/// The row at content `y`.
	pub fn at(&self, y: f32) -> Option<&Row> {
		let i = self.rows.partition_point(|r| r.bottom() <= y);
		self.rows.get(i).filter(|r| r.contains(y))
	}

	/// The track whose group (clip row and its lanes) contains `y`.
	pub fn track_at(&self, y: f32) -> Option<(TrackId, usize)> {
		let i = self.rows.partition_point(|r| r.bottom() <= y);
		if !self.rows.get(i).is_some_and(|r| r.contains(y)) {
			return None;
		}
		for r in self.rows[..=i].iter().rev() {
			match r.kind {
				RowKind::Track { id, index } => return Some((id, index)),
				RowKind::Master => return None,
				RowKind::Lane { .. } | RowKind::Take { .. } => {}
			}
		}
		None
	}

	/// The clip row of track `index`.
	pub fn track(&self, index: usize) -> Option<&Row> {
		self.rows
			.iter()
			.find(|r| matches!(r.kind, RowKind::Track { index: i, .. } if i == index))
	}

	/// Top of track `index`'s group; `tracks.len()` gives the end of the track groups.
	pub fn track_top(&self, index: usize) -> f32 {
		self.track(index).map_or_else(
			|| {
				self.rows
					.iter()
					.find(|r| matches!(r.kind, RowKind::Master))
					.map_or(self.height, |r| r.top)
			},
			|r| r.top,
		)
	}

	/// The automation lane row for `address`.
	pub fn lane(&self, address: &ParamAddress) -> Option<&Row> {
		self.rows
			.iter()
			.find(|r| matches!(&r.kind, RowKind::Lane { address: a, .. } if a == address))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Track, TrackKind};

	#[test]
	fn lanes_expand_under_their_track() {
		let mut p = Project::new("rows");
		p.tracks = vec![
			Track::new("A", TrackKind::Instrument),
			Track::new("B", TrackKind::Audio),
		];
		let (a, b) = (p.tracks[0].id, p.tracks[1].id);
		let mut view = ViewState {
			track_height: 60.0,
			..ViewState::default()
		};
		let rows = Rows::new(&p, &view);
		assert_eq!(rows.rows.len(), 2);
		assert_eq!(rows.height, 120.0);

		view.automation.toggle(&p, Some(a));
		view.automation.show(ParamAddress::track_pan(a));
		let rows = Rows::new(&p, &view);
		assert_eq!(rows.rows.len(), 4);
		assert_eq!(rows.track_top(1), 60.0 + 2.0 * LANE_HEIGHT);
		assert_eq!(
			rows.track_at(70.0),
			Some((a, 0)),
			"a lane belongs to its track"
		);
		assert_eq!(rows.track_at(60.0 + 2.0 * LANE_HEIGHT + 1.0), Some((b, 1)));
		assert!(matches!(
			rows.at(61.0).unwrap().kind,
			RowKind::Lane { owner: Some(o), .. } if o == a
		));
		assert_eq!(
			rows.lane(&ParamAddress::track_pan(a)).unwrap().top,
			60.0 + LANE_HEIGHT
		);
		assert!(rows.at(-1.0).is_none() && rows.at(1e6).is_none());

		// The master row appears once master automation is shown.
		view.automation.toggle(&p, None);
		let rows = Rows::new(&p, &view);
		let master = rows
			.rows
			.iter()
			.position(|r| r.kind == RowKind::Master)
			.unwrap();
		assert_eq!(master, 4);
		assert_eq!(rows.rows.len(), 6, "master volume lane under it");
		assert_eq!(rows.track_top(2), rows.rows[master].top);
		assert_eq!(rows.track_at(rows.rows[master].top + 1.0), None);
	}
}
