//! Plugin delay compensation planning.
//!
//! Every node (track or bus) has a chain latency (sum of its devices' `latency()`). A bus
//! reads a summing accumulator; its output latency is the accumulator's latency plus its own
//! chain. At every accumulator (each bus input and the master input) the latest arriving
//! connection defines the accumulator latency and every other connection is delayed by the
//! difference, so all paths reach the master time-aligned. The master's total latency is
//! then removed by running the graph that many samples ahead of the output and discarding
//! the first `latency` output samples after every start/seek.

#[derive(Debug, Clone, Copy)]
pub(crate) struct PdcNode {
	pub chain_latency: u32,
	/// Accumulator this node reads its input from (buses only).
	pub input: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PdcEdge {
	pub from: usize,
	pub to_accumulator: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdcPlan {
	pub node_out_latency: Vec<u32>,
	pub accumulator_latency: Vec<u32>,
	pub edge_delay: Vec<u32>,
}

/// `nodes` must be in processing order: every edge goes from a node to an accumulator that
/// is only read by later nodes (or by the master, which is not a node here).
pub(crate) fn plan(nodes: &[PdcNode], edges: &[PdcEdge], accumulators: usize) -> PdcPlan {
	let mut node_out = vec![0u32; nodes.len()];
	let mut acc = vec![0u32; accumulators];
	for (i, node) in nodes.iter().enumerate() {
		let input = node.input.map_or(0, |a| acc[a]);
		node_out[i] = input + node.chain_latency;
		for e in edges.iter().filter(|e| e.from == i) {
			acc[e.to_accumulator] = acc[e.to_accumulator].max(node_out[i]);
		}
	}
	let edge_delay = edges
		.iter()
		.map(|e| acc[e.to_accumulator] - node_out[e.from])
		.collect();
	PdcPlan {
		node_out_latency: node_out,
		accumulator_latency: acc,
		edge_delay,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn aligns_paths_through_buses() {
		// 0: dry track -> master(acc 1); 1: track with 100 latency -> bus (acc 0) and send to
		// master; 2: bus reading acc 0 with 20 latency -> master.
		let nodes = [
			PdcNode {
				chain_latency: 0,
				input: None,
			},
			PdcNode {
				chain_latency: 100,
				input: None,
			},
			PdcNode {
				chain_latency: 20,
				input: Some(0),
			},
		];
		let edges = [
			PdcEdge {
				from: 0,
				to_accumulator: 1,
			},
			PdcEdge {
				from: 1,
				to_accumulator: 0,
			},
			PdcEdge {
				from: 1,
				to_accumulator: 1,
			},
			PdcEdge {
				from: 2,
				to_accumulator: 1,
			},
		];
		let p = plan(&nodes, &edges, 2);
		assert_eq!(p.node_out_latency, vec![0, 100, 120]);
		assert_eq!(p.accumulator_latency, vec![100, 120]);
		assert_eq!(p.edge_delay, vec![120, 0, 20, 0]);
	}
}
