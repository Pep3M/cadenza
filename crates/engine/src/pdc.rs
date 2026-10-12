//! Plugin delay compensation planning.
//!
//! Every node (track or bus) runs a chain of device stages, each with a latency. A bus
//! reads a summing accumulator; its chain starts at the accumulator's latency. At every
//! accumulator (each bus input and the master input) the latest arriving connection
//! defines the accumulator latency and every other connection is delayed by the
//! difference, so all paths reach the master time-aligned. The master's total latency is
//! then removed by running the graph that many samples ahead of the output and discarding
//! the first `latency` output samples after every start/seek.
//!
//! Sidechains: a keyed stage receives the key from an earlier node's output. Whichever of
//! the two signals is earlier is delayed to meet the other at the stage (delaying the main
//! signal raises the latency of the rest of the chain).

/// One device in a node's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PdcStage {
	pub latency: u32,
	/// Node whose output keys this stage (must come earlier in processing order).
	pub key: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdcNode {
	/// Accumulator this node reads its input from (buses only).
	pub input: Option<usize>,
	pub stages: Vec<PdcStage>,
}

/// Where a connection leaves its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tap {
	/// The node's output (after its whole chain).
	Out,
	/// Right after the first stage (an instrument's extra outputs).
	FirstStage,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PdcEdge {
	pub from: usize,
	pub to_accumulator: usize,
	pub tap: Tap,
}

/// Compensation delays for one keyed stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StageDelay {
	/// Delay applied to the chain signal before the stage.
	pub main: u32,
	/// Delay applied to the key signal.
	pub key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdcPlan {
	pub node_out_latency: Vec<u32>,
	pub accumulator_latency: Vec<u32>,
	pub edge_delay: Vec<u32>,
	/// Per node, per stage.
	pub stage_delays: Vec<Vec<StageDelay>>,
	pub master_delays: Vec<StageDelay>,
	/// Latency at the master output.
	pub latency: u32,
}

/// Runs `stages` starting at latency `lat`; returns the delays and the end latency.
fn run_chain(stages: &[PdcStage], mut lat: u32, out: &[u32]) -> (Vec<StageDelay>, Vec<u32>) {
	let mut delays = Vec::with_capacity(stages.len());
	let mut after = Vec::with_capacity(stages.len());
	for stage in stages {
		let mut d = StageDelay::default();
		if let Some(&key) = stage.key.and_then(|k| out.get(k)) {
			if key > lat {
				d.main = key - lat;
				lat = key;
			} else {
				d.key = lat - key;
			}
		}
		lat += stage.latency;
		delays.push(d);
		after.push(lat);
	}
	(delays, after)
}

/// `nodes` must be in processing order: every edge goes from a node to an accumulator that
/// is only read by later nodes (or by the master, accumulator `accumulators - 1`), and every
/// key comes from an earlier node (keys from later nodes are ignored here; the compiler
/// drops them).
pub(crate) fn plan(
	nodes: &[PdcNode],
	edges: &[PdcEdge],
	accumulators: usize,
	master: &[PdcStage],
) -> PdcPlan {
	let mut node_out = Vec::with_capacity(nodes.len());
	let mut first_stage = Vec::with_capacity(nodes.len());
	let mut stage_delays = Vec::with_capacity(nodes.len());
	let mut acc = vec![0u32; accumulators];
	for (i, node) in nodes.iter().enumerate() {
		let input = node.input.map_or(0, |a| acc[a]);
		let (delays, after) = run_chain(&node.stages, input, &node_out);
		node_out.push(after.last().copied().unwrap_or(input));
		first_stage.push(after.first().copied().unwrap_or(input));
		stage_delays.push(delays);
		for e in edges.iter().filter(|e| e.from == i) {
			let tap = match e.tap {
				Tap::Out => node_out[i],
				Tap::FirstStage => first_stage[i],
			};
			acc[e.to_accumulator] = acc[e.to_accumulator].max(tap);
		}
	}
	let edge_delay = edges
		.iter()
		.map(|e| {
			let tap = match e.tap {
				Tap::Out => node_out[e.from],
				Tap::FirstStage => first_stage[e.from],
			};
			acc[e.to_accumulator] - tap
		})
		.collect();
	let master_in = acc.last().copied().unwrap_or(0);
	let (master_delays, after) = run_chain(master, master_in, &node_out);
	PdcPlan {
		latency: after.last().copied().unwrap_or(master_in),
		node_out_latency: node_out,
		accumulator_latency: acc,
		edge_delay,
		stage_delays,
		master_delays,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn chain(latencies: &[u32]) -> Vec<PdcStage> {
		latencies
			.iter()
			.map(|&latency| PdcStage { latency, key: None })
			.collect()
	}

	fn edge(from: usize, to_accumulator: usize) -> PdcEdge {
		PdcEdge {
			from,
			to_accumulator,
			tap: Tap::Out,
		}
	}

	#[test]
	fn aligns_paths_through_buses() {
		// 0: dry track -> master(acc 1); 1: track with 100 latency -> bus (acc 0) and send to
		// master; 2: bus reading acc 0 with 20 latency -> master.
		let nodes = [
			PdcNode {
				input: None,
				stages: chain(&[]),
			},
			PdcNode {
				input: None,
				stages: chain(&[60, 40]),
			},
			PdcNode {
				input: Some(0),
				stages: chain(&[20]),
			},
		];
		let edges = [edge(0, 1), edge(1, 0), edge(1, 1), edge(2, 1)];
		let p = plan(&nodes, &edges, 2, &chain(&[5]));
		assert_eq!(p.node_out_latency, vec![0, 100, 120]);
		assert_eq!(p.accumulator_latency, vec![100, 120]);
		assert_eq!(p.edge_delay, vec![120, 0, 20, 0]);
		assert_eq!(p.latency, 125);
	}

	#[test]
	fn keys_meet_the_main_signal() {
		// 0: kick with 64 latency; 1: bass keyed by the kick at its second stage (after a
		// 10-sample device): the main signal waits 54 samples; 2: pad keyed by the kick
		// after a 100-sample device: the key waits 36 samples.
		let nodes = [
			PdcNode {
				input: None,
				stages: chain(&[64]),
			},
			PdcNode {
				input: None,
				stages: vec![
					PdcStage {
						latency: 10,
						key: None,
					},
					PdcStage {
						latency: 0,
						key: Some(0),
					},
				],
			},
			PdcNode {
				input: None,
				stages: vec![
					PdcStage {
						latency: 100,
						key: None,
					},
					PdcStage {
						latency: 0,
						key: Some(0),
					},
				],
			},
		];
		let edges = [edge(0, 0), edge(1, 0), edge(2, 0)];
		let master = [PdcStage {
			latency: 0,
			key: Some(2),
		}];
		let p = plan(&nodes, &edges, 1, &master);
		assert_eq!(p.stage_delays[1][1], StageDelay { main: 54, key: 0 });
		assert_eq!(p.stage_delays[2][1], StageDelay { main: 0, key: 36 });
		assert_eq!(p.node_out_latency, vec![64, 64, 100]);
		assert_eq!(p.edge_delay, vec![36, 36, 0]);
		assert_eq!(p.master_delays[0], StageDelay { main: 0, key: 0 });
		assert_eq!(p.latency, 100);
	}

	#[test]
	fn first_stage_taps_skip_the_rest_of_the_chain() {
		// Instrument (30) then an insert (70); its extra output goes straight to a bus.
		let nodes = [
			PdcNode {
				input: None,
				stages: chain(&[30, 70]),
			},
			PdcNode {
				input: Some(0),
				stages: chain(&[]),
			},
		];
		let edges = [
			edge(0, 1),
			PdcEdge {
				from: 0,
				to_accumulator: 0,
				tap: Tap::FirstStage,
			},
			edge(1, 1),
		];
		let p = plan(&nodes, &edges, 2, &[]);
		assert_eq!(p.accumulator_latency, vec![30, 100]);
		assert_eq!(p.edge_delay, vec![0, 0, 70]);
	}
}
