//! Factory presets of the built-in devices: named parameter maps (plus sample zones and
//! modulators). Loading one replaces a device's explicit parameter values (params left
//! out return to their defaults) and its modulators.

mod poly_synth;

/// A named set of parameter values for a built-in device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactoryPreset {
	pub name: &'static str,
	/// One of [`PRESET_CATEGORIES`], or empty for devices whose presets are not grouped.
	pub category: &'static str,
	/// What it sounds like and when to use it, written for an AI choosing presets.
	pub description: &'static str,
	/// Plain values by param id; every other param stays at its default.
	pub params: &'static [(&'static str, f32)],
	/// Sample zones (sample-based devices), playing factory samples by file name.
	pub zones: &'static [FactoryZone],
	/// Modulators the preset installs on the device (replacing its own).
	pub modulators: &'static [PresetModulator],
}

impl FactoryPreset {
	pub const fn category(mut self, category: &'static str) -> Self {
		self.category = category;
		self
	}

	pub const fn modulators(mut self, modulators: &'static [PresetModulator]) -> Self {
		self.modulators = modulators;
		self
	}
}

/// Preset categories, in browsing order.
pub const PRESET_CATEGORIES: [&str; 12] = [
	"Bass", "808", "Lead", "Pluck", "Keys", "Bell", "Pad", "Choir", "Strings", "Brass", "Arp", "FX",
];

/// LFO wave of a [`PresetModulator`] (mirrors the model's `LfoShape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetLfoShape {
	Sine,
	Triangle,
	Saw,
	Square,
	Random,
}

/// When a preset LFO restarts (mirrors the model's `Retrigger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetRetrigger {
	Free,
	Note,
	Transport,
}

/// What drives a [`PresetModulator`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PresetModSource {
	Lfo {
		shape: PresetLfoShape,
		rate_hz: f32,
		/// Tempo-synced cycle in quarter notes; overrides `rate_hz`.
		sync_beats: Option<f32>,
		phase: f32,
		retrigger: PresetRetrigger,
	},
	/// Follower of the device's own input (an instrument: its own output).
	Envelope {
		attack_ms: f32,
		release_ms: f32,
		gain_db: f32,
	},
}

/// A modulator of a factory preset: the source and the params it moves, each by a depth
/// in -1..1 of the param's range (the model's `ModTarget`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresetModulator {
	pub source: PresetModSource,
	pub targets: &'static [(&'static str, f32)],
}

impl PresetModulator {
	/// A free-running LFO at `rate_hz`.
	pub const fn lfo(
		shape: PresetLfoShape,
		rate_hz: f32,
		targets: &'static [(&'static str, f32)],
	) -> Self {
		Self {
			source: PresetModSource::Lfo {
				shape,
				rate_hz,
				sync_beats: None,
				phase: 0.0,
				retrigger: PresetRetrigger::Free,
			},
			targets,
		}
	}

	/// An LFO locked to the song: one cycle every `beats` quarter notes.
	pub const fn synced(
		shape: PresetLfoShape,
		beats: f32,
		targets: &'static [(&'static str, f32)],
	) -> Self {
		Self {
			source: PresetModSource::Lfo {
				shape,
				rate_hz: 1.0,
				sync_beats: Some(beats),
				phase: 0.0,
				retrigger: PresetRetrigger::Transport,
			},
			targets,
		}
	}

	/// An envelope follower on the device's own signal.
	pub const fn follower(
		attack_ms: f32,
		release_ms: f32,
		gain_db: f32,
		targets: &'static [(&'static str, f32)],
	) -> Self {
		Self {
			source: PresetModSource::Envelope {
				attack_ms,
				release_ms,
				gain_db,
			},
			targets,
		}
	}

	/// The same LFO restarting at every note-on, from `phase` (0..1).
	pub const fn on_note(mut self, phase: f32) -> Self {
		if let PresetModSource::Lfo {
			shape,
			rate_hz,
			sync_beats,
			..
		} = self.source
		{
			self.source = PresetModSource::Lfo {
				shape,
				rate_hz,
				sync_beats,
				phase,
				retrigger: PresetRetrigger::Note,
			};
		}
		self
	}
}

/// A zone of a factory preset: a file of the factory sample folder (synthesized on first
/// use by the host) and how it is mapped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactoryZone {
	/// File name inside the factory sample folder.
	pub sample: &'static str,
	pub key_low: u8,
	pub key_high: u8,
	pub root_key: u8,
	pub gain_db: f32,
	pub pan: f32,
	pub choke: u8,
	/// Decay in ms (`None` = the sample rings out).
	pub decay_ms: Option<f32>,
	/// `Some((start, end, crossfade))` in seconds: a forward loop.
	pub forward_loop: Option<(f64, f64, f64)>,
}

impl FactoryZone {
	/// A drum pad on `note`.
	pub const fn pad(sample: &'static str, note: u8) -> Self {
		Self {
			sample,
			key_low: note,
			key_high: note,
			root_key: note,
			gain_db: 0.0,
			pan: 0.0,
			choke: 0,
			decay_ms: None,
			forward_loop: None,
		}
	}

	pub const fn gain(mut self, db: f32) -> Self {
		self.gain_db = db;
		self
	}

	pub const fn pan(mut self, pan: f32) -> Self {
		self.pan = pan;
		self
	}

	pub const fn choke(mut self, group: u8) -> Self {
		self.choke = group;
		self
	}

	pub const fn decay(mut self, ms: f32) -> Self {
		self.decay_ms = Some(ms);
		self
	}
}

const fn preset(
	name: &'static str,
	description: &'static str,
	params: &'static [(&'static str, f32)],
) -> FactoryPreset {
	FactoryPreset {
		name,
		category: "",
		description,
		params,
		zones: &[],
		modulators: &[],
	}
}

const fn sampled(
	name: &'static str,
	description: &'static str,
	params: &'static [(&'static str, f32)],
	zones: &'static [FactoryZone],
) -> FactoryPreset {
	FactoryPreset {
		name,
		category: "",
		description,
		params,
		zones,
		modulators: &[],
	}
}

/// Factory sample files of the synthesized kit and multisample (rendered by the host into
/// its factory sample folder; see `cadenza_engine::factory`).
pub const KICK: &str = "Kick.wav";
pub const SNARE: &str = "Snare.wav";
pub const CLAP: &str = "Clap.wav";
pub const CLOSED_HAT: &str = "Closed Hat.wav";
pub const OPEN_HAT: &str = "Open Hat.wav";
pub const LOW_TOM: &str = "Low Tom.wav";
pub const MID_TOM: &str = "Mid Tom.wav";
pub const HIGH_TOM: &str = "High Tom.wav";
pub const CRASH: &str = "Crash.wav";
pub const RIDE: &str = "Ride.wav";
pub const RIM: &str = "Rim.wav";
pub const SINE_PAD: [&str; 4] = [
	"Sine Pad C2.wav",
	"Sine Pad C3.wav",
	"Sine Pad C4.wav",
	"Sine Pad C5.wav",
];
/// Loop of the sine pad samples: start, end, crossfade (seconds).
pub const SINE_PAD_LOOP: (f64, f64, f64) = (0.5, 2.5, 0.25);

const fn pad_loop(sample: &'static str, low: u8, high: u8, root: u8) -> FactoryZone {
	FactoryZone {
		sample,
		key_low: low,
		key_high: high,
		root_key: root,
		gain_db: 0.0,
		pan: 0.0,
		choke: 0,
		decay_ms: None,
		forward_loop: Some(SINE_PAD_LOOP),
	}
}

static DRUM_SAMPLER: [FactoryPreset; 2] = [
	sampled(
		"Synth Kit",
		"Punchy synthesized kit on GM notes (kick, snare, clap, rim, hats with the closed \
		 and pedal hat choking the open hat, three toms, crash, ride). Pop, house, hip-hop.",
		&[],
		&[
			FactoryZone::pad(KICK, 36),
			FactoryZone::pad(KICK, 35).gain(-2.0),
			FactoryZone::pad(SNARE, 38),
			FactoryZone::pad(SNARE, 40).gain(-3.0),
			FactoryZone::pad(RIM, 37).gain(-4.0),
			FactoryZone::pad(CLAP, 39),
			FactoryZone::pad(CLOSED_HAT, 42).choke(1).pan(0.15),
			FactoryZone::pad(CLOSED_HAT, 44)
				.choke(1)
				.pan(0.15)
				.gain(-4.0),
			FactoryZone::pad(OPEN_HAT, 46).choke(1).pan(0.15),
			FactoryZone::pad(LOW_TOM, 41).pan(-0.3),
			FactoryZone::pad(LOW_TOM, 43).pan(-0.3),
			FactoryZone::pad(MID_TOM, 45),
			FactoryZone::pad(MID_TOM, 47),
			FactoryZone::pad(HIGH_TOM, 48).pan(0.3),
			FactoryZone::pad(HIGH_TOM, 50).pan(0.3),
			FactoryZone::pad(CRASH, 49).pan(-0.35),
			FactoryZone::pad(CRASH, 57).pan(0.35),
			FactoryZone::pad(RIDE, 51).pan(0.35),
			FactoryZone::pad(RIDE, 59).pan(0.35),
		],
	),
	sampled(
		"Tight Synth Kit",
		"The Synth Kit with short decays and a gated open hat: tight funk, disco and dense mixes.",
		&[],
		&[
			FactoryZone::pad(KICK, 36).decay(220.0),
			FactoryZone::pad(SNARE, 38).decay(160.0),
			FactoryZone::pad(RIM, 37).gain(-4.0),
			FactoryZone::pad(CLAP, 39).decay(180.0),
			FactoryZone::pad(CLOSED_HAT, 42).choke(1).decay(45.0),
			FactoryZone::pad(CLOSED_HAT, 44)
				.choke(1)
				.decay(40.0)
				.gain(-4.0),
			FactoryZone::pad(OPEN_HAT, 46).choke(1).decay(220.0),
			FactoryZone::pad(LOW_TOM, 41).pan(-0.3).decay(250.0),
			FactoryZone::pad(MID_TOM, 45).decay(250.0),
			FactoryZone::pad(HIGH_TOM, 48).pan(0.3).decay(250.0),
			FactoryZone::pad(CRASH, 49).pan(-0.35).decay(900.0),
			FactoryZone::pad(RIDE, 51).pan(0.35).decay(700.0),
		],
	),
];

static SAMPLER: [FactoryPreset; 2] = [
	sampled(
		"Sine Pad",
		"Soft, looped sine-organ pad multisampled every octave (C2-C5): slow swell, long \
		 release. Chords, beds and sub-friendly bass notes.",
		&[
			("amp_attack", 350.0),
			("amp_release", 1_200.0),
			("filter_type", 1.0),
			("filter_cutoff", 4_000.0),
			("gain", -4.0),
		],
		&[
			pad_loop(SINE_PAD[0], 0, 41, 36),
			pad_loop(SINE_PAD[1], 42, 53, 48),
			pad_loop(SINE_PAD[2], 54, 65, 60),
			pad_loop(SINE_PAD[3], 66, 127, 72),
		],
	),
	sampled(
		"Sine Pluck",
		"The sine multisample as a short, round pluck with a closing filter: arps and keys.",
		&[
			("amp_attack", 2.0),
			("amp_decay", 450.0),
			("amp_sustain", 0.0),
			("amp_release", 250.0),
			("filter_type", 1.0),
			("filter_cutoff", 1_200.0),
			("filter_env_amount", 36.0),
			("filter_decay", 300.0),
			("filter_velocity", 40.0),
		],
		&[
			pad_loop(SINE_PAD[0], 0, 41, 36),
			pad_loop(SINE_PAD[1], 42, 53, 48),
			pad_loop(SINE_PAD[2], 54, 65, 60),
			pad_loop(SINE_PAD[3], 66, 127, 72),
		],
	),
];

static DRUM_MACHINE: [FactoryPreset; 2] = [
	preset(
		"Tight Kit",
		"Short, punchy kit for funk, pop and dense arrangements.",
		&[
			("kick_decay", 280.0),
			("kick_punch", 80.0),
			("snare_decay", 160.0),
			("snare_snappy", 75.0),
			("hat_decay", 45.0),
			("open_hat_decay", 250.0),
			("tone", 60.0),
		],
	),
	preset(
		"Boomy 808",
		"Long, tuned-down kick and dark tone for hip-hop and trap.",
		&[
			("kick_tune", -5.0),
			("kick_decay", 1_400.0),
			("kick_punch", 40.0),
			("snare_decay", 260.0),
			("hat_decay", 60.0),
			("tone", 35.0),
		],
	),
];

static ELECTRIC_PIANO: [FactoryPreset; 2] = [
	preset(
		"Classic Tine",
		"Bright, bell-forward tine piano for soul and pop comping.",
		&[("brightness", 65.0), ("bell", 50.0), ("decay", 5_000.0)],
	),
	preset(
		"Dreamy Tremolo",
		"Mellow piano with stereo-feel tremolo for ballads and lo-fi.",
		&[
			("brightness", 35.0),
			("bell", 25.0),
			("tremolo_rate", 4.0),
			("tremolo_depth", 45.0),
			("release", 600.0),
		],
	),
];

static COMPRESSOR: [FactoryPreset; 3] = [
	preset(
		"Vocal Leveler",
		"Even out a lead vocal: moderate ratio, fast attack, medium release.",
		&[
			("threshold", -20.0),
			("ratio", 3.0),
			("attack", 5.0),
			("release", 100.0),
			("makeup", 4.0),
		],
	),
	preset(
		"Drum Bus Glue",
		"Parallel punch for a drum bus: slow attack lets transients through.",
		&[
			("threshold", -18.0),
			("ratio", 4.0),
			("attack", 20.0),
			("release", 100.0),
			("makeup", 3.0),
			("mix", 50.0),
		],
	),
	preset(
		"Sidechain Pump",
		"Deep, fast ducking keyed by another track (set_sidechain from the kick): the \
		 classic EDM pump on pads and bass.",
		&[
			("threshold", -30.0),
			("ratio", 10.0),
			("attack", 0.5),
			("release", 160.0),
			("knee", 3.0),
		],
	),
];

static REVERB: [FactoryPreset; 2] = [
	preset(
		"Small Room",
		"Short, natural room for drums and dry sources that need a space.",
		&[
			("size", 25.0),
			("decay", 0.6),
			("damping", 60.0),
			("pre_delay", 5.0),
			("mix", 20.0),
		],
	),
	preset(
		"Large Hall",
		"Long, smooth hall for pads, strings and ballad vocals (use on a send bus at 100% mix).",
		&[
			("size", 85.0),
			("decay", 4.5),
			("damping", 40.0),
			("pre_delay", 40.0),
			("mix", 100.0),
		],
	),
];

static PITCH_CORRECT: [FactoryPreset; 4] = [
	preset(
		"Natural",
		"Transparent tuning for a lead vocal: notes settle in tune, scoops and vibrato stay.",
		&[("retune", 50.0), ("humanize", 40.0), ("amount", 100.0)],
	),
	preset(
		"Modern Pop",
		"Tight, polished tuning heard in current pop and R&B, still human on long notes.",
		&[("retune", 15.0), ("humanize", 20.0), ("amount", 100.0)],
	),
	preset(
		"Hard Tune",
		"Instant correction: the stepped, robotic tuned-vocal effect.",
		&[("retune", 0.0), ("humanize", 0.0), ("amount", 100.0)],
	),
	preset(
		"Gentle",
		"Light touch for a good singer or backing vocals: pulls halfway, slowly.",
		&[("retune", 120.0), ("humanize", 50.0), ("amount", 60.0)],
	),
];

/// Factory presets of the built-in device `id` (empty for unknown ids).
pub fn factory_presets(id: &str) -> &'static [FactoryPreset] {
	match id {
		"poly_synth" => &poly_synth::PRESETS,
		"drum_machine" => &DRUM_MACHINE,
		"electric_piano" => &ELECTRIC_PIANO,
		"sampler" => &SAMPLER,
		"drum_sampler" => &DRUM_SAMPLER,
		"compressor" => &COMPRESSOR,
		"reverb" => &REVERB,
		"pitch_correct" => &PITCH_CORRECT,
		_ => &[],
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_preset_param_exists_and_is_in_range() {
		for info in crate::registry() {
			let presets = factory_presets(info.id);
			for p in presets {
				assert!(!p.name.is_empty() && !p.description.is_empty());
				for (id, value) in p.params {
					let param = info
						.param(id)
						.unwrap_or_else(|| panic!("{}/{}: unknown param {id}", info.id, p.name));
					assert!(
						(param.min..=param.max).contains(value),
						"{}/{}: {id} = {value} out of range",
						info.id,
						p.name
					);
				}
			}
			for z in presets.iter().flat_map(|p| p.zones) {
				assert!(z.key_low <= z.key_high && z.key_high <= 127 && z.root_key <= 127);
				assert!(z.sample.ends_with(".wav"));
			}
			assert_eq!(
				crate::uses_samples(info.id),
				presets.iter().all(|p| !p.zones.is_empty()) && !presets.is_empty(),
				"{}: sample devices ship sampled presets",
				info.id
			);
			let mut names: Vec<_> = presets.iter().map(|p| p.name).collect();
			names.sort_unstable();
			names.dedup();
			assert_eq!(names.len(), presets.len(), "{}: duplicate names", info.id);
		}
		assert!(factory_presets("nope").is_empty());
		assert_eq!(factory_presets("compressor").len(), 3);
	}
}
