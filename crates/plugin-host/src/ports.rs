//! Audio port layout shared by the CLAP and VST3 wrappers: which port carries
//! the track signal, which one the sidechain (key) signal, and which extra
//! outputs a multi-output instrument offers. All buffers are flattened per
//! channel in port order.

use crate::descriptor::AuxOutput;

/// Audio port layout of an instance: channel count of every port, plus which
/// port plays which role in each direction.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PortLayout {
	pub inputs: Vec<u32>,
	pub outputs: Vec<u32>,
	pub main_input: Option<usize>,
	pub main_output: Option<usize>,
	/// Input port fed with the sidechain signal.
	pub sidechain_input: Option<usize>,
	/// Port names (may be empty strings), parallel to `outputs`.
	pub output_names: Vec<String>,
}

impl PortLayout {
	pub fn main_input_channels(&self) -> u32 {
		self.main_input.map_or(0, |i| self.inputs[i])
	}

	pub fn main_output_channels(&self) -> u32 {
		self.main_output.map_or(0, |i| self.outputs[i])
	}

	pub fn sidechain_channels(&self) -> u32 {
		self.sidechain_input.map_or(0, |i| self.inputs[i])
	}

	/// Extra (non-main) output ports that carry audio, in port order; entry
	/// `k` is the plugin's extra output `k + 1`.
	pub fn aux_output_ports(&self) -> impl Iterator<Item = usize> + '_ {
		(0..self.outputs.len()).filter(move |&p| Some(p) != self.main_output && self.outputs[p] > 0)
	}

	pub fn aux_outputs(&self) -> Vec<AuxOutput> {
		self.aux_output_ports()
			.map(|p| AuxOutput {
				name: self.output_names.get(p).cloned().unwrap_or_default(),
				channels: self.outputs[p],
			})
			.collect()
	}

	/// Index of the first channel of output port `port` in flattened buffers.
	fn output_offset(&self, port: usize) -> usize {
		self.outputs[..port].iter().map(|&c| c as usize).sum()
	}

	/// The first input port that is not the main one and has channels: CLAP
	/// has no sidechain flag, so this is the convention every host follows.
	pub fn first_aux_input(&self) -> Option<usize> {
		(0..self.inputs.len()).find(|&p| Some(p) != self.main_input && self.inputs[p] > 0)
	}

	/// Fills every input channel for one block of `n` frames: the main port
	/// gets `main` (effects; down-mixed for mono ports), the sidechain port
	/// gets `key` when given, every other channel is silent.
	pub fn fill_inputs(
		&self,
		bufs: &mut [Vec<f32>],
		n: usize,
		main: Option<(&[f32], &[f32])>,
		key: Option<(&[f32], &[f32])>,
	) {
		let mut channel = 0;
		for (port, &count) in self.inputs.iter().enumerate() {
			let source = if Some(port) == self.main_input {
				main
			} else if Some(port) == self.sidechain_input {
				key
			} else {
				None
			};
			for c in 0..count as usize {
				let buf = &mut bufs[channel + c][..n];
				match (source, count, c) {
					(Some((l, r)), 1, 0) => {
						for ((o, l), r) in buf.iter_mut().zip(l).zip(r) {
							*o = 0.5 * (l + r);
						}
					}
					(Some((l, _)), _, 0) => copy_or_silence(buf, l),
					(Some((_, r)), _, 1) => copy_or_silence(buf, r),
					_ => buf.fill(0.0),
				}
			}
			channel += count as usize;
		}
	}

	/// Left/right of output port `port` for the first `n` frames (mono ports
	/// return the same slice twice, ports without channels `None`).
	pub fn output_pair<'a>(
		&self,
		bufs: &'a [Vec<f32>],
		port: usize,
		n: usize,
	) -> Option<(&'a [f32], &'a [f32])> {
		let first = self.output_offset(port);
		match self.outputs.get(port)? {
			0 => None,
			1 => Some((&bufs[first][..n], &bufs[first][..n])),
			_ => Some((&bufs[first][..n], &bufs[first + 1][..n])),
		}
	}

	/// Extra output `index` (0-based over [`Self::aux_output_ports`]).
	pub fn aux_pair<'a>(
		&self,
		bufs: &'a [Vec<f32>],
		index: usize,
		n: usize,
	) -> Option<(&'a [f32], &'a [f32])> {
		let port = self.aux_output_ports().nth(index)?;
		self.output_pair(bufs, port, n)
	}
}

fn copy_or_silence(dst: &mut [f32], src: &[f32]) {
	let n = dst.len().min(src.len());
	dst[..n].copy_from_slice(&src[..n]);
	dst[n..].fill(0.0);
}

/// The sidechain signal handed to a plugin wrapper for its next block,
/// copied into buffers allocated on activation.
#[derive(Debug, Default)]
pub(crate) struct KeyInput {
	l: Vec<f32>,
	r: Vec<f32>,
	len: usize,
	active: bool,
}

impl KeyInput {
	pub fn allocate(&mut self, max_block: usize) {
		self.l = vec![0.0; max_block];
		self.r = vec![0.0; max_block];
		self.len = 0;
		self.active = false;
	}

	/// Realtime-safe copy (truncated to the allocated size; the rest of the
	/// buffers is zeroed).
	pub fn set(&mut self, left: &[f32], right: &[f32]) {
		let n = left.len().min(right.len()).min(self.l.len());
		self.l[..n].copy_from_slice(&left[..n]);
		self.r[..n].copy_from_slice(&right[..n]);
		self.l[n..].fill(0.0);
		self.r[n..].fill(0.0);
		self.len = n;
		self.active = true;
	}

	/// Whether a key was set for this block; consumes it like [`Self::take`].
	pub fn take_active(&mut self) -> bool {
		std::mem::take(&mut self.active)
	}

	/// The whole allocated buffers (zero past the last key length).
	pub fn buffers(&self) -> (&[f32], &[f32]) {
		(&self.l, &self.r)
	}

	/// The key for this block (consumed: the next block has none unless set
	/// again).
	pub fn take(&mut self) -> Option<(&[f32], &[f32])> {
		if !std::mem::take(&mut self.active) {
			return None;
		}
		Some((&self.l[..self.len], &self.r[..self.len]))
	}

	pub fn clear(&mut self) {
		self.active = false;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn layout() -> PortLayout {
		PortLayout {
			inputs: vec![2, 2, 1],
			outputs: vec![2, 1, 0, 2],
			main_input: Some(0),
			main_output: Some(0),
			sidechain_input: Some(1),
			output_names: vec!["Main".into(), "Kick".into(), "".into(), "Snare".into()],
		}
	}

	#[test]
	fn inputs_route_main_and_key_and_silence_the_rest() {
		let l = layout();
		let mut bufs = vec![vec![9.0f32; 4]; 5];
		let (ml, mr) = ([1.0f32; 4], [2.0f32; 4]);
		let (kl, kr) = ([3.0f32; 2], [4.0f32; 2]);
		l.fill_inputs(&mut bufs, 4, Some((&ml, &mr)), Some((&kl, &kr)));
		assert_eq!(bufs[0], [1.0; 4]);
		assert_eq!(bufs[1], [2.0; 4]);
		assert_eq!(bufs[2], [3.0, 3.0, 0.0, 0.0], "short key is padded");
		assert_eq!(bufs[3], [4.0, 4.0, 0.0, 0.0]);
		assert_eq!(bufs[4], [0.0; 4]);
		l.fill_inputs(&mut bufs, 4, None, None);
		assert!(bufs.iter().all(|b| b.iter().all(|&x| x == 0.0)));
	}

	#[test]
	fn aux_outputs_skip_main_and_empty_ports() {
		let l = layout();
		assert_eq!(l.sidechain_channels(), 2);
		assert_eq!(l.first_aux_input(), Some(1));
		let aux = l.aux_outputs();
		assert_eq!(
			aux,
			vec![
				AuxOutput {
					name: "Kick".into(),
					channels: 1
				},
				AuxOutput {
					name: "Snare".into(),
					channels: 2
				}
			]
		);
		let bufs: Vec<Vec<f32>> = (0..5).map(|i| vec![i as f32; 8]).collect();
		let (a, b) = l.aux_pair(&bufs, 0, 3).unwrap();
		assert_eq!((a, b), (&[2.0; 3][..], &[2.0; 3][..]), "mono duplicated");
		let (a, b) = l.aux_pair(&bufs, 1, 3).unwrap();
		assert_eq!((a[0], b[0]), (3.0, 4.0));
		assert!(l.aux_pair(&bufs, 2, 3).is_none());
	}

	#[test]
	fn key_input_is_consumed_per_block() {
		let mut k = KeyInput::default();
		k.allocate(4);
		assert!(k.take().is_none());
		k.set(&[1.0; 8], &[2.0; 8]);
		let (l, r) = k.take().unwrap();
		assert_eq!((l.len(), r[0]), (4, 2.0));
		assert!(k.take().is_none());
	}
}
