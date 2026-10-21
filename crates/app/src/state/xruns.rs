//! Dropout counters for the transport's xrun indicator: buffer under/overruns the audio
//! device reported and late or busy blocks of sandboxed plugins (`Player::device_xruns`,
//! `Player::plugin_xruns`, polled by the player thread). Counted from the last reset.

use crate::i18n::{trn, trn_with};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use cadenza_core::DeviceId;
use cadenza_engine::PluginXruns;

/// The indicator glows this long after a new dropout.
pub const RECENT: Duration = Duration::from_secs(3);

#[derive(Debug, Default)]
pub struct Xruns {
	/// Device xruns since the player opened, and the count at the last reset.
	device: u64,
	device_base: u64,
	/// Plugins with late blocks in the loaded graph (stats restart with each load).
	plugins: Vec<PluginXruns>,
	/// Late blocks per device at the last reset.
	plugin_base: BTreeMap<DeviceId, u64>,
	/// When the count last grew.
	pub last: Option<Instant>,
}

impl Xruns {
	/// New totals from the player thread.
	pub fn update(&mut self, device: u64, plugins: Vec<PluginXruns>, now: Instant) {
		let before = self.count();
		if device < self.device {
			// The device was reopened: its counter restarted.
			self.device_base = 0;
		}
		self.device = device;
		for p in &plugins {
			if let Some(base) = self.plugin_base.get_mut(&p.device_id)
				&& p.stats.late_blocks < *base
			{
				// Reloaded: the plugin's stats restarted.
				*base = 0;
			}
		}
		self.plugins = plugins;
		if self.count() > before {
			self.last = Some(now);
		}
	}

	fn device_count(&self) -> u64 {
		self.device.saturating_sub(self.device_base)
	}

	fn plugin_count(&self, p: &PluginXruns) -> u64 {
		let base = self.plugin_base.get(&p.device_id).copied().unwrap_or(0);
		p.stats.late_blocks.saturating_sub(base)
	}

	/// Dropouts since the last reset.
	pub fn count(&self) -> u64 {
		self.device_count()
			+ self
				.plugins
				.iter()
				.map(|p| self.plugin_count(p))
				.sum::<u64>()
	}

	/// A dropout happened within [`RECENT`].
	pub fn recent(&self, now: Instant) -> bool {
		self.last
			.is_some_and(|t| now.saturating_duration_since(t) < RECENT)
	}

	/// Starts counting from zero (the indicator hides until the next dropout).
	pub fn reset(&mut self) {
		self.device_base = self.device;
		self.plugin_base = self
			.plugins
			.iter()
			.map(|p| (p.device_id, p.stats.late_blocks))
			.collect();
		self.last = None;
	}

	/// Tooltip lines: what dropped out and how often.
	pub fn lines(&self) -> Vec<String> {
		let mut out = Vec::new();
		let device = self.device_count();
		if device > 0 {
			out.push(trn(
				"Audio device: {n} buffer under/overrun (try a larger buffer in Audio settings)",
				"Audio device: {n} buffer under/overruns (try a larger buffer in Audio settings)",
				device,
			));
		}
		for p in &self.plugins {
			let n = self.plugin_count(p);
			if n > 0 {
				out.push(trn_with(
					"{device} on {track}: {n} late block (longest stall {ms} ms)",
					"{device} on {track}: {n} late blocks (longest stall {ms} ms)",
					n,
					&[
						("device", &p.device_name),
						("track", &p.location),
						("ms", &p.stats.longest_stall_ms),
					],
				));
			}
		}
		out
	}
}

#[cfg(test)]
mod tests {
	use cadenza_plugin_host::SandboxStats;

	use super::*;

	fn plugin(id: DeviceId, late: u64) -> PluginXruns {
		PluginXruns {
			track_id: None,
			location: "track 'Lead'".into(),
			device_id: id,
			device_name: "Surge XT".into(),
			plugin_id: "surge".into(),
			stats: SandboxStats {
				late_blocks: late,
				dropped_frames: late * 128,
				longest_stall_ms: 4,
			},
		}
	}

	#[test]
	fn counts_from_the_last_reset_and_survives_reloads() {
		let t0 = Instant::now();
		let lead = DeviceId::new();
		let mut x = Xruns::default();
		x.update(0, Vec::new(), t0);
		assert_eq!(x.count(), 0);
		assert!(!x.recent(t0));
		x.update(2, vec![plugin(lead, 3)], t0);
		assert_eq!(x.count(), 5);
		assert!(x.recent(t0) && !x.recent(t0 + RECENT));
		assert_eq!(x.lines().len(), 2);
		assert!(x.lines()[1].starts_with("Surge XT on track 'Lead': 3 late blocks"));
		x.reset();
		assert_eq!(x.count(), 0);
		assert!(x.lines().is_empty());
		x.update(3, vec![plugin(lead, 3)], t0);
		assert_eq!(x.count(), 1);
		// A reload restarts the plugin's stats: new late blocks still count.
		x.update(3, vec![plugin(lead, 2)], t0);
		assert_eq!(x.count(), 3);
	}
}
