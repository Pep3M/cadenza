//! Factory presets of the built-in devices: named parameter maps. Loading one
//! replaces a device's explicit parameter values (params left out return to
//! their defaults).

/// A named set of parameter values for a built-in device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactoryPreset {
	pub name: &'static str,
	/// What it sounds like and when to use it, written for an AI choosing presets.
	pub description: &'static str,
	/// Plain values by param id; every other param stays at its default.
	pub params: &'static [(&'static str, f32)],
}

const fn preset(
	name: &'static str,
	description: &'static str,
	params: &'static [(&'static str, f32)],
) -> FactoryPreset {
	FactoryPreset {
		name,
		description,
		params,
	}
}

static POLY_SYNTH: [FactoryPreset; 3] = [
	preset(
		"Warm Pad",
		"Slow, detuned saw pad with a soft filter; sustained chords and beds.",
		&[
			("unison", 3.0),
			("unison_detune", 18.0),
			("filter_cutoff", 1_400.0),
			("filter_resonance", 10.0),
			("filter_env_amount", 12.0),
			("amp_attack", 600.0),
			("amp_sustain", 85.0),
			("amp_release", 1_200.0),
			("gain", -6.0),
		],
	),
	preset(
		"Pluck Lead",
		"Bright, short pluck with a snappy filter envelope; arpeggios and hooks.",
		&[
			("osc2_wave", 2.0),
			("osc2_semitones", 12.0),
			("filter_cutoff", 900.0),
			("filter_resonance", 30.0),
			("filter_env_amount", 40.0),
			("filter_decay", 180.0),
			("filter_sustain", 0.0),
			("amp_decay", 350.0),
			("amp_sustain", 0.0),
			("amp_release", 200.0),
		],
	),
	preset(
		"Sub Bass",
		"Round mono-style bass: sine + square an octave down, low cutoff, tight release.",
		&[
			("osc1_wave", 0.0),
			("osc2_wave", 2.0),
			("osc2_semitones", -12.0),
			("osc2_cents", 0.0),
			("osc_mix", 35.0),
			("filter_cutoff", 450.0),
			("filter_env_amount", 6.0),
			("amp_sustain", 90.0),
			("amp_release", 80.0),
			("glide", 30.0),
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

/// Factory presets of the built-in device `id` (empty for unknown ids).
pub fn factory_presets(id: &str) -> &'static [FactoryPreset] {
	match id {
		"poly_synth" => &POLY_SYNTH,
		"drum_machine" => &DRUM_MACHINE,
		"electric_piano" => &ELECTRIC_PIANO,
		"compressor" => &COMPRESSOR,
		"reverb" => &REVERB,
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
			let mut names: Vec<_> = presets.iter().map(|p| p.name).collect();
			names.sort_unstable();
			names.dedup();
			assert_eq!(names.len(), presets.len(), "{}: duplicate names", info.id);
		}
		assert!(factory_presets("nope").is_empty());
		assert_eq!(factory_presets("compressor").len(), 3);
	}
}
