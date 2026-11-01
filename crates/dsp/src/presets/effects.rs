//! Factory presets of the built-in effects.

use super::{FactoryPreset, preset};

pub(super) static COMPRESSOR: &[FactoryPreset] = &[
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
	preset(
		"Gentle Leveler",
		"Soft-knee 2:1 that evens a voice, podcast or acoustic part without audible pumping.",
		&[
			("threshold", -24.0),
			("ratio", 2.0),
			("attack", 15.0),
			("release", 250.0),
			("knee", 12.0),
			("makeup", 3.0),
		],
	),
	preset(
		"Bass Control",
		"Steady bass level for any genre: 4:1, medium attack keeps the pluck, 3 dB makeup.",
		&[
			("threshold", -20.0),
			("ratio", 4.0),
			("attack", 10.0),
			("release", 120.0),
			("knee", 6.0),
			("makeup", 3.0),
		],
	),
	preset(
		"Kick Snap",
		"Slow attack and fast release that sharpen a kick's beater click for pop, rock and \
		 EDM.",
		&[
			("threshold", -16.0),
			("ratio", 4.0),
			("attack", 25.0),
			("release", 60.0),
			("knee", 3.0),
			("makeup", 2.0),
		],
	),
	preset(
		"Snare Body",
		"Fattens a snare or clap: fast-ish attack, quick release brings up the body and room.",
		&[
			("threshold", -22.0),
			("ratio", 5.0),
			("attack", 8.0),
			("release", 80.0),
			("knee", 3.0),
			("makeup", 4.0),
		],
	),
	preset(
		"Parallel Smash",
		"Heavily crushed parallel (New York) compression at 35% mix: thick, aggressive drums \
		 or vocals.",
		&[
			("threshold", -35.0),
			("ratio", 10.0),
			("attack", 1.0),
			("release", 80.0),
			("knee", 0.0),
			("makeup", 10.0),
			("mix", 35.0),
		],
	),
	preset(
		"Mix Bus Glue",
		"Gentle 2:1 for the mix bus or a group: slow attack, auto-ish release, key high-pass \
		 so the kick doesn't pump the mix.",
		&[
			("threshold", -14.0),
			("ratio", 2.0),
			("attack", 30.0),
			("release", 200.0),
			("knee", 6.0),
			("makeup", 1.5),
			("key_hp", 100.0),
		],
	),
	preset(
		"Kick Duck",
		"Sidechain duck that reacts to the kick only (key low-pass at 150 Hz): set_sidechain \
		 from a full drum bus, hats and snares won't trigger it.",
		&[
			("threshold", -28.0),
			("ratio", 8.0),
			("attack", 0.5),
			("release", 120.0),
			("knee", 3.0),
			("key_lp", 150.0),
		],
	),
	preset(
		"De-Esser",
		"Fast full-band compression keyed by 5 kHz and up: tames harsh S and T sounds on a \
		 vocal.",
		&[
			("threshold", -32.0),
			("ratio", 8.0),
			("attack", 0.5),
			("release", 60.0),
			("knee", 3.0),
			("key_hp", 5000.0),
		],
	),
	preset(
		"Peak Tamer",
		"Catches only stray peaks (8:1 above -8 dB, very fast): safety on a lively vocal, \
		 slap bass or percussion.",
		&[
			("threshold", -8.0),
			("ratio", 8.0),
			("attack", 0.3),
			("release", 50.0),
			("knee", 2.0),
		],
	),
	preset(
		"Smooth Keys",
		"Transparent soft-knee control for piano, keys and pads: slow attack, long release.",
		&[
			("threshold", -20.0),
			("ratio", 2.5),
			("attack", 20.0),
			("release", 250.0),
			("knee", 10.0),
			("makeup", 2.0),
		],
	),
	preset(
		"Rap Vocal Upfront",
		"Firm 4:1 with fast attack and release that keeps a hip-hop or trap vocal loud and in \
		 your face.",
		&[
			("threshold", -24.0),
			("ratio", 4.0),
			("attack", 2.0),
			("release", 70.0),
			("knee", 4.0),
			("makeup", 6.0),
		],
	),
	preset(
		"Lo-Fi Crush",
		"Brickwall-style squash with near-instant attack, half wet: pumping, gritty lo-fi and \
		 boom-bap drums.",
		&[
			("threshold", -40.0),
			("ratio", 20.0),
			("attack", 0.1),
			("release", 40.0),
			("knee", 0.0),
			("makeup", 12.0),
			("mix", 50.0),
		],
	),
];

pub(super) static REVERB: &[FactoryPreset] = &[
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
	preset(
		"Vocal Plate",
		"Bright, dense plate for lead vocals on a send (100% mix): 30 ms pre-delay and a \
		 light duck keep the words clear.",
		&[
			("size", 50.0),
			("decay", 1.8),
			("damping", 35.0),
			("pre_delay", 30.0),
			("low_cut", 250.0),
			("mix", 100.0),
			("duck", 4.0),
			("duck_release", 300.0),
		],
	),
	preset(
		"Snare Plate",
		"Short, splashy plate for snares and claps on a send (100% mix): pop, rock and 80s \
		 ballads.",
		&[
			("size", 45.0),
			("decay", 1.4),
			("damping", 30.0),
			("pre_delay", 10.0),
			("low_cut", 300.0),
			("mix", 100.0),
		],
	),
	preset(
		"Drum Room",
		"Tight, natural room as an insert at low mix: glues drums and gives dry samples a \
		 space.",
		&[
			("size", 20.0),
			("decay", 0.5),
			("damping", 50.0),
			("pre_delay", 0.0),
			("low_cut", 200.0),
			("mix", 15.0),
		],
	),
	preset(
		"Ducked Vocal Hall",
		"Lush hall that ducks 10 dB while the vocal sings and blooms in the gaps (send, 100% \
		 mix): modern pop and R&B.",
		&[
			("size", 70.0),
			("decay", 2.8),
			("damping", 50.0),
			("pre_delay", 40.0),
			("low_cut", 250.0),
			("mix", 100.0),
			("duck", 10.0),
			("duck_release", 400.0),
		],
	),
	preset(
		"Pumping Reverb",
		"Big hall ducked hard and released fast: key it from the kick (set_sidechain) for a \
		 breathing EDM tail.",
		&[
			("size", 60.0),
			("decay", 2.5),
			("damping", 45.0),
			("pre_delay", 10.0),
			("low_cut", 300.0),
			("mix", 100.0),
			("duck", 18.0),
			("duck_release", 150.0),
		],
	),
	preset(
		"Ambient Wash",
		"Huge, dark 12-second space for ambient pads, drones and cinematic swells (send, 100% \
		 mix).",
		&[
			("size", 100.0),
			("decay", 12.0),
			("damping", 55.0),
			("pre_delay", 60.0),
			("low_cut", 250.0),
			("mix", 100.0),
		],
	),
	preset(
		"Dark Chamber",
		"Warm, heavily damped chamber that sits behind the mix: soul, jazz and vintage vocals \
		 (send).",
		&[
			("size", 35.0),
			("decay", 1.2),
			("damping", 80.0),
			("pre_delay", 15.0),
			("low_cut", 180.0),
			("mix", 100.0),
		],
	),
	preset(
		"Bright Studio",
		"Short bright space as an insert that adds air to acoustic guitar, piano or backing \
		 vocals.",
		&[
			("size", 30.0),
			("decay", 0.9),
			("damping", 25.0),
			("pre_delay", 12.0),
			("low_cut", 220.0),
			("mix", 18.0),
		],
	),
	preset(
		"Lo-Fi Mono Room",
		"Narrow, dark mono room for lo-fi hip-hop and vintage textures (insert).",
		&[
			("size", 25.0),
			("decay", 0.8),
			("damping", 85.0),
			("pre_delay", 5.0),
			("width", 0.0),
			("low_cut", 200.0),
			("mix", 25.0),
		],
	),
	preset(
		"Wide Pad Space",
		"Long, wide hall as an insert at 40% for pads and strings that need depth without a \
		 send.",
		&[
			("size", 85.0),
			("decay", 6.0),
			("damping", 40.0),
			("pre_delay", 20.0),
			("low_cut", 350.0),
			("mix", 40.0),
		],
	),
	preset(
		"Infinite Space",
		"20-second near-endless tail for drones and transitions; automate freeze for a held \
		 swell (send).",
		&[
			("size", 100.0),
			("decay", 20.0),
			("damping", 60.0),
			("pre_delay", 30.0),
			("low_cut", 300.0),
			("mix", 100.0),
		],
	),
];

pub(super) static PITCH_CORRECT: &[FactoryPreset] = &[
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
	preset(
		"Soft Backing",
		"Very slow, partial pull for stacked backing vocals and choirs: keeps the natural \
		 blend.",
		&[("retune", 200.0), ("humanize", 60.0), ("amount", 80.0)],
	),
	preset(
		"Trap Tune",
		"Fast, near-instant snapping with a hint of glide: the melodic rap and trap vocal \
		 sound.",
		&[("retune", 5.0), ("humanize", 5.0), ("amount", 100.0)],
	),
	preset(
		"Octave Down",
		"Retunes the voice an octave lower (deep doubled or pitched-down vocal effect).",
		&[
			("retune", 30.0),
			("humanize", 30.0),
			("amount", 100.0),
			("transpose", -12.0),
		],
	),
	preset(
		"Octave Up",
		"Retunes the voice an octave higher: chipmunk-style hooks and vocal chops.",
		&[
			("retune", 30.0),
			("humanize", 30.0),
			("amount", 100.0),
			("transpose", 12.0),
		],
	),
	preset(
		"MIDI Melody",
		"Hard-tunes to the notes of a melody track (set_midi_input) only while they are held: \
		 a vocoder-like played melody.",
		&[
			("retune", 0.0),
			("humanize", 0.0),
			("amount", 100.0),
			("midi", 2.0),
		],
	),
	preset(
		"Baroque Pitch",
		"Natural tuning against A = 415 Hz, for vocals over baroque or period-instrument \
		 recordings.",
		&[
			("retune", 50.0),
			("humanize", 40.0),
			("amount", 100.0),
			("reference", 415.0),
		],
	),
];

pub(super) static EQ: &[FactoryPreset] = &[
	preset(
		"Vocal Clarity",
		"Lead vocal: cleans rumble, cuts boxy 300 Hz, adds presence at 3 kHz and air at 10 \
		 kHz.",
		&[
			("hp_freq", 90.0),
			("hp_slope", 2.0),
			("peak1_freq", 300.0),
			("peak1_gain", -2.5),
			("peak1_q", 1.2),
			("peak2_freq", 3000.0),
			("peak2_gain", 2.0),
			("peak2_q", 0.9),
			("high_shelf_freq", 10000.0),
			("high_shelf_gain", 2.5),
		],
	),
	preset(
		"Kick Punch",
		"Kick drum: low thump at 60 Hz, scooped boxiness at 350 Hz, beater click at 4 kHz.",
		&[
			("hp_freq", 30.0),
			("hp_slope", 1.0),
			("peak1_freq", 60.0),
			("peak1_gain", 3.0),
			("peak1_q", 1.0),
			("peak2_freq", 350.0),
			("peak2_gain", -4.0),
			("peak2_q", 1.5),
			("peak3_freq", 4000.0),
			("peak3_gain", 3.0),
			("peak3_q", 1.2),
		],
	),
	preset(
		"Snare Crack",
		"Snare or clap: body at 200 Hz, less ring at 900 Hz, crack at 5 kHz.",
		&[
			("hp_freq", 80.0),
			("hp_slope", 2.0),
			("peak1_freq", 200.0),
			("peak1_gain", 2.0),
			("peak1_q", 1.2),
			("peak2_freq", 900.0),
			("peak2_gain", -2.0),
			("peak2_q", 1.5),
			("peak3_freq", 5000.0),
			("peak3_gain", 3.0),
			("peak3_q", 1.0),
		],
	),
	preset(
		"Bass Definition",
		"Bass guitar or synth bass: tight sub, less mud at 250 Hz, growl at 800 Hz, top \
		 rolled off.",
		&[
			("hp_freq", 35.0),
			("hp_slope", 2.0),
			("low_shelf_freq", 80.0),
			("low_shelf_gain", 1.5),
			("peak1_freq", 250.0),
			("peak1_gain", -3.0),
			("peak1_q", 1.0),
			("peak2_freq", 800.0),
			("peak2_gain", 2.0),
			("peak2_q", 1.2),
			("lp_freq", 8000.0),
			("lp_slope", 1.0),
		],
	),
	preset(
		"Acoustic Guitar",
		"Strummed or picked acoustic: low cut, less boom at 200 Hz, sparkle on top.",
		&[
			("hp_freq", 100.0),
			("hp_slope", 2.0),
			("peak1_freq", 200.0),
			("peak1_gain", -3.0),
			("peak1_q", 1.0),
			("peak2_freq", 2500.0),
			("peak2_gain", 1.5),
			("peak2_q", 1.0),
			("high_shelf_freq", 10000.0),
			("high_shelf_gain", 2.0),
		],
	),
	preset(
		"Electric Guitar Focus",
		"Distorted or clean electric guitar: band-limited to its range with mid bite at 2.5 \
		 kHz.",
		&[
			("hp_freq", 90.0),
			("hp_slope", 2.0),
			("peak1_freq", 400.0),
			("peak1_gain", -2.0),
			("peak1_q", 1.0),
			("peak2_freq", 2500.0),
			("peak2_gain", 2.0),
			("peak2_q", 1.0),
			("lp_freq", 9000.0),
			("lp_slope", 1.0),
		],
	),
	preset(
		"Piano Clarity",
		"Piano and keys: low cut, less mud at 300 Hz, gentle top shelf.",
		&[
			("hp_freq", 50.0),
			("hp_slope", 1.0),
			("peak1_freq", 300.0),
			("peak1_gain", -2.0),
			("peak1_q", 0.9),
			("high_shelf_freq", 8000.0),
			("high_shelf_gain", 1.5),
		],
	),
	preset(
		"Pad Make Room",
		"Pads and synth beds: steep low cut and a broad dip at 2.5 kHz that leaves room for \
		 the vocal.",
		&[
			("hp_freq", 200.0),
			("hp_slope", 2.0),
			("peak2_freq", 2500.0),
			("peak2_gain", -3.0),
			("peak2_q", 0.8),
			("lp_freq", 12000.0),
			("lp_slope", 1.0),
		],
	),
	preset(
		"Drum Bus Polish",
		"Drum bus: more weight at 80 Hz, less boxiness at 400 Hz, crisp top.",
		&[
			("hp_freq", 30.0),
			("hp_slope", 1.0),
			("low_shelf_freq", 80.0),
			("low_shelf_gain", 2.0),
			("peak1_freq", 400.0),
			("peak1_gain", -2.0),
			("peak1_q", 0.8),
			("high_shelf_freq", 10000.0),
			("high_shelf_gain", 2.0),
		],
	),
	preset(
		"Master Air",
		"Subtle master or mix bus shaping: infrasonic cut, slight low-mid dip, a touch of \
		 air.",
		&[
			("hp_freq", 25.0),
			("hp_slope", 1.0),
			("peak1_freq", 300.0),
			("peak1_gain", -1.0),
			("peak1_q", 0.7),
			("high_shelf_freq", 12000.0),
			("high_shelf_gain", 1.5),
		],
	),
	preset(
		"Mono Low End",
		"High-pass on the side channel below 150 Hz: a mono, club-safe low end on a bus or \
		 the master.",
		&[("hp_freq", 150.0), ("hp_slope", 2.0), ("hp_ms", 2.0)],
	),
	preset(
		"Side Air Width",
		"High shelf on the side channel only: wider, airier stereo image without touching the \
		 center.",
		&[
			("high_shelf_freq", 8000.0),
			("high_shelf_gain", 2.5),
			("high_shelf_ms", 2.0),
		],
	),
	preset(
		"Dynamic De-Ess",
		"Dynamic cut at 6.5 kHz that only acts when sibilance gets loud: vocals, cymbals.",
		&[
			("peak3_freq", 6500.0),
			("peak3_gain", -8.0),
			("peak3_q", 2.0),
			("peak3_dyn", 1.0),
			("peak3_threshold", -30.0),
			("peak3_ratio", 4.0),
			("dyn_attack", 1.0),
			("dyn_release", 80.0),
		],
	),
	preset(
		"Dynamic Mud Control",
		"Dynamic dip at 250 Hz that only cuts when the low mids build up: full mixes, \
		 guitars, vocals near the mic.",
		&[
			("peak1_freq", 250.0),
			("peak1_gain", -5.0),
			("peak1_q", 1.2),
			("peak1_dyn", 1.0),
			("peak1_threshold", -28.0),
			("peak1_ratio", 3.0),
			("dyn_attack", 10.0),
			("dyn_release", 150.0),
		],
	),
	preset(
		"Telephone",
		"Narrow, band-limited midrange of a phone or old radio: intros, vocal effects, \
		 breakdowns.",
		&[
			("hp_freq", 400.0),
			("hp_slope", 3.0),
			("peak2_freq", 1500.0),
			("peak2_gain", 4.0),
			("peak2_q", 0.8),
			("lp_freq", 3500.0),
			("lp_slope", 3.0),
		],
	),
	preset(
		"Lo-Fi Dull",
		"Dark, rolled-off tone of a worn sample or cassette: lo-fi hip-hop and chill beats.",
		&[
			("hp_freq", 120.0),
			("hp_slope", 1.0),
			("peak1_freq", 900.0),
			("peak1_gain", 2.0),
			("peak1_q", 0.7),
			("lp_freq", 4000.0),
			("lp_slope", 2.0),
		],
	),
];

pub(super) static LIMITER: &[FactoryPreset] = &[
	preset(
		"Master Streaming",
		"Master limiter for streaming: -1 dB ceiling, 4x oversampling, moderate 3 dB push.",
		&[
			("input_gain", 3.0),
			("ceiling", -1.0),
			("release", 120.0),
			("oversample", 2.0),
		],
	),
	preset(
		"Master Transparent",
		"Barely-there master limiting that only catches peaks: acoustic, jazz, classical.",
		&[
			("input_gain", 1.5),
			("ceiling", -1.0),
			("release", 200.0),
			("oversample", 2.0),
		],
	),
	preset(
		"Master Loud",
		"Loud modern master for pop, EDM and hip-hop: 6 dB of drive, fast release.",
		&[
			("input_gain", 6.0),
			("ceiling", -1.0),
			("release", 60.0),
			("oversample", 2.0),
		],
	),
	preset(
		"Club Master",
		"Very loud, dense master for club music; transients get flattened, check the low end.",
		&[
			("input_gain", 9.0),
			("ceiling", -1.0),
			("release", 40.0),
			("oversample", 2.0),
		],
	),
	preset(
		"Peak Safety",
		"No drive, ceiling just under 0 dB: a safety net on a bus or a live input.",
		&[("input_gain", 0.0), ("ceiling", -0.3), ("release", 100.0)],
	),
	preset(
		"Drum Bus Punch",
		"Drives a drum bus 4 dB into a fast limiter for density and punch.",
		&[
			("input_gain", 4.0),
			("ceiling", -3.0),
			("release", 30.0),
			("oversample", 1.0),
		],
	),
	preset(
		"Vocal Catch",
		"Holds a vocal track under -6 dB after compression: no peak pokes out of the mix.",
		&[("input_gain", 2.0), ("ceiling", -6.0), ("release", 80.0)],
	),
	preset(
		"Broadcast Voice",
		"Dense, even voice-over or podcast level: 6 dB push, -2 dB ceiling, slow release.",
		&[
			("input_gain", 6.0),
			("ceiling", -2.0),
			("release", 150.0),
			("oversample", 1.0),
		],
	),
];

pub(super) static DELAY: &[FactoryPreset] = &[
	preset(
		"Eighth Echo",
		"Tempo-synced 1/8 echo, filtered and low in the mix: keys, plucks and guitars.",
		&[
			("sync", 6.0),
			("feedback", 30.0),
			("high_cut", 6000.0),
			("low_cut", 150.0),
			("mix", 20.0),
		],
	),
	preset(
		"Dotted Eighth Lead",
		"Dotted 1/8 repeats that fill the groove between notes: leads and clean guitars.",
		&[
			("sync", 7.0),
			("feedback", 40.0),
			("high_cut", 5000.0),
			("low_cut", 200.0),
			("mix", 25.0),
		],
	),
	preset(
		"Ping-Pong Quarter",
		"1/4 echoes bouncing left and right: wide synths, vocal ad-libs.",
		&[
			("sync", 9.0),
			("feedback", 35.0),
			("ping_pong", 1.0),
			("high_cut", 7000.0),
			("low_cut", 150.0),
			("mix", 25.0),
		],
	),
	preset(
		"Triplet Bounce",
		"1/8 triplet ping-pong that adds swing to straight parts: pop, house, afro.",
		&[
			("sync", 5.0),
			("feedback", 35.0),
			("ping_pong", 1.0),
			("high_cut", 6500.0),
			("low_cut", 200.0),
			("mix", 22.0),
		],
	),
	preset(
		"Slapback",
		"Single short 110 ms slap: rockabilly and vintage vocals, guitars, snares.",
		&[
			("sync", 0.0),
			("time", 110.0),
			("feedback", 5.0),
			("high_cut", 4500.0),
			("low_cut", 150.0),
			("mix", 22.0),
		],
	),
	preset(
		"Vocal Throw",
		"Dark 1/4 echoes for a send (100% mix): automate the send to throw the last word of a \
		 phrase.",
		&[
			("sync", 9.0),
			("feedback", 45.0),
			("high_cut", 5000.0),
			("low_cut", 300.0),
			("mix", 100.0),
		],
	),
	preset(
		"Dub Echo",
		"Dotted 1/4 repeats with high feedback, dark and thin: dub, reggae and lo-fi.",
		&[
			("sync", 10.0),
			("feedback", 70.0),
			("high_cut", 2500.0),
			("low_cut", 250.0),
			("mix", 30.0),
		],
	),
	preset(
		"Tape Echo",
		"Free-running 320 ms echo with dark, decaying repeats like a worn tape unit.",
		&[
			("sync", 0.0),
			("time", 320.0),
			("feedback", 55.0),
			("high_cut", 2200.0),
			("low_cut", 300.0),
			("mix", 30.0),
		],
	),
	preset(
		"Sixteenth Shimmer",
		"Fast, bright 1/16 repeats at low level: movement on arps, hats and plucks.",
		&[
			("sync", 3.0),
			("feedback", 25.0),
			("high_cut", 9000.0),
			("low_cut", 300.0),
			("mix", 15.0),
		],
	),
	preset(
		"Long Ambient Echo",
		"Whole-bar ping-pong with long feedback for pads and ambient sends (100% mix).",
		&[
			("sync", 12.0),
			("feedback", 60.0),
			("ping_pong", 1.0),
			("high_cut", 3000.0),
			("low_cut", 250.0),
			("mix", 100.0),
		],
	),
	preset(
		"Multi-Tap Rhythm",
		"Four 1/8 taps panned left-right-left: a rhythmic stereo pattern from a single hit.",
		&[
			("sync", 6.0),
			("feedback", 20.0),
			("high_cut", 6000.0),
			("low_cut", 200.0),
			("mix", 25.0),
			("taps", 4.0),
			("tap_spread", 100.0),
			("tap2_level", 70.0),
			("tap3_level", 50.0),
			("tap4_level", 35.0),
			("tap2_pan", -0.7),
			("tap3_pan", 0.7),
			("tap4_pan", -0.7),
		],
	),
	preset(
		"Wide Doubler",
		"Two very short taps panned apart with no feedback: a fake double-tracked vocal or \
		 guitar.",
		&[
			("sync", 0.0),
			("time", 18.0),
			("feedback", 0.0),
			("high_cut", 12000.0),
			("low_cut", 150.0),
			("mix", 35.0),
			("taps", 2.0),
			("tap_spread", 60.0),
			("tap2_level", 90.0),
			("tap2_pan", -0.8),
		],
	),
];

pub(super) static CHORUS: &[FactoryPreset] = &[
	preset(
		"Classic Chorus",
		"Even, medium-speed chorus: clean guitars, electric pianos and synths.",
		&[
			("rate", 0.8),
			("depth", 45.0),
			("delay", 12.0),
			("feedback", 0.0),
			("width", 100.0),
			("mix", 50.0),
		],
	),
	preset(
		"Lush Pad Chorus",
		"Slow, deep, wide chorus that thickens pads and strings.",
		&[
			("rate", 0.3),
			("depth", 70.0),
			("delay", 18.0),
			("feedback", 0.0),
			("width", 100.0),
			("mix", 50.0),
		],
	),
	preset(
		"Subtle Widen",
		"Gentle, low-mix chorus that widens a part without obvious wobble: vocals, keys.",
		&[
			("rate", 0.5),
			("depth", 20.0),
			("delay", 10.0),
			("feedback", 0.0),
			("width", 100.0),
			("mix", 30.0),
		],
	),
	preset(
		"Ensemble",
		"Faster, shallow ensemble shimmer for string machines and vintage polysynths.",
		&[
			("rate", 1.2),
			("depth", 35.0),
			("delay", 8.0),
			("feedback", 0.0),
			("width", 100.0),
			("mix", 55.0),
		],
	),
	preset(
		"Clean Guitar Shimmer",
		"Bright, swirling chorus for clean electric guitar arpeggios: 80s pop, indie, funk.",
		&[
			("rate", 1.0),
			("depth", 50.0),
			("delay", 7.0),
			("feedback", 0.0),
			("width", 100.0),
			("mix", 45.0),
		],
	),
	preset(
		"Bass Chorus",
		"Narrow chorus that keeps the low end centered: fretless and synth bass movement.",
		&[
			("rate", 0.6),
			("depth", 30.0),
			("delay", 9.0),
			("feedback", 0.0),
			("width", 40.0),
			("mix", 35.0),
		],
	),
	preset(
		"Jet Flanger",
		"Slow, resonant jet sweep: drums in a breakdown, guitars, transitions.",
		&[
			("rate", 0.15),
			("depth", 80.0),
			("delay", 2.0),
			("feedback", 70.0),
			("width", 100.0),
			("mix", 50.0),
		],
	),
	preset(
		"Metallic Flanger",
		"Strongly resonant, faster flanger for metallic, robotic textures.",
		&[
			("rate", 0.5),
			("depth", 60.0),
			("delay", 1.5),
			("feedback", 85.0),
			("width", 100.0),
			("mix", 50.0),
		],
	),
	preset(
		"Vibrato",
		"Pure pitch vibrato (100% wet, mono): organ vibrato, warbly leads.",
		&[
			("rate", 5.0),
			("depth", 35.0),
			("delay", 5.0),
			("feedback", 0.0),
			("width", 0.0),
			("mix", 100.0),
		],
	),
	preset(
		"Rotary Speaker",
		"Fast, wide swirl reminiscent of a spinning speaker cabinet: organs and electric \
		 pianos.",
		&[
			("rate", 5.0),
			("depth", 40.0),
			("delay", 4.0),
			("feedback", 0.0),
			("width", 100.0),
			("mix", 60.0),
		],
	),
	preset(
		"Tape Warble",
		"Slow, deep, fully wet pitch drift like a stretched tape: lo-fi and chill textures.",
		&[
			("rate", 0.4),
			("depth", 90.0),
			("delay", 20.0),
			("feedback", 0.0),
			("width", 30.0),
			("mix", 100.0),
		],
	),
];

pub(super) static SATURATOR: &[FactoryPreset] = &[
	preset(
		"Tape Warmth",
		"Subtle tape-style saturation with a slightly darker top: buses, master, anything too \
		 clean.",
		&[
			("drive", 4.0),
			("mode", 0.0),
			("tone", -15.0),
			("mix", 100.0),
		],
	),
	preset(
		"Tube Glow",
		"Warm even-harmonic tube color: vocals, keys, bass.",
		&[("drive", 6.0), ("mode", 1.0), ("tone", 0.0), ("mix", 100.0)],
	),
	preset(
		"Master Glue",
		"Barely audible tape saturation for the master or mix bus: density without \
		 distortion.",
		&[("drive", 2.0), ("mode", 0.0), ("tone", 0.0), ("mix", 100.0)],
	),
	preset(
		"Bass Grit",
		"Tube drive blended at 60% so a bass line cuts on small speakers while the sub stays \
		 clean.",
		&[
			("drive", 12.0),
			("mode", 1.0),
			("tone", 10.0),
			("mix", 60.0),
			("output", -1.0),
		],
	),
	preset(
		"808 Saturation",
		"Tape drive that adds harmonics to an 808 or sub so it is heard on phones and \
		 laptops.",
		&[
			("drive", 14.0),
			("mode", 0.0),
			("tone", 0.0),
			("mix", 100.0),
			("output", -1.0),
		],
	),
	preset(
		"Drum Crunch",
		"Parallel tape crunch at 40%: adds weight and grit to a drum bus while transients \
		 survive.",
		&[
			("drive", 15.0),
			("mode", 0.0),
			("tone", 20.0),
			("mix", 40.0),
		],
	),
	preset(
		"Vocal Edge",
		"Parallel tube saturation with a bright tilt: rap, rock and pop vocals that need \
		 bite.",
		&[("drive", 8.0), ("mode", 1.0), ("tone", 25.0), ("mix", 30.0)],
	),
	preset(
		"Parallel Harmonics",
		"Moderate tube drive at 25%: richer harmonics with the dry dynamics intact.",
		&[("drive", 10.0), ("mode", 1.0), ("tone", 0.0), ("mix", 25.0)],
	),
	preset(
		"Transient Clipper",
		"Light hard clipping that shaves drum and percussion peaks for loudness.",
		&[("drive", 4.0), ("mode", 2.0), ("tone", 0.0), ("mix", 100.0)],
	),
	preset(
		"Hard Clip Fuzz",
		"Aggressive, dark hard-clip distortion: fuzz guitars, industrial leads, distorted \
		 drums.",
		&[
			("drive", 24.0),
			("mode", 2.0),
			("tone", -20.0),
			("mix", 100.0),
			("output", -3.0),
		],
	),
	preset(
		"Lo-Fi Dirt",
		"Dark, gritty hard clipping blended in: lo-fi beats, broken-radio textures.",
		&[
			("drive", 18.0),
			("mode", 2.0),
			("tone", -60.0),
			("mix", 70.0),
			("output", -2.0),
		],
	),
];

pub(super) static GAIN: &[FactoryPreset] = &[
	preset(
		"Trim -6 dB",
		"Lowers the level by 6 dB: gain staging before a plugin or a hot track.",
		&[("gain", -6.0)],
	),
	preset(
		"Pad -12 dB",
		"Lowers the level by 12 dB: tames a very hot source or a loud instrument.",
		&[("gain", -12.0)],
	),
	preset(
		"Boost +6 dB",
		"Raises a quiet track or a low-level recording by 6 dB.",
		&[("gain", 6.0)],
	),
	preset(
		"Mono",
		"Folds the stereo image to mono: check mono compatibility, bass, kick or a mono \
		 source in a stereo file.",
		&[("width", 0.0)],
	),
	preset(
		"Narrow",
		"Narrows the stereo image to 60% so a busy part sits nearer the center.",
		&[("width", 60.0)],
	),
	preset(
		"Wider",
		"Widens the stereo image to 130% (mid/side): pads, backing vocals, reverb returns.",
		&[("width", 130.0)],
	),
	preset(
		"Extra Wide",
		"Very wide 170% image; use sparingly and check mono, it weakens the center.",
		&[("width", 170.0)],
	),
	preset(
		"Polarity Flip",
		"Flips the polarity of both channels: fixes a mic or layer out of phase with another.",
		&[("invert_left", 1.0), ("invert_right", 1.0)],
	),
	preset(
		"Flip Left",
		"Flips the left channel's polarity only: repairs a stereo recording with one channel \
		 out of phase.",
		&[("invert_left", 1.0)],
	),
	preset(
		"Hard Left",
		"Pans the track fully left: double-tracked guitars, call-and-response parts.",
		&[("pan", -100.0)],
	),
	preset(
		"Hard Right",
		"Pans the track fully right: double-tracked guitars, call-and-response parts.",
		&[("pan", 100.0)],
	),
];
