//! Factory presets of the built-in instruments without a library module of their own.

use super::{FactoryPreset, FactoryZone, SINE_PAD, pad_loop, preset, sampled};

pub(super) static SAMPLER: &[FactoryPreset] = &[
	sampled(
		"Sine Pad",
		"Soft, looped sine-organ pad multisampled every octave (C2-C5): slow swell, long \
		 release. Chords, beds and sub-friendly bass notes.",
		&[
			("amp_attack", 350.0),
			("amp_release", 1_200.0),
			("filter_type", 1.0),
			("filter_cutoff", 4_000.0),
			("gain", -12.0),
		],
		&[
			pad_loop(SINE_PAD[0], 0, 41, 36),
			pad_loop(SINE_PAD[1], 42, 53, 48),
			pad_loop(SINE_PAD[2], 54, 65, 60),
			pad_loop(SINE_PAD[3], 66, 127, 72),
		],
	)
	.category("Pad"),
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
			("gain", -9.0),
		],
		&[
			pad_loop(SINE_PAD[0], 0, 41, 36),
			pad_loop(SINE_PAD[1], 42, 53, 48),
			pad_loop(SINE_PAD[2], 54, 65, 60),
			pad_loop(SINE_PAD[3], 66, 127, 72),
		],
	)
	.category("Pluck"),
	sampled(
		"Trap 808",
		"Mono 808 bass with a hard knock and a short glide between overlapping notes: trap, \
		 hip-hop and modern pop low end. Play it in the song's bass register (C1-C3).",
		&[
			("polyphony", 1.0),
			("legato", 1.0),
			("glide", 50.0),
			("amp_release", 150.0),
			("gain", -3.0),
		],
		&[keyboard("Trap 808 F.flac", 0, 72, F1)],
	)
	.category("808"),
	sampled(
		"Clean 808 Glide",
		"Smooth, round mono 808 with a soft attack and a long portamento: melodic trap and R&B \
		 slides.",
		&[
			("polyphony", 1.0),
			("legato", 1.0),
			("glide", 120.0),
			("amp_release", 200.0),
			("gain", -3.0),
		],
		&[keyboard("Trap 808 Clean F.flac", 0, 72, F1)],
	)
	.category("808"),
	sampled(
		"Rage 808",
		"Clipped, distorted mono 808 that reads on small speakers: rage, hyperpop and aggressive \
		 trap.",
		&[
			("polyphony", 1.0),
			("legato", 1.0),
			("glide", 40.0),
			("amp_release", 120.0),
			("gain", -4.0),
		],
		&[keyboard("Rage 808 F.flac", 0, 72, F1)],
	)
	.category("808"),
	sampled(
		"Drill 808 Slide",
		"Long, dark mono 808 with a slow slide between overlapping notes: UK and NY drill bass \
		 lines.",
		&[
			("polyphony", 1.0),
			("legato", 1.0),
			("glide", 180.0),
			("amp_release", 250.0),
			("gain", -3.0),
		],
		&[keyboard("Drill 808 F.flac", 0, 72, F1)],
	)
	.category("808"),
	sampled(
		"Sine Sub Bass",
		"Pure, steady mono sub bass from the sine multisample with a quick glide: deep house, \
		 dubstep and any track that needs clean sub weight under a mid bass.",
		&[
			("polyphony", 1.0),
			("legato", 1.0),
			("glide", 30.0),
			("amp_attack", 3.0),
			("amp_release", 80.0),
			("filter_type", 1.0),
			("filter_cutoff", 800.0),
			("gain", -3.0),
		],
		SINE_ZONES,
	)
	.category("Bass"),
	sampled(
		"Sine Lead",
		"Soft, flute-like mono sine lead with a smooth glide: chill, lo-fi and ambient melodies.",
		&[
			("polyphony", 1.0),
			("legato", 1.0),
			("glide", 90.0),
			("amp_attack", 15.0),
			("amp_release", 250.0),
			("gain", -4.0),
		],
		SINE_ZONES,
	)
	.category("Lead"),
	sampled(
		"Soft Organ",
		"Gentle, sustained sine organ that starts and stops with the key: gospel pads, \
		 soul chords and simple church-style keys.",
		&[
			("amp_attack", 5.0),
			("amp_release", 120.0),
			("velocity_sensitivity", 30.0),
			("gain", -12.0),
		],
		SINE_ZONES,
	)
	.category("Keys"),
	sampled(
		"Muted Keys",
		"Dark, velocity-sensitive keys that fade to half level: lo-fi chords and soft \
		 background comping.",
		&[
			("amp_attack", 2.0),
			("amp_decay", 1_200.0),
			("amp_sustain", 50.0),
			("amp_release", 300.0),
			("velocity_sensitivity", 80.0),
			("filter_type", 1.0),
			("filter_cutoff", 1_500.0),
			("filter_resonance", 20.0),
			("filter_velocity", 30.0),
			("gain", -10.0),
		],
		SINE_ZONES,
	)
	.category("Keys"),
	sampled(
		"Slow Swell Pad",
		"Very slow, dark pad whose filter opens over two seconds: ambient beds, film \
		 intros and breakdowns.",
		&[
			("amp_attack", 1_500.0),
			("amp_release", 3_000.0),
			("filter_type", 1.0),
			("filter_cutoff", 600.0),
			("filter_env_amount", 30.0),
			("filter_attack", 2_000.0),
			("filter_decay", 2_000.0),
			("filter_sustain", 60.0),
			("filter_release", 3_000.0),
			("gain", -7.5),
		],
		SINE_ZONES,
	)
	.category("Pad"),
	sampled(
		"Glass Bell",
		"Pure, ringing sine bell an octave up with a long decay: music-box melodies, lullabies \
		 and dreamy pop toplines.",
		&[
			("transpose", 12.0),
			("amp_attack", 1.0),
			("amp_decay", 1_800.0),
			("amp_sustain", 0.0),
			("amp_release", 1_200.0),
			("filter_type", 2.0),
			("filter_cutoff", 200.0),
			("gain", -13.0),
		],
		SINE_ZONES,
	)
	.category("Bell"),
	sampled(
		"Reverse Swell FX",
		"A reversed crash cymbal played whole on every note (higher keys play it shorter and \
		 brighter): risers into drops, chorus entries and transitions. Hit middle C one bar \
		 before the downbeat.",
		&[("mode", 1.0), ("gain", -4.0)],
		&[keyboard("Vintage Crash.flac", 0, 127, 60).reversed()],
	)
	.category("FX"),
];

/// Pitch of the factory 808 samples (F1).
const F1: u8 = 29;

/// The sine multisample mapped every octave (C2-C5), as in "Sine Pad".
const SINE_ZONES: &[FactoryZone] = &[
	pad_loop(SINE_PAD[0], 0, 41, 36),
	pad_loop(SINE_PAD[1], 42, 53, 48),
	pad_loop(SINE_PAD[2], 54, 65, 60),
	pad_loop(SINE_PAD[3], 66, 127, 72),
];

/// One sample played chromatically over `low..=high`, at its own pitch on `root`.
const fn keyboard(sample: &'static str, low: u8, high: u8, root: u8) -> FactoryZone {
	let mut z = FactoryZone::pad(sample, root);
	z.key_low = low;
	z.key_high = high;
	z
}

pub(super) static DRUM_MACHINE: &[FactoryPreset] = &[
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
			("gain", -2.0),
		],
	)
	.category("Funk"),
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
	)
	.category("Trap"),
	preset(
		"House Kit",
		"Four-on-the-floor house kit: round punchy kick, bright clap, short closed and airy \
		 open hats.",
		&[
			("kick_decay", 380.0),
			("kick_punch", 65.0),
			("snare_decay", 180.0),
			("clap_level", -1.0),
			("hat_decay", 40.0),
			("open_hat_decay", 300.0),
			("tone", 60.0),
			("gain", -3.0),
		],
	)
	.category("House"),
	preset(
		"Deep House Kit",
		"Softer, warmer house kit with a longer kick and darker tone: deep house, lo-fi \
		 house.",
		&[
			("kick_decay", 480.0),
			("kick_punch", 50.0),
			("snare_decay", 200.0),
			("snare_snappy", 50.0),
			("clap_level", -2.0),
			("hat_decay", 50.0),
			("open_hat_decay", 380.0),
			("hat_level", -8.0),
			("tone", 40.0),
			("gain", -1.5),
		],
	)
	.category("House"),
	preset(
		"Techno Kit",
		"Driving techno kit: tuned-down hard kick, tight bright hats, quieter snare.",
		&[
			("kick_tune", -2.0),
			("kick_decay", 550.0),
			("kick_punch", 75.0),
			("snare_level", -6.0),
			("hat_decay", 30.0),
			("open_hat_decay", 200.0),
			("tone", 65.0),
			("gain", -3.0),
		],
	)
	.category("Techno"),
	preset(
		"Big Room Kit",
		"Huge, long, hard-hitting kick with loud clap for festival EDM drops.",
		&[
			("kick_tune", -1.0),
			("kick_decay", 700.0),
			("kick_punch", 95.0),
			("snare_level", -4.0),
			("clap_level", 0.0),
			("hat_decay", 35.0),
			("tone", 65.0),
			("gain", -3.0),
		],
	)
	.category("Dance"),
	preset(
		"Drill Kit",
		"Long sliding-style 808 kick, high tight snare and very short hats for drill and dark \
		 trap.",
		&[
			("kick_tune", -3.0),
			("kick_decay", 1800.0),
			("kick_punch", 50.0),
			("snare_tune", 3.0),
			("snare_decay", 140.0),
			("snare_snappy", 80.0),
			("hat_decay", 25.0),
			("tone", 55.0),
			("gain", -5.0),
		],
	)
	.category("Trap"),
	preset(
		"Boom Bap Kit",
		"Dusty, fat 90s hip-hop kit: thick kick, roomy snare, darker tone.",
		&[
			("kick_decay", 500.0),
			("kick_punch", 55.0),
			("snare_decay", 280.0),
			("snare_snappy", 50.0),
			("hat_decay", 70.0),
			("tone", 35.0),
			("gain", -1.5),
		],
	)
	.category("Hip-Hop"),
	preset(
		"Lo-Fi Kit",
		"Soft, muffled kit for lo-fi and chill beats: low punch, quiet hats, dark tone.",
		&[
			("kick_decay", 400.0),
			("kick_punch", 30.0),
			("snare_decay", 200.0),
			("snare_snappy", 40.0),
			("hat_decay", 50.0),
			("hat_level", -9.0),
			("perc_level", -6.0),
			("tone", 20.0),
		],
	)
	.category("Hip-Hop"),
	preset(
		"Reggaeton Kit",
		"Dembow-ready kit: punchy kick, crisp tuned-up snare, short hats and present \
		 percussion.",
		&[
			("kick_decay", 450.0),
			("kick_punch", 70.0),
			("snare_tune", 2.0),
			("snare_decay", 150.0),
			("snare_snappy", 70.0),
			("hat_decay", 40.0),
			("perc_level", -4.0),
			("tone", 55.0),
			("gain", -3.0),
		],
	)
	.category("Reggaeton"),
	preset(
		"Latin Percussion Kit",
		"Kit with the hand percussion up front (congas, bongos, timbales, cowbell, guiro): \
		 salsa, cumbia, Latin house.",
		&[
			("kick_level", -3.0),
			("snare_level", -4.0),
			("perc_level", 0.0),
			("perc_decay", 110.0),
			("hat_decay", 45.0),
			("tone", 55.0),
			("gain", -3.0),
		],
	)
	.category("Latin"),
	preset(
		"Afro Groove Kit",
		"Bright, bouncy kit with tuned-up percussion and short snare: afrobeats, amapiano, \
		 afro house.",
		&[
			("kick_decay", 350.0),
			("kick_punch", 60.0),
			("snare_decay", 140.0),
			("hat_decay", 35.0),
			("perc_tune", 2.0),
			("perc_level", -1.0),
			("tone", 55.0),
			("gain", -3.0),
		],
	)
	.category("Afro"),
	preset(
		"Disco Kit",
		"Tight, bright disco and nu-disco kit: short kick, open hats that sing on the \
		 offbeat.",
		&[
			("kick_decay", 300.0),
			("kick_punch", 65.0),
			("snare_decay", 200.0),
			("hat_decay", 50.0),
			("open_hat_decay", 350.0),
			("tone", 60.0),
			("gain", -3.0),
		],
	)
	.category("Funk"),
	preset(
		"Synth Pop Kit",
		"80s-style synth pop kit: long snare, big pitched-down toms and long cymbals.",
		&[
			("snare_decay", 350.0),
			("snare_snappy", 55.0),
			("tom_tune", -2.0),
			("tom_decay", 600.0),
			("cymbal_decay", 2500.0),
			("tone", 55.0),
			("gain", -3.0),
		],
	)
	.category("Pop"),
	preset(
		"Electro Kit",
		"Hard, zappy electro and breakdance kit: punchy tuned-up kick, snappy snare, short \
		 toms.",
		&[
			("kick_tune", 2.0),
			("kick_decay", 600.0),
			("kick_punch", 90.0),
			("snare_snappy", 85.0),
			("tom_decay", 300.0),
			("tone", 70.0),
			("gain", -5.0),
		],
	)
	.category("Retro"),
];

pub(super) static ELECTRIC_PIANO: &[FactoryPreset] = &[
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
	preset(
		"Neo-Soul Mellow",
		"Round, mellow tines with a soft attack for neo-soul, R&B and jazz chords.",
		&[
			("brightness", 25.0),
			("bell", 20.0),
			("decay", 6000.0),
			("release", 300.0),
			("velocity_sensitivity", 80.0),
		],
	),
	preset(
		"Ballad Shine",
		"Bright, glassy FM-style keys of 80s power ballads and city pop.",
		&[
			("brightness", 85.0),
			("bell", 70.0),
			("decay", 7000.0),
			("release", 500.0),
			("gain", -2.0),
		],
	),
	preset(
		"Suitcase Tremolo",
		"Warm keys with a medium stereo tremolo: soul, gospel and indie ballads.",
		&[
			("brightness", 45.0),
			("bell", 35.0),
			("tremolo_rate", 5.0),
			("tremolo_depth", 40.0),
		],
	),
	preset(
		"Gospel Bark",
		"Driven, barky tines that cut through a band: gospel, funk and blues comping.",
		&[
			("brightness", 75.0),
			("bell", 45.0),
			("decay", 4500.0),
			("velocity_sensitivity", 90.0),
			("gain", -1.0),
		],
	),
	preset(
		"Lo-Fi Dusty Keys",
		"Dark, soft keys with a slow gentle wobble for lo-fi hip-hop and chill beats.",
		&[
			("brightness", 20.0),
			("bell", 10.0),
			("decay", 3000.0),
			("release", 200.0),
			("velocity_sensitivity", 50.0),
			("tremolo_rate", 3.0),
			("tremolo_depth", 15.0),
		],
	),
	preset(
		"Glass Bell Keys",
		"Bell-heavy, sparkling tines with a long ring: intros, dreamy pop, chillwave.",
		&[
			("brightness", 55.0),
			("bell", 90.0),
			("decay", 8000.0),
			("release", 800.0),
			("gain", -2.0),
		],
	),
	preset(
		"Pedal Keys",
		"Long, sustained keys with a pedal-like release for slow ballads and ambient chords.",
		&[
			("brightness", 35.0),
			("bell", 30.0),
			("decay", 10000.0),
			("release", 2500.0),
		],
	),
	preset(
		"Funk Stabs",
		"Short, bright, punchy tines with a quick release for funk and disco stabs.",
		&[
			("brightness", 60.0),
			("bell", 40.0),
			("decay", 1500.0),
			("release", 60.0),
		],
	),
	preset(
		"Felt Soft",
		"Very soft, dark and even keys with little velocity response: background chords under \
		 vocals.",
		&[
			("brightness", 15.0),
			("bell", 5.0),
			("decay", 5000.0),
			("velocity_sensitivity", 40.0),
		],
	),
	preset(
		"Jazz Club",
		"Balanced, natural tine piano for jazz standards and smooth comping.",
		&[
			("brightness", 35.0),
			("bell", 30.0),
			("decay", 5000.0),
			("release", 250.0),
			("velocity_sensitivity", 75.0),
		],
	),
	preset(
		"Fast Tremolo Chop",
		"Deep, fast stereo tremolo that chops chords into pulses: psychedelic soul and indie.",
		&[
			("brightness", 45.0),
			("bell", 35.0),
			("tremolo_rate", 8.0),
			("tremolo_depth", 70.0),
		],
	),
];
