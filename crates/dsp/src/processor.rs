//! The real-time processing contract shared by every built-in device.

/// Whether a device generates sound from MIDI or transforms incoming audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceRole {
	Instrument,
	Effect,
}

/// Static description of one automatable parameter. Values are always plain
/// (unnormalized) numbers in `min..=max`; switch-like parameters use
/// integer-valued floats (e.g. `0` = off, `1` = on).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
	pub id: &'static str,
	pub name: &'static str,
	pub min: f32,
	pub max: f32,
	pub default: f32,
	pub unit: &'static str,
	pub description: &'static str,
}

impl ParamInfo {
	pub const fn new(
		id: &'static str,
		name: &'static str,
		min: f32,
		max: f32,
		default: f32,
		unit: &'static str,
		description: &'static str,
	) -> Self {
		Self {
			id,
			name,
			min,
			max,
			default,
			unit,
			description,
		}
	}

	/// Clamps `value` into this parameter's range. Returns `None` for NaN so a
	/// bad automation value never poisons the DSP state.
	pub fn clamp(&self, value: f32) -> Option<f32> {
		if value.is_nan() {
			None
		} else {
			Some(value.clamp(self.min, self.max))
		}
	}
}

/// Static description of a device, written so an LLM can pick and configure it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceInfo {
	pub id: &'static str,
	pub name: &'static str,
	pub role: DeviceRole,
	pub description: &'static str,
	pub params: &'static [ParamInfo],
}

impl DeviceInfo {
	/// Index of the parameter with the given stable id.
	pub fn param_index(&self, id: &str) -> Option<usize> {
		self.params.iter().position(|p| p.id == id)
	}

	/// The parameter with the given stable id.
	pub fn param(&self, id: &str) -> Option<&'static ParamInfo> {
		self.params.iter().find(|p| p.id == id)
	}
}

/// A MIDI-like event positioned at a sample `offset` inside the current block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiEvent {
	/// `velocity` is in `0.0..=1.0`.
	NoteOn {
		offset: u32,
		pitch: u8,
		velocity: f32,
	},
	NoteOff {
		offset: u32,
		pitch: u8,
	},
	AllNotesOff {
		offset: u32,
	},
	/// Control change: `controller` 0..=127 (1 = mod wheel, 64 = sustain pedal), `value`
	/// in `0.0..=1.0` (MIDI 0..=127).
	ControlChange {
		offset: u32,
		controller: u8,
		value: f32,
	},
	/// Pitch bend, `value` in `-1.0..=1.0` (0 = center; instruments bend ±2 semitones by
	/// default).
	PitchBend {
		offset: u32,
		value: f32,
	},
	/// Channel pressure (aftertouch), `value` in `0.0..=1.0`.
	ChannelPressure {
		offset: u32,
		value: f32,
	},
}

/// MIDI controller numbers with a meaning in the built-in instruments.
pub mod cc {
	pub const MOD_WHEEL: u8 = 1;
	pub const SUSTAIN: u8 = 64;
	pub const ALL_SOUND_OFF: u8 = 120;
	pub const RESET_ALL_CONTROLLERS: u8 = 121;
	pub const ALL_NOTES_OFF: u8 = 123;
}

impl MidiEvent {
	pub fn offset(&self) -> u32 {
		match *self {
			MidiEvent::NoteOn { offset, .. }
			| MidiEvent::NoteOff { offset, .. }
			| MidiEvent::AllNotesOff { offset }
			| MidiEvent::ControlChange { offset, .. }
			| MidiEvent::PitchBend { offset, .. }
			| MidiEvent::ChannelPressure { offset, .. } => offset,
		}
	}

	/// The same event at another offset.
	pub fn with_offset(self, offset: u32) -> Self {
		match self {
			MidiEvent::NoteOn {
				pitch, velocity, ..
			} => MidiEvent::NoteOn {
				offset,
				pitch,
				velocity,
			},
			MidiEvent::NoteOff { pitch, .. } => MidiEvent::NoteOff { offset, pitch },
			MidiEvent::AllNotesOff { .. } => MidiEvent::AllNotesOff { offset },
			MidiEvent::ControlChange {
				controller, value, ..
			} => MidiEvent::ControlChange {
				offset,
				controller,
				value,
			},
			MidiEvent::PitchBend { value, .. } => MidiEvent::PitchBend { offset, value },
			MidiEvent::ChannelPressure { value, .. } => {
				MidiEvent::ChannelPressure { offset, value }
			}
		}
	}

	/// Whether this is a note event (on, off, all off) rather than a controller.
	pub fn is_note(&self) -> bool {
		matches!(
			self,
			MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. } | MidiEvent::AllNotesOff { .. }
		)
	}
}

/// Host transport state for the current block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transport {
	pub bpm: f64,
	pub playing: bool,
	/// Position of the first sample of the block, in quarter notes.
	pub beat_position: f64,
	/// Time signature in effect at the block start.
	pub time_sig_numerator: u16,
	pub time_sig_denominator: u16,
	/// Start of the bar holding the block start, in quarter notes.
	pub bar_start: f64,
	/// 0-based index of that bar.
	pub bar_number: i32,
}

impl Default for Transport {
	fn default() -> Self {
		Self {
			bpm: 120.0,
			playing: false,
			beat_position: 0.0,
			time_sig_numerator: 4,
			time_sig_denominator: 4,
			bar_start: 0.0,
			bar_number: 0,
		}
	}
}

/// A real-time audio device.
///
/// Rules for implementors (and guarantees for hosts):
/// - `process` never allocates, locks, performs I/O or panics on valid input.
///   All buffers are allocated in `prepare`.
/// - `process` accepts any block length from 1 up to the `max_block` passed to
///   `prepare`; `left` and `right` always have equal length.
/// - Instruments overwrite `left`/`right`; effects process them in place.
/// - `events` are sorted by `offset`; offsets past the block end are clamped.
/// - Parameter changes are smoothed internally, so hosts may call `set_param`
///   at block boundaries without causing zipper noise.
pub trait Processor: Send {
	fn info(&self) -> &'static DeviceInfo;
	fn prepare(&mut self, sample_rate: f32, max_block: usize);
	/// Sets a parameter by index in `info().params`, as a plain value.
	/// Out-of-range values are clamped; unknown indices and NaN are ignored.
	fn set_param(&mut self, index: usize, value: f32);
	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		events: &[MidiEvent],
		transport: &Transport,
	);
	/// Clears all internal state (voices, delay lines, envelopes).
	fn reset(&mut self);
	/// Called on the audio thread, about once per output buffer, while the host does
	/// not `process` the device (transport stopped). Plugins use it to exchange pending
	/// parameter changes with their editor. Realtime rules apply, as for `process`.
	fn idle(&mut self) {}
	/// Processing latency in samples that the host should compensate.
	fn latency(&self) -> u32 {
		0
	}
	/// How long the device keeps producing output after its input goes silent.
	fn tail_seconds(&self) -> f32 {
		0.0
	}
	/// Serializes the device's complete internal state (e.g. a plugin's preset
	/// chunk) as opaque bytes. Non-realtime: called off the audio thread.
	/// `None` means the device has no state beyond its parameters.
	fn save_state(&mut self) -> Option<Vec<u8>> {
		None
	}
	/// Restores state produced by [`Processor::save_state`]. Non-realtime:
	/// called off the audio thread. Returns `false` if the data was rejected
	/// or the device has no opaque state.
	fn load_state(&mut self, _data: &[u8]) -> bool {
		false
	}
	/// Whether the device has a sidechain (key) input, fed with
	/// [`Processor::set_sidechain`].
	fn has_sidechain(&self) -> bool {
		false
	}
	/// Provides the sidechain signal for the next `process` call only (same
	/// length as that block). A block processed without it has no sidechain
	/// signal: devices then key from their main input (dynamics) or treat the
	/// sidechain as silent. Realtime-safe: implementors copy into buffers
	/// allocated in `prepare`.
	fn set_sidechain(&mut self, _left: &[f32], _right: &[f32]) {}
	/// Number of extra stereo outputs besides the main one (e.g. the
	/// individual outputs of a multi-output drum sampler).
	fn aux_output_count(&self) -> usize {
		0
	}
	/// Extra output `index` (`0..aux_output_count()`) of the last `process`
	/// call, as long as that block. Realtime-safe.
	fn aux_output(&self, _index: usize) -> Option<(&[f32], &[f32])> {
		None
	}
	/// Sample-based built-ins ([`crate::sample::uses_samples`]): replaces the zone map
	/// (sample references are shared, never copied). Non-realtime: hosts call it after
	/// `prepare` and never concurrently with `process`; sounding voices are stopped.
	/// Returns `false` for devices that do not play samples.
	fn set_zones(&mut self, _zones: Vec<crate::sample::Zone>) -> bool {
		false
	}
	/// Factory presets the device itself exposes (third-party plugins).
	/// Built-in presets are listed by [`crate::factory_presets`] instead.
	/// Non-realtime.
	fn presets(&mut self) -> Vec<PresetInfo> {
		Vec::new()
	}
	/// Loads the factory preset with this [`PresetInfo::key`]; a following
	/// [`Processor::save_state`] captures the result. Non-realtime. Returns
	/// `false` if the preset is unknown or the device refused it.
	fn load_preset(&mut self, _key: &str) -> bool {
		false
	}
	/// Live readings of the last `process` call (e.g. gain reduction) written into
	/// `out` (at most [`readings::MAX`] values; index meanings in [`readings`]).
	/// Returns how many were written. Realtime-safe: called on the audio thread.
	fn readings(&self, _out: &mut [f32]) -> usize {
		0
	}
}

/// Index meanings of [`Processor::readings`].
pub mod readings {
	/// Most readings a device publishes.
	pub const MAX: usize = 16;
	/// `compressor`, `limiter`: largest gain reduction of the last block, positive dB.
	pub const GAIN_REDUCTION_DB: usize = 0;
	/// `drum_machine`: first of [`DRUM_VOICES`] trigger counters, one per voice in
	/// [`DRUM_VOICE_NAMES`] order (kick, snare, clap, hats, toms, cymbals, perc). Each
	/// grows by one per note-on of that voice; readers compare against the last value.
	pub const DRUM_VOICE_TRIGGERS_START: usize = 0;
	/// Number of `drum_machine` trigger counters.
	pub const DRUM_VOICES: usize = 7;
	/// `drum_machine` voice order of the trigger counters.
	pub const DRUM_VOICE_NAMES: [&str; DRUM_VOICES] =
		["kick", "snare", "clap", "hats", "toms", "cymbals", "perc"];
	pub const DRUM_KICK: usize = 0;
	pub const DRUM_SNARE: usize = 1;
	pub const DRUM_CLAP: usize = 2;
	pub const DRUM_HATS: usize = 3;
	pub const DRUM_TOMS: usize = 4;
	pub const DRUM_CYMBALS: usize = 5;
	pub const DRUM_PERC: usize = 6;
	/// `drum_sampler`: first of [`DRUM_PADS`] trigger counters, one per pad of
	/// `instruments::drum_sampler::PADS` (in that order). Each grows by one per note-on of
	/// the pad's note that starts a sample.
	pub const DRUM_PAD_TRIGGERS_START: usize = 0;
	/// Number of `drum_sampler` pad trigger counters.
	pub const DRUM_PADS: usize = 16;
}

/// A factory preset exposed by a device (see [`Processor::presets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetInfo {
	/// Opaque, device-specific key for [`Processor::load_preset`].
	pub key: String,
	pub name: String,
	/// Bank, category or folder; may be empty.
	pub category: String,
}
