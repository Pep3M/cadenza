//! Project -> [`Graph`] compilation. Runs off the audio thread: it allocates, loads audio
//! files and instantiates devices. Problems that only affect part of the project (unknown
//! devices or params, unsupported plugin formats, missing files, invalid routing) become
//! warnings instead of errors so a render always produces something.
//!
//! Third-party plugins (`DeviceKind::Clap`/`Vst3`) are resolved through an optional
//! [`PluginCatalog`]. A plugin that cannot be found or loaded is bypassed with a warning:
//! an instrument slot stays silent and an effect slot passes audio through. Plugins run in
//! sandbox processes unless [`PluginHosting`] says otherwise (see [`crate::hosting`]); a
//! missing sandbox executable falls back to in-process hosting with a warning.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use cadenza_core::{DeviceId, ParamTarget, TrackId};
use cadenza_dsp::{DeviceRole, Processor};
use cadenza_model::{ClipContent, DeviceKind, Output, Project, Track, TrackKind};
use cadenza_plugin_host::{
	HostingMode, PluginCatalog, PluginError, PluginFormat, ProcessMode, SandboxMonitor,
};

use crate::audio_clip::{AudioData, ClipPlayback, load_wav};
use crate::automation::{AutoTarget, Binding, Curve, DeviceSlot};
use crate::error::EngineError;
use crate::graph::{Device, Edge, Fader, Graph, Master, Node, Source, StereoBuf, StereoDelay};
use crate::hosting::{PluginHosting, PluginWatch, sandbox_unavailable};
use crate::pdc::{PdcEdge, PdcNode, plan};
use crate::schedule::MidiSchedule;
use crate::{MAX_TAIL_SECONDS, QUANTUM};

/// Compiles `project` for `sample_rate` with built-in devices only; plugin devices are
/// bypassed with a warning. See [`compile_with`].
pub fn compile(
	project: &Project,
	sample_rate: u32,
	max_block: usize,
) -> Result<Graph, EngineError> {
	compile_with(project, sample_rate, max_block, None)
}

/// Compiles `project` for `sample_rate`, instantiating plugin devices from `plugins` with
/// the default [`PluginHosting`] (sandboxed unless `CADENZA_PLUGIN_HOSTING=in-process`).
///
/// `max_block` is the largest block the host will pass to [`Graph::process`]; it is only
/// validated, since the graph accepts any block length and processes devices in fixed
/// [`QUANTUM`]-sample steps.
pub fn compile_with(
	project: &Project,
	sample_rate: u32,
	max_block: usize,
	plugins: Option<&PluginCatalog>,
) -> Result<Graph, EngineError> {
	compile_hosted(
		project,
		sample_rate,
		max_block,
		plugins,
		&PluginHosting::default(),
	)
}

/// [`compile_with`] with an explicit [`PluginHosting`], for realtime playback.
pub fn compile_hosted(
	project: &Project,
	sample_rate: u32,
	max_block: usize,
	plugins: Option<&PluginCatalog>,
	hosting: &PluginHosting,
) -> Result<Graph, EngineError> {
	compile_for(
		project,
		sample_rate,
		max_block,
		plugins,
		hosting,
		ProcessMode::Realtime,
	)
}

/// [`compile_hosted`] for a given plugin processing mode (offline renders let plugins use
/// their slower, higher-quality paths).
pub(crate) fn compile_for(
	project: &Project,
	sample_rate: u32,
	max_block: usize,
	plugins: Option<&PluginCatalog>,
	hosting: &PluginHosting,
	mode: ProcessMode,
) -> Result<Graph, EngineError> {
	if !(8_000..=384_000).contains(&sample_rate) {
		return Err(EngineError::InvalidOptions(format!(
			"sample_rate {sample_rate} is out of range; use 8000..=384000 (e.g. 48000)"
		)));
	}
	if max_block == 0 {
		return Err(EngineError::InvalidOptions(
			"block size must be at least 1 sample (e.g. 512)".into(),
		));
	}
	Compiler::new(project, sample_rate, plugins, hosting, mode).run()
}

/// Where a track's output or send goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dest {
	Master,
	Bus(TrackId),
}

struct Compiler<'a> {
	project: &'a Project,
	plugins: Option<&'a PluginCatalog>,
	hosting: &'a PluginHosting,
	/// Whether the sandbox can be used; resolved (and warned about) on first plugin load.
	sandbox_ok: Option<bool>,
	/// Sandboxed devices compiled so far, for crash reports.
	watches: Vec<PluginWatch>,
	/// Track whose devices are being compiled (`None` = master).
	current_track: Option<TrackId>,
	mode: ProcessMode,
	sample_rate: u32,
	warnings: Vec<String>,
	files: HashMap<PathBuf, Option<Arc<AudioData>>>,
	tail: f32,
}

impl<'a> Compiler<'a> {
	fn new(
		project: &'a Project,
		sample_rate: u32,
		plugins: Option<&'a PluginCatalog>,
		hosting: &'a PluginHosting,
		mode: ProcessMode,
	) -> Self {
		Self {
			project,
			plugins,
			hosting,
			sandbox_ok: None,
			watches: Vec::new(),
			current_track: None,
			mode,
			sample_rate,
			warnings: Vec::new(),
			files: HashMap::new(),
			tail: 0.0,
		}
	}

	fn is_bus(&self, id: TrackId) -> bool {
		self.project
			.track(id)
			.is_some_and(|t| t.kind == TrackKind::Bus)
	}

	fn output_of(&mut self, track: &Track) -> Dest {
		match track.channel.output {
			Output::Master => Dest::Master,
			Output::Bus(id) if id != track.id && self.is_bus(id) => Dest::Bus(id),
			Output::Bus(id) => {
				self.warnings.push(format!(
					"track '{}' outputs to {id}, which is not another bus track; routed to master",
					track.name
				));
				Dest::Master
			}
		}
	}

	/// Buses in an order where every bus comes after all buses feeding it. Buses caught in
	/// a feedback loop are appended in display order; edges that would feed backwards are
	/// dropped later with a warning.
	fn bus_order(&self, routes: &HashMap<TrackId, Vec<Dest>>) -> Vec<TrackId> {
		let buses: Vec<TrackId> = self
			.project
			.tracks
			.iter()
			.filter(|t| t.kind == TrackKind::Bus)
			.map(|t| t.id)
			.collect();
		let mut indegree: HashMap<TrackId, usize> = buses.iter().map(|&b| (b, 0)).collect();
		for &b in &buses {
			for d in &routes[&b] {
				if let Dest::Bus(t) = d {
					*indegree.get_mut(t).expect("bus") += 1;
				}
			}
		}
		let mut order = Vec::with_capacity(buses.len());
		let mut done: Vec<bool> = vec![false; buses.len()];
		while let Some(i) = (0..buses.len()).find(|&i| !done[i] && indegree[&buses[i]] == 0) {
			done[i] = true;
			order.push(buses[i]);
			for d in &routes[&buses[i]] {
				if let Dest::Bus(t) = d {
					*indegree.get_mut(t).expect("bus") -= 1;
				}
			}
		}
		order.extend((0..buses.len()).filter(|&i| !done[i]).map(|i| buses[i]));
		order
	}

	/// Instantiates, prepares and configures one device. Bypassed devices are left out of
	/// the graph entirely (no processing, no latency).
	fn instantiate(
		&mut self,
		device: &cadenza_model::Device,
		role: DeviceRole,
		owner: &str,
	) -> Option<Box<dyn Processor>> {
		if device.bypass {
			return None;
		}
		let (mut processor, id) = match &device.kind {
			DeviceKind::Builtin { id } => {
				let Some(processor) = cadenza_dsp::create(id) else {
					self.warnings.push(format!(
						"{owner}: unknown built-in device '{id}' ('{}'); skipped",
						device.name
					));
					return None;
				};
				(processor, id.as_str())
			}
			DeviceKind::Clap { plugin_id, path } => (
				self.plugin(device, PluginFormat::Clap, plugin_id, path, owner)?,
				plugin_id.as_str(),
			),
			DeviceKind::Vst3 { class_id, path } => (
				self.plugin(device, PluginFormat::Vst3, class_id, path, owner)?,
				class_id.as_str(),
			),
		};
		let info = processor.info();
		if info.role != role {
			self.warnings.push(format!(
				"{owner}: '{id}' is {} and cannot be used as {}; skipped",
				role_name(info.role),
				role_name(role)
			));
			return None;
		}
		processor.prepare(self.sample_rate as f32, QUANTUM);
		for (param, &value) in &device.params {
			match info.param_index(param) {
				Some(i) => processor.set_param(i, value),
				None => self.warnings.push(format!(
					"{owner}: device '{}' ({id}) has no parameter '{param}'; ignored",
					device.name
				)),
			}
		}
		// Snap internal smoothers to the configured values.
		processor.reset();
		self.tail = self.tail.max(processor.tail_seconds());
		Some(processor)
	}

	/// Loads a third-party plugin from the catalog, restoring `device.state`. Explicit
	/// `device.params` are applied by the caller; untouched parameters keep the plugin's
	/// own values (pushing every default would override a restored state or preset).
	fn plugin(
		&mut self,
		device: &cadenza_model::Device,
		format: PluginFormat,
		plugin_id: &str,
		path: &std::path::Path,
		owner: &str,
	) -> Option<Box<dyn Processor>> {
		let label = format!(
			"{owner}: {} plugin '{plugin_id}' ('{}')",
			format.as_str(),
			device.name
		);
		let Some(catalog) = self.plugins else {
			self.warnings.push(format!(
				"{label} cannot be loaded because no plugin catalog was provided; bypassed"
			));
			return None;
		};
		let Some(desc) = catalog
			.find_at(plugin_id, path)
			.filter(|d| d.format == format)
			.or_else(|| {
				catalog
					.descriptors()
					.find(|d| d.id == plugin_id && d.format == format)
			})
		else {
			self.warnings.push(format!(
				"{label} is not installed or not scanned (expected at '{}'); run scan_plugins; bypassed",
				path.display()
			));
			return None;
		};
		let state = match device.state.as_deref() {
			None => None,
			Some(encoded) => match cadenza_plugin_host::decode_state(plugin_id, encoded) {
				Ok(bytes) => Some(bytes),
				Err(e) => {
					self.warnings.push(format!("{label}: {e}"));
					None
				}
			},
		};
		let sr = self.sample_rate as f32;
		let mode = self.mode;
		let sandboxed = self.sandbox_ok();
		let hosting = self.hosting;
		let options = &hosting.sandbox;
		let create = |state: Option<&[u8]>| -> Result<_, PluginError> {
			if sandboxed {
				let p = cadenza_plugin_host::instantiate_sandboxed(
					desc,
					sr,
					QUANTUM,
					state,
					mode,
					options.clone(),
				)?;
				let monitor = p.monitor();
				Ok((Box::new(p) as Box<dyn Processor>, Some(monitor)))
			} else {
				cadenza_plugin_host::instantiate_with_mode(desc, sr, QUANTUM, state, mode)
					.map(|p| (p, None))
			}
		};
		let result = match create(state.as_deref()) {
			Err(e @ PluginError::State { .. }) if state.is_some() => {
				self.warnings.push(format!("{label}: {e}"));
				create(None)
			}
			other => other,
		};
		match result {
			Ok((processor, monitor)) => {
				if let Some(monitor) = monitor {
					self.watch(monitor, device, owner, &label, desc.role);
				}
				Some(processor)
			}
			Err(e) => {
				self.warnings
					.push(format!("{label} failed to load: {e}; bypassed"));
				None
			}
		}
	}

	/// Whether plugins are sandboxed in this compile; warns once when the sandbox was
	/// requested but cannot be used (plugins then run in-process).
	fn sandbox_ok(&mut self) -> bool {
		if self.hosting.mode == HostingMode::InProcess {
			return false;
		}
		*self
			.sandbox_ok
			.get_or_insert_with(|| match sandbox_unavailable(&self.hosting.sandbox) {
				Some(why) => {
					self.warnings.push(why);
					false
				}
				None => true,
			})
	}

	fn watch(
		&mut self,
		monitor: SandboxMonitor,
		device: &cadenza_model::Device,
		owner: &str,
		label: &str,
		role: cadenza_plugin_host::PluginRole,
	) {
		// `label` starts with the owner; the watch keeps them apart.
		let label = label
			.strip_prefix(owner)
			.and_then(|l| l.strip_prefix(": "))
			.unwrap_or(label);
		self.watches.push(PluginWatch {
			monitor,
			track_id: self.current_track,
			location: owner.to_string(),
			device_id: device.id,
			device_name: device.name.clone(),
			label: label.to_string(),
			instrument: role == cadenza_plugin_host::PluginRole::Instrument,
			crashed_at: None,
		});
	}

	fn inserts(&mut self, devices: &[cadenza_model::Device], owner: &str) -> Vec<Device> {
		devices
			.iter()
			.filter_map(|d| {
				self.instantiate(d, DeviceRole::Effect, owner)
					.map(|processor| Device {
						id: d.id,
						processor,
					})
			})
			.collect()
	}

	fn audio(&mut self, path: &PathBuf, owner: &str) -> Option<Arc<AudioData>> {
		if let Some(cached) = self.files.get(path) {
			return cached.clone();
		}
		let loaded = match load_wav(path, self.sample_rate) {
			Ok(data) => Some(Arc::new(data)),
			Err(e) => {
				self.warnings.push(format!("{owner}: {e}; clip is silent"));
				None
			}
		};
		self.files.insert(path.clone(), loaded.clone());
		loaded
	}

	fn source(&mut self, track: &Track, owner: &str) -> (Source, bool) {
		let sr = self.sample_rate as f64;
		match track.kind {
			TrackKind::Instrument => {
				let device = track.instrument.as_ref().and_then(|d| {
					self.instantiate(d, DeviceRole::Instrument, owner)
						.map(|processor| Device {
							id: d.id,
							processor,
						})
				});
				if track.instrument.is_none() && track.clips.iter().any(|c| !c.notes().is_empty()) {
					self.warnings.push(format!(
						"{owner} has MIDI notes but no instrument; it is silent"
					));
				}
				let notes_off_on_reset = track
					.instrument
					.as_ref()
					.is_none_or(|d| matches!(d.kind, DeviceKind::Builtin { .. }));
				let schedule = MidiSchedule::from_clips(&track.clips, &self.project.tempo, sr);
				// +1 for a possible `AllNotesOff` on reset, plus room for every note that
				// could be chased back in at that same instant.
				let events = Vec::with_capacity(
					schedule.max_events_per_quantum() + schedule.max_chase_notes() + 1,
				);
				(
					Source::Instrument {
						device,
						schedule,
						events,
						notes_off_on_reset,
					},
					false,
				)
			}
			TrackKind::Audio => {
				let mut clips = Vec::new();
				for clip in &track.clips {
					let ClipContent::Audio {
						file,
						offset_seconds,
						gain_db,
					} = &clip.content
					else {
						continue;
					};
					let Some(data) = self.audio(file, owner) else {
						continue;
					};
					let tempo = &self.project.tempo;
					clips.push(ClipPlayback {
						data,
						start: tempo.tick_to_sample(clip.start, sr) as i64,
						end: tempo.tick_to_sample(clip.end(), sr) as i64,
						offset: (offset_seconds * sr).round() as i64,
						gain: cadenza_core::db_to_gain(*gain_db),
					});
				}
				let mono = !clips.is_empty() && clips.iter().all(|c| !c.data.is_stereo());
				(Source::Audio { clips }, mono)
			}
			TrackKind::Bus => (Source::Bus { accumulator: 0 }, false),
		}
	}

	fn run(mut self) -> Result<Graph, EngineError> {
		let project = self.project;
		let sr = self.sample_rate;

		// Routing: output first, then sends, per track.
		let mut routes: HashMap<TrackId, Vec<Dest>> = HashMap::new();
		let mut send_specs: HashMap<TrackId, Vec<Option<(f32, bool)>>> = HashMap::new();
		for track in &project.tracks {
			let mut dests = vec![self.output_of(track)];
			let mut specs = vec![None];
			for send in &track.channel.sends {
				if send.target == track.id || !self.is_bus(send.target) {
					self.warnings.push(format!(
						"track '{}' has a send to {}, which is not another bus track; send ignored",
						track.name, send.target
					));
					continue;
				}
				dests.push(Dest::Bus(send.target));
				specs.push(Some((send.level_db, send.pre_fader)));
			}
			routes.insert(track.id, dests);
			send_specs.insert(track.id, specs);
		}

		let buses = self.bus_order(&routes);
		let bus_acc: HashMap<TrackId, usize> =
			buses.iter().enumerate().map(|(i, &b)| (b, i)).collect();
		let master_acc = buses.len();

		let mut order: Vec<usize> = project
			.tracks
			.iter()
			.enumerate()
			.filter(|(_, t)| t.kind != TrackKind::Bus)
			.map(|(i, _)| i)
			.collect();
		order.extend(
			buses
				.iter()
				.map(|b| project.track_index(*b).expect("bus exists")),
		);
		let position: HashMap<TrackId, usize> = order
			.iter()
			.enumerate()
			.map(|(pos, &i)| (project.tracks[i].id, pos))
			.collect();

		// Solo-in-place: a source track plays when nothing is soloed, when it is soloed, or
		// when a bus on its output path is soloed. Buses and master are never solo-muted.
		let any_solo = project.tracks.iter().any(|t| t.channel.solo);
		let solo_path = |track: &Track| {
			let mut current = Some(track);
			let mut hops = 0;
			while let Some(t) = current {
				if t.channel.solo {
					return true;
				}
				hops += 1;
				current = match t.channel.output {
					Output::Bus(id) if hops <= project.tracks.len() => project.track(id),
					_ => None,
				};
			}
			false
		};

		let mut nodes = Vec::with_capacity(order.len());
		let mut pdc_nodes = Vec::with_capacity(order.len());
		let mut pdc_edges = Vec::new();
		for (pos, &ti) in order.iter().enumerate() {
			let track = &project.tracks[ti];
			let owner = format!("track '{}'", track.name);
			self.current_track = Some(track.id);
			let (mut source, mono) = self.source(track, &owner);
			if let Source::Bus { accumulator } = &mut source {
				*accumulator = bus_acc[&track.id];
			}
			let inserts = self.inserts(&track.channel.inserts, &owner);
			let instrument_latency = match &source {
				Source::Instrument {
					device: Some(d), ..
				} => d.processor.latency(),
				_ => 0,
			};
			let chain_latency =
				instrument_latency + inserts.iter().map(|d| d.processor.latency()).sum::<u32>();
			pdc_nodes.push(PdcNode {
				chain_latency,
				input: match &source {
					Source::Bus { accumulator } => Some(*accumulator),
					_ => None,
				},
			});

			let mut edges = Vec::new();
			let dests = routes[&track.id].clone();
			let specs = send_specs[&track.id].clone();
			for (dest, spec) in dests.into_iter().zip(specs) {
				let accumulator = match dest {
					Dest::Master => master_acc,
					Dest::Bus(b) if position[&b] > pos => bus_acc[&b],
					Dest::Bus(b) => {
						let target = project.track(b).map_or("?", |t| t.name.as_str());
						self.warnings.push(format!(
							"{owner} -> bus '{target}' would create a feedback loop; connection dropped"
						));
						if spec.is_none() {
							master_acc
						} else {
							continue;
						}
					}
				};
				let (gain, pre_fader) = spec.map_or((1.0, false), |(db, pre)| {
					(cadenza_core::db_to_gain(db), pre)
				});
				pdc_edges.push(PdcEdge {
					from: pos,
					to_accumulator: accumulator,
				});
				edges.push(Edge {
					accumulator,
					gain,
					pre_fader,
					delay: StereoDelay::new(0),
				});
			}

			let audible = !track.channel.mute
				&& (track.kind == TrackKind::Bus || !any_solo || solo_path(track));
			nodes.push(Node {
				track_id: track.id,
				name: track.name.clone(),
				display_index: ti,
				source,
				inserts,
				fader: Fader::new(track.channel.volume_db, track.channel.pan, mono, sr as f32),
				audible,
				edges,
				buf: StereoBuf::new(),
				out_latency: 0,
			});
		}

		self.current_track = None;
		let master_inserts = self.inserts(&project.master.inserts, "master");
		let master_chain: u32 = master_inserts.iter().map(|d| d.processor.latency()).sum();
		let pdc = plan(&pdc_nodes, &pdc_edges, master_acc + 1);
		let mut edge_index = 0;
		for (i, node) in nodes.iter_mut().enumerate() {
			node.out_latency = pdc.node_out_latency[i];
			for edge in &mut node.edges {
				edge.delay = StereoDelay::new(pdc.edge_delay[edge_index]);
				edge_index += 1;
			}
		}
		let latency = pdc.accumulator_latency[master_acc] + master_chain;

		let master = Master {
			inserts: master_inserts,
			fader: Fader::new(
				project.master.volume_db,
				project.master.pan,
				false,
				sr as f32,
			),
			buf: StereoBuf::new(),
		};
		let automation = self.bind_automation(&nodes, &master);

		Ok(Graph {
			sample_rate: sr,
			accumulators: (0..=master_acc).map(|_| StereoBuf::new()).collect(),
			nodes,
			master,
			automation,
			tempo: crate::graph::tempo_segments(&project.tempo, sr as f64),
			latency,
			tail_seconds: self.tail.min(MAX_TAIL_SECONDS),
			warnings: self.warnings,
			plugin_watches: self.watches,
			engine_pos: 0,
			discard: latency,
			out_pos: 0,
			fifo_pos: QUANTUM,
			playing: false,
			needs_reset: true,
		})
	}

	fn bind_automation(&mut self, nodes: &[Node], master: &Master) -> Vec<Binding> {
		let project = self.project;
		let sr = self.sample_rate as f64;
		let mut bindings = Vec::new();
		for lane in &project.automation {
			let Some(curve) = Curve::from_lane(lane, &project.tempo, sr) else {
				continue;
			};
			let address = &lane.address;
			let node = match address.track {
				None => None,
				Some(id) => match nodes.iter().position(|n| n.track_id == id) {
					Some(i) => Some(i),
					None => {
						self.warnings.push(format!(
							"automation lane '{address}' targets a track that does not exist; ignored"
						));
						continue;
					}
				},
			};
			let target = match &address.target {
				ParamTarget::Volume => AutoTarget::Volume { node },
				ParamTarget::Pan => AutoTarget::Pan { node },
				ParamTarget::Device { device, param } => {
					let Some((slot, processor)) = find_device(nodes, master, node, *device) else {
						// Present in the project but skipped (bypassed/unsupported) is fine.
						if project.device(address.track, *device).is_none() {
							self.warnings.push(format!(
								"automation lane '{address}' targets a device that does not exist; ignored"
							));
						}
						continue;
					};
					let Some(index) = processor.info().param_index(param) else {
						self.warnings.push(format!(
							"automation lane '{address}': device '{}' has no parameter '{param}'; ignored",
							processor.info().id
						));
						continue;
					};
					AutoTarget::Param { node, slot, index }
				}
			};
			bindings.push(Binding {
				curve,
				target,
				last: f32::NAN,
			});
		}
		bindings
	}
}

fn find_device<'g>(
	nodes: &'g [Node],
	master: &'g Master,
	node: Option<usize>,
	id: DeviceId,
) -> Option<(DeviceSlot, &'g dyn Processor)> {
	let inserts = match node {
		Some(i) => {
			if let Source::Instrument {
				device: Some(d), ..
			} = &nodes[i].source
				&& d.id == id
			{
				return Some((DeviceSlot::Instrument, d.processor.as_ref()));
			}
			&nodes[i].inserts
		}
		None => &master.inserts,
	};
	inserts
		.iter()
		.position(|d| d.id == id)
		.map(|k| (DeviceSlot::Insert(k), inserts[k].processor.as_ref()))
}

fn role_name(role: DeviceRole) -> &'static str {
	match role {
		DeviceRole::Instrument => "an instrument",
		DeviceRole::Effect => "an effect",
	}
}
