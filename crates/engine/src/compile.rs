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

use cadenza_core::{DeviceId, TempoMap, TrackId};
use cadenza_dsp::{DeviceRole, Processor};
use cadenza_model::{ClipContent, DeviceKind, Output, Project, Track, TrackKind};
use cadenza_plugin_host::{
	HostingMode, PluginCatalog, PluginError, PluginFormat, ProcessMode, SandboxMonitor,
};

use crate::audio_clip::{self, ClipPlayback};
use crate::automation::{DeviceEntry, DeviceSlot, Resolver};
use crate::error::EngineError;
use crate::graph::{
	AuxEdge, Device, Edge, Fader, Graph, Master, Node, Sidechain, Source, StereoBuf, StereoDelay,
};
use crate::hosting::{PluginEditorSlot, PluginHosting, PluginWatch, sandbox_unavailable};
use crate::modulation::Modulation;
use crate::pdc::{PdcEdge, PdcNode, PdcStage, Tap, plan};
use crate::schedule::MidiSchedule;
use crate::{MAX_TAIL_SECONDS, QUANTUM};
use cadenza_audio_io::{AudioBuffer, ClipRegion};

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

/// A send's id, level (dB) and pre-fader flag.
type SendSpec = (DeviceId, f32, bool);

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
	/// Editor handles of the plugin devices compiled so far.
	editors: Vec<PluginEditorSlot>,
	/// Track whose devices are being compiled (`None` = master).
	current_track: Option<TrackId>,
	mode: ProcessMode,
	sample_rate: u32,
	warnings: Vec<String>,
	files: HashMap<PathBuf, Option<Arc<AudioBuffer>>>,
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
			editors: Vec::new(),
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
		if !device.zones.is_empty() || cadenza_dsp::uses_samples(id) {
			let zones = self.sample_zones(device, owner);
			let count = zones.len();
			if !processor.set_zones(zones) && count > 0 {
				self.warnings.push(format!(
					"{owner}: device '{}' ({id}) does not play samples; its {count} sample zones are ignored",
					device.name
				));
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
		let Some(desc) = crate::devices::find_plugin(catalog, format, plugin_id, path) else {
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
		let create = |state: Option<&[u8]>| {
			crate::devices::create_plugin(desc, sr, state, mode, hosting, sandboxed)
		};
		let result = match create(state.as_deref()) {
			Err(e @ PluginError::State { .. }) if state.is_some() => {
				self.warnings.push(format!("{label}: {e}"));
				create(None)
			}
			other => other,
		};
		match result {
			Ok(created) => {
				if let Some(monitor) = created.monitor {
					self.watch(monitor, device, owner, &label, desc.role);
				}
				self.editors.push(PluginEditorSlot {
					device_id: device.id,
					track_id: self.current_track,
					editor: created.editor,
				});
				Some(created.processor)
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

	/// The graph device for `model`'s instantiated `processor`, with its modulators.
	fn device(
		&mut self,
		model: &cadenza_model::Device,
		processor: Box<dyn Processor>,
		instrument: bool,
		owner: &str,
	) -> Device {
		let info = processor.info();
		let warnings = &mut self.warnings;
		let modulation = Modulation::new(
			&model.modulators,
			info,
			&model.params,
			self.sample_rate as f64,
			instrument,
			|param| {
				warnings.push(format!(
					"{owner}: device '{}' has no parameter '{param}' to modulate; that target is ignored",
					model.name
				))
			},
		)
		.map(Box::new);
		Device {
			id: model.id,
			processor,
			sidechain: None,
			modulation,
		}
	}

	fn inserts(&mut self, devices: &[cadenza_model::Device], owner: &str) -> Vec<Device> {
		devices
			.iter()
			.filter_map(|d| {
				self.instantiate(d, DeviceRole::Effect, owner)
					.map(|processor| self.device(d, processor, false, owner))
			})
			.collect()
	}

	fn audio(&mut self, path: &PathBuf, owner: &str) -> Option<Arc<AudioBuffer>> {
		if let Some(cached) = self.files.get(path) {
			return cached.clone();
		}
		let loaded = match audio_clip::load(path, self.sample_rate) {
			Ok(data) => Some(data),
			Err(e) => {
				self.warnings.push(format!(
					"{owner}: {e}; its clips are silent (fix the path or re-import the file)"
				));
				None
			}
		};
		self.files.insert(path.clone(), loaded.clone());
		loaded
	}

	/// The device's sample zones with their audio loaded at the engine rate. A file that
	/// cannot be read leaves its zone silent, with one warning per file.
	fn sample_zones(
		&mut self,
		device: &cadenza_model::Device,
		owner: &str,
	) -> Vec<cadenza_dsp::Zone> {
		device
			.zones
			.iter()
			.map(|z| {
				let path = &z.sample.path;
				let sample = match self.files.get(path) {
					Some(cached) => cached.clone(),
					None => {
						let loaded = match audio_clip::load(path, self.sample_rate) {
							Ok(data) => Some(data),
							Err(e) => {
								self.warnings.push(format!(
									"{owner}: '{}' sample '{}' (key {}): {e}; the zone is silent (load the \
									 sample again or fix the path)",
									device.name,
									z.sample.display_name(),
									cadenza_model::note_name(z.key_low),
								));
								None
							}
						};
						self.files.insert(path.clone(), loaded.clone());
						loaded
					}
				};
				crate::samples::dsp_zone(z, sample)
			})
			.collect()
	}

	/// A frozen track's render as its only clip (`None`: not frozen, or the render is
	/// missing, in which case the track plays normally with a warning).
	fn frozen_source(&mut self, track: &Track, owner: &str) -> Option<Source> {
		let f = track.freeze.as_ref()?;
		let Ok(source) = audio_clip::load(&f.file, self.sample_rate) else {
			self.warnings.push(format!(
				"{owner} is frozen but its render '{}' cannot be read; playing it unfrozen (freeze it again)",
				f.file.display()
			));
			return None;
		};
		let region = cadenza_audio_io::ClipRegion::whole(&source);
		Some(Source::Audio {
			clips: vec![ClipPlayback {
				source,
				start: 0,
				region,
				crossfaded_in: false,
				crossfaded_out: false,
			}],
			fade_in: 0,
		})
	}

	fn source(&mut self, track: &Track, owner: &str) -> (Source, bool) {
		let sr = self.sample_rate as f64;
		match track.kind {
			TrackKind::Instrument => {
				let device = track.instrument.as_ref().and_then(|d| {
					self.instantiate(d, DeviceRole::Instrument, owner)
						.map(|processor| self.device(d, processor, true, owner))
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
				let events = Vec::with_capacity(event_capacity(&schedule));
				(
					Source::Instrument {
						device,
						schedule,
						events,
						notes_off_on_reset,
						midi_delay: 0,
						release: [0; 2],
					},
					false,
				)
			}
			TrackKind::Audio => {
				let mut clips = Vec::new();
				for clip in track.clips.iter().filter(|c| c.sounds()) {
					let ClipContent::Audio {
						file,
						offset_seconds,
						gain_db,
						fade_in,
						fade_out,
					} = &clip.content
					else {
						continue;
					};
					let Some(source) = self.audio(file, owner) else {
						continue;
					};
					let tempo = &self.project.tempo;
					let start = tempo.tick_to_sample(clip.start, sr);
					let length = tempo.tick_to_sample(clip.end(), sr).saturating_sub(start);
					let (fade_in, fade_out) = audio_clip::fades(*fade_in, *fade_out, length, sr);
					clips.push(ClipPlayback {
						source,
						start: start as i64,
						region: ClipRegion {
							source_offset: (offset_seconds.max(0.0) * sr).round() as u64,
							length,
							gain: cadenza_core::db_to_gain(*gain_db),
							fade_in,
							fade_out,
						},
						crossfaded_in: false,
						crossfaded_out: false,
					});
				}
				audio_clip::crossfade_overlaps(&mut clips);
				audio_clip::micro_fades(&mut clips, sr);
				let mono = !clips.is_empty() && clips.iter().all(ClipPlayback::is_mono);
				(Source::Audio { clips, fade_in: 0 }, mono)
			}
			TrackKind::Bus => (Source::Bus { accumulator: 0 }, false),
			// Folders have no node (never in the processing order).
			TrackKind::Folder => (
				Source::Audio {
					clips: Vec::new(),
					fade_in: 0,
				},
				false,
			),
		}
	}

	fn run(mut self) -> Result<Graph, EngineError> {
		let project = self.project;
		let sr = self.sample_rate;

		// Routing: output first, then sends, per track.
		let mut routes: HashMap<TrackId, Vec<Dest>> = HashMap::new();
		let mut send_specs: HashMap<TrackId, Vec<Option<SendSpec>>> = HashMap::new();
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
				specs.push(Some((send.id, send.level_db, send.pre_fader)));
			}
			routes.insert(track.id, dests);
			send_specs.insert(track.id, specs);
		}

		let buses = self.bus_order(&routes);
		let bus_acc: HashMap<TrackId, usize> =
			buses.iter().enumerate().map(|(i, &b)| (b, i)).collect();
		let master_acc = buses.len();

		let mut priority: Vec<usize> = project
			.tracks
			.iter()
			.enumerate()
			.filter(|(_, t)| !matches!(t.kind, TrackKind::Bus | TrackKind::Folder))
			.map(|(i, _)| i)
			.collect();
		priority.extend(
			buses
				.iter()
				.map(|b| project.track_index(*b).expect("bus exists")),
		);
		let order = self.processing_order(&priority, &routes);
		let position: HashMap<TrackId, usize> = order
			.iter()
			.enumerate()
			.map(|(pos, &i)| (project.tracks[i].id, pos))
			.collect();

		// Solo-in-place: a source track plays when nothing is soloed, when it (or a folder
		// holding it) is soloed, or when a bus on its output path is. Buses and master are
		// never solo-muted. Folders: their mute mutes, their volume offsets (VCA) the tracks
		// inside.
		let any_solo = project.tracks.iter().any(|t| t.channel.solo);
		let folder_solo = |t: &Track| {
			project
				.folders_of(t.id)
				.iter()
				.any(|f| project.track(*f).is_some_and(|f| f.channel.solo))
		};
		let folder_mute = |t: &Track| {
			project
				.folders_of(t.id)
				.iter()
				.any(|f| project.track(*f).is_some_and(|f| f.channel.mute))
		};
		let folder_db = |t: &Track| -> f32 {
			project
				.folders_of(t.id)
				.iter()
				.filter_map(|f| project.track(*f))
				.map(|f| f.channel.volume_db)
				.sum()
		};
		let solo_path = |track: &Track| {
			let mut current = Some(track);
			let mut hops = 0;
			while let Some(t) = current {
				if t.channel.solo || folder_solo(t) {
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

		let mut nodes: Vec<Node> = Vec::with_capacity(order.len());
		let mut pdc_nodes = Vec::with_capacity(order.len());
		let mut pdc_edges = Vec::new();
		for (pos, &ti) in order.iter().enumerate() {
			let track = &project.tracks[ti];
			let owner = format!("track '{}'", track.name);
			self.current_track = Some(track.id);
			let frozen = self.frozen_source(track, &owner);
			let is_frozen = frozen.is_some();
			let (mut source, mono) = match frozen {
				Some(s) => (s, false),
				None => self.source(track, &owner),
			};
			let input = (track.kind == TrackKind::Audio)
				.then_some(track.input)
				.flatten()
				.map(|i| {
					let left = usize::from(i.channel).min(crate::input::MAX_INPUT_CHANNELS - 1);
					crate::graph::NodeInput {
						left,
						right: if i.stereo {
							(left + 1).min(crate::input::MAX_INPUT_CHANNELS - 1)
						} else {
							left
						},
						mode: i.monitor,
					}
				});
			// A mono input pans like a mono clip (only when the clips agree).
			let mono = match input {
				Some(i) if track.clips.is_empty() => i.left == i.right,
				Some(i) => mono && i.left == i.right,
				None => mono,
			};
			if let Source::Bus { accumulator } = &mut source {
				*accumulator = bus_acc[&track.id];
			}
			// A frozen track's inserts are in its render: none are loaded.
			let mut inserts = if is_frozen {
				Vec::new()
			} else {
				self.inserts(&track.channel.inserts, &owner)
			};
			let mut stages = Vec::new();
			if let Source::Instrument {
				device: Some(d), ..
			} = &mut source
			{
				self.key(d, track.devices(), Some(pos), &position, &mut nodes, &owner);
				stages.push(stage(d));
			}
			for d in &mut inserts {
				self.key(d, track.devices(), Some(pos), &position, &mut nodes, &owner);
				stages.push(stage(d));
			}
			pdc_nodes.push(PdcNode {
				input: match &source {
					Source::Bus { accumulator } => Some(*accumulator),
					_ => None,
				},
				stages,
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
				let (send, gain, pre_fader) = spec.map_or((None, 1.0, false), |(id, db, pre)| {
					(Some(id), cadenza_core::db_to_gain(db), pre)
				});
				pdc_edges.push(PdcEdge {
					from: pos,
					to_accumulator: accumulator,
					tap: Tap::Out,
				});
				edges.push(Edge {
					accumulator,
					send,
					gain,
					target_gain: gain,
					pre_fader,
					delay: StereoDelay::new(0),
				});
			}
			let aux_edges = self.aux_edges(track, &source, pos, &position, &bus_acc, &owner);
			for e in &aux_edges {
				if let Some(accumulator) = e.accumulator {
					pdc_edges.push(PdcEdge {
						from: pos,
						to_accumulator: accumulator,
						tap: Tap::FirstStage,
					});
				}
			}

			let muted = track.channel.mute || folder_mute(track);
			let audible = !muted && (track.kind == TrackKind::Bus || !any_solo || solo_path(track));
			let group_db = folder_db(track);
			nodes.push(Node {
				track_id: track.id,
				name: track.name.clone(),
				display_index: ti,
				source,
				inserts,
				fader: Fader::new(
					track.channel.volume_db + group_db,
					track.channel.pan,
					mono,
					sr as f32,
				),
				own_db: track.channel.volume_db,
				folder_db: group_db,
				audible,
				muted,
				edges,
				aux_edges,
				buf: StereoBuf::new(),
				tap_pre: false,
				pre: StereoBuf::new(),
				out_latency: 0,
				input,
				armed: false,
				monitoring: false,
			});
		}

		self.current_track = None;
		let mut master_inserts = self.inserts(&project.master.inserts, "master");
		let mut master_stages = Vec::new();
		for d in &mut master_inserts {
			self.key(
				d,
				project.master.inserts.iter(),
				None,
				&position,
				&mut nodes,
				"master",
			);
			master_stages.push(stage(d));
		}
		let pdc = plan(&pdc_nodes, &pdc_edges, master_acc + 1, &master_stages);
		let mut edge_index = 0;
		for (i, node) in nodes.iter_mut().enumerate() {
			node.out_latency = pdc.node_out_latency[i];
			for edge in &mut node.edges {
				edge.delay = StereoDelay::new(pdc.edge_delay[edge_index]);
				edge_index += 1;
			}
			for edge in node
				.aux_edges
				.iter_mut()
				.filter(|e| e.accumulator.is_some())
			{
				edge.delay = StereoDelay::new(pdc.edge_delay[edge_index]);
				edge_index += 1;
			}
			let mut delays = pdc.stage_delays[i].iter();
			if let Source::Instrument {
				device: Some(d),
				midi_delay,
				..
			} = &mut node.source
				&& let Some(delay) = delays.next()
			{
				// Instruments have no audio input: their notes are delayed instead.
				*midi_delay = delay.main;
				apply_delay(d, delay.key, 0);
			}
			for (d, delay) in node.inserts.iter_mut().zip(delays) {
				apply_delay(d, delay.key, delay.main);
			}
		}
		for (d, delay) in master_inserts.iter_mut().zip(&pdc.master_delays) {
			apply_delay(d, delay.key, delay.main);
		}
		let latency = pdc.latency;

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
		let folders: Vec<crate::graph::FolderGain> = project
			.tracks
			.iter()
			.filter(|t| t.kind == TrackKind::Folder)
			.map(|f| {
				let inside = project.folder_contents(f.id);
				crate::graph::FolderGain {
					track: f.id,
					db: f.channel.volume_db,
					nodes: nodes
						.iter()
						.enumerate()
						.filter(|(_, n)| inside.contains(&n.track_id))
						.map(|(i, _)| i)
						.collect(),
				}
			})
			.collect();
		let mut resolver = resolver(&nodes, &master, &project.tempo, f64::from(sr));
		resolver.folders = folders
			.iter()
			.enumerate()
			.map(|(i, f)| (f.track, i))
			.collect();
		let automation = resolver.bind(
			&project.automation,
			&project.tempo,
			Some(project),
			&mut self.warnings,
		);
		let meters = crate::meter::Meters::new(nodes.iter().map(|n| n.track_id), sr);
		let audition = Arc::new(crate::graph::Audition::new(nodes.len()));

		Ok(Graph {
			sample_rate: sr,
			accumulators: (0..=master_acc).map(|_| StereoBuf::new()).collect(),
			nodes,
			master,
			automation,
			resolver,
			folders,
			load_id: 0,
			tempo: crate::graph::tempo_segments(&project.tempo, sr as f64),
			latency,
			tail_seconds: self.tail.min(MAX_TAIL_SECONDS),
			warnings: self.warnings,
			plugin_watches: self.watches,
			plugin_editors: self.editors,
			engine_pos: 0,
			discard: latency,
			out_pos: 0,
			fifo_pos: QUANTUM,
			playing: false,
			needs_reset: true,
			monitor: None,
			monitor_release: None,
			live: Vec::with_capacity(crate::LIVE_EVENT_CAPACITY),
			preview_frames: 0,
			held: [0; 2],
			previewed: false,
			meters,
			loop_range: None,
			wrap_pending: None,
			out_wrap: None,
			audition,
			input_block: Box::new([[0.0; QUANTUM]; crate::input::MAX_INPUT_CHANNELS]),
			recording: false,
			metronome: crate::metronome::Metronome::new(sr, latency),
			meter: crate::metronome::meter_segments(project.meter()),
			count_in: crate::graph::CountIn::default(),
			segments: [crate::graph::Segment::default(); crate::graph::MAX_SEGMENTS],
			segment_count: 0,
			probe: crate::graph::ProbeTap::new(),
			listen: None,
			master_tap: None,
		})
	}

	/// Processing order: non-bus tracks in display order, then buses (`priority`), moved
	/// only as much as needed so every track comes after the tracks it needs (audio
	/// routed to a bus, extra instrument outputs, sidechain keys). Tracks caught in a loop
	/// keep their priority order; the connections that would run backwards are dropped
	/// later with a warning.
	fn processing_order(
		&self,
		priority: &[usize],
		routes: &HashMap<TrackId, Vec<Dest>>,
	) -> Vec<usize> {
		let project = self.project;
		let index: HashMap<TrackId, usize> = priority
			.iter()
			.enumerate()
			.map(|(k, &ti)| (project.tracks[ti].id, k))
			.collect();
		let mut needs: Vec<Vec<usize>> = vec![Vec::new(); priority.len()];
		for (k, &ti) in priority.iter().enumerate() {
			let track = &project.tracks[ti];
			let aux = track
				.instrument
				.iter()
				.flat_map(|d| d.aux_outputs.iter().filter_map(|r| r.bus));
			let feeds = routes[&track.id]
				.iter()
				.filter_map(|d| match d {
					Dest::Bus(b) => Some(*b),
					Dest::Master => None,
				})
				.chain(aux);
			for bus in feeds {
				if let Some(&b) = index.get(&bus)
					&& b != k
				{
					needs[b].push(k);
				}
			}
			for d in track.devices() {
				if let Some(&src) = d.sidechain.and_then(|s| index.get(&s.track))
					&& src != k
				{
					needs[k].push(src);
				}
			}
		}
		let mut done = vec![false; priority.len()];
		let mut order = Vec::with_capacity(priority.len());
		while order.len() < priority.len() {
			let ready =
				(0..priority.len()).find(|&k| !done[k] && needs[k].iter().all(|&n| done[n]));
			let Some(k) = ready.or_else(|| (0..priority.len()).find(|&k| !done[k])) else {
				break;
			};
			done[k] = true;
			order.push(priority[k]);
		}
		order
	}

	/// Connects `device`'s sidechain (as declared on the model device with the same id
	/// among `declared`) when its source is processed earlier; warns otherwise.
	/// `pos` is the position of the device's node (`None` = master).
	fn key<'d>(
		&mut self,
		device: &mut Device,
		mut declared: impl Iterator<Item = &'d cadenza_model::Device>,
		pos: Option<usize>,
		position: &HashMap<TrackId, usize>,
		nodes: &mut [Node],
		owner: &str,
	) {
		let Some(model) = declared.find(|d| d.id == device.id) else {
			return;
		};
		let Some(source) = model.sidechain else {
			return;
		};
		let name = &model.name;
		let source_name = self
			.project
			.track(source.track)
			.map_or_else(|| source.track.to_string(), |t| format!("'{}'", t.name));
		let Some(&src) = position.get(&source.track) else {
			self.warnings.push(format!(
				"{owner}: device '{name}' is keyed by track {source_name}, which does not exist; sidechain ignored"
			));
			return;
		};
		if pos.is_some_and(|p| src >= p) {
			self.warnings.push(format!(
				"{owner}: device '{name}' keyed by track {source_name} would create a feedback loop; sidechain ignored"
			));
			return;
		}
		let feeds = device.processor.has_sidechain();
		let follows = device.modulation.as_ref().is_some_and(|m| m.needs_key());
		if !feeds && !follows {
			self.warnings.push(format!(
				"{owner}: device '{name}' has no sidechain input; the sidechain from track {source_name} is ignored (use a compressor or a plugin with a sidechain input)"
			));
			return;
		}
		if source.pre_fader {
			nodes[src].tap_pre = true;
		}
		device.sidechain = Some(Box::new(Sidechain {
			source: src,
			pre_fader: source.pre_fader,
			feeds,
			key: StereoBuf::new(),
			key_delay: StereoDelay::new(0),
			main_delay: StereoDelay::new(0),
		}));
	}

	/// Extra instrument outputs routed by `track`'s instrument.
	fn aux_edges(
		&mut self,
		track: &Track,
		source: &Source,
		pos: usize,
		position: &HashMap<TrackId, usize>,
		bus_acc: &HashMap<TrackId, usize>,
		owner: &str,
	) -> Vec<AuxEdge> {
		let routes = track
			.instrument
			.as_ref()
			.map_or(&[][..], |d| d.aux_outputs.as_slice());
		let Source::Instrument {
			device: Some(device),
			..
		} = source
		else {
			return Vec::new();
		};
		let available = device.processor.aux_output_count();
		let mut edges = Vec::new();
		for route in routes {
			let port = route.port as usize;
			if port == 0 || port > available {
				self.warnings.push(format!(
					"{owner}: the instrument has {available} extra output(s); output {port} is not available and was ignored"
				));
				continue;
			}
			let accumulator = match route.bus {
				None => None,
				Some(bus) if bus != track.id && self.is_bus(bus) && position[&bus] > pos => {
					Some(bus_acc[&bus])
				}
				Some(bus) => {
					self.warnings.push(format!(
						"{owner}: instrument output {port} -> {bus} is not a valid bus route (missing, not a bus, or a feedback loop); ignored"
					));
					continue;
				}
			};
			edges.push(AuxEdge {
				port: port - 1,
				accumulator,
				delay: StereoDelay::new(0),
			});
		}
		edges
	}
}

/// Address -> target map of a compiled graph.
fn resolver(nodes: &[Node], master: &Master, tempo: &TempoMap, sample_rate: f64) -> Resolver {
	let mut r = Resolver {
		tempo: tempo.clone(),
		sample_rate,
		..Resolver::default()
	};
	let mut add = |node: Option<usize>, slot: DeviceSlot, d: &Device| {
		r.devices.insert(
			d.id,
			DeviceEntry {
				node,
				slot,
				info: d.processor.info(),
			},
		);
	};
	for (i, n) in nodes.iter().enumerate() {
		if let Source::Instrument {
			device: Some(d), ..
		} = &n.source
		{
			add(Some(i), DeviceSlot::Instrument, d);
		}
		for (k, d) in n.inserts.iter().enumerate() {
			add(Some(i), DeviceSlot::Insert(k), d);
		}
	}
	for (k, d) in master.inserts.iter().enumerate() {
		add(None, DeviceSlot::Insert(k), d);
	}
	let bus_nodes: HashMap<usize, usize> = nodes
		.iter()
		.enumerate()
		.filter_map(|(i, n)| match n.source {
			Source::Bus { accumulator } => Some((accumulator, i)),
			_ => None,
		})
		.collect();
	for (i, n) in nodes.iter().enumerate() {
		r.nodes.insert(n.track_id, i);
		if matches!(n.source, Source::Instrument { .. }) {
			r.instruments.insert(i);
		}
		for (k, e) in n.edges.iter().enumerate() {
			if let Some(send) = e.send {
				r.sends.insert(send, (i, k));
			}
		}
		let feeds = n
			.edges
			.iter()
			.map(|e| e.accumulator)
			.chain(n.aux_edges.iter().filter_map(|e| e.accumulator))
			.filter_map(|a| bus_nodes.get(&a).copied())
			.collect();
		r.feeds.push(feeds);
	}
	r
}

/// Room for one quantum of a schedule's events: a possible `AllNotesOff` on reset, every
/// note that could be chased back in at that instant, live input (monitoring) and its
/// release, and twice the scheduled events and chased notes for a loop wrap (both sides
/// of the jump), plus the releases of a schedule swap.
pub(crate) fn event_capacity(schedule: &MidiSchedule) -> usize {
	2 * (schedule.max_events_per_quantum() + schedule.max_chase_notes())
		+ crate::LIVE_EVENT_CAPACITY
		+ 2 + 128
}

fn stage(d: &Device) -> PdcStage {
	PdcStage {
		latency: d.processor.latency(),
		key: d.sidechain.as_ref().map(|s| s.source),
	}
}

fn apply_delay(d: &mut Device, key: u32, main: u32) {
	if let Some(sc) = &mut d.sidechain {
		sc.key_delay = StereoDelay::new(key);
		sc.main_delay = StereoDelay::new(main);
	}
}

fn role_name(role: DeviceRole) -> &'static str {
	match role {
		DeviceRole::Instrument => "an instrument",
		DeviceRole::Effect => "an effect",
	}
}
