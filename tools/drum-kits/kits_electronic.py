"""Electronic genre kits made of drum-lab samples."""

from kitgen import kit, rr

KITS = [
    # --- House ----------------------------------------------------------------------
    kit("Deep House Kit", "House",
        "Round, low 909 kick, soft clap and rim, short offbeat 808 open hat, congas and a maracas bed: "
        "dusty deep house at 118-124 BPM. Swing the hats 55-60%.",
        kick="909 Kick Deep", kick2=("808 Kick", {"gain": -3.0}), snare=("909 Snare Tight", {"gain": -7.0}),
        clap=("808 Clap", {"gain": -4.0}), rim="808 Rim", closed_hat="808 Closed Hat",
        open_hat=("808 Open Hat", {"decay": 220.0}), pedal_hat=("808 Closed Hat", {"decay": 30.0}),
        ride="909 Ride", shaker=("808 Maracas", {"gain": -10.0}), mute_conga="808 Conga Hi",
        open_conga="808 Conga Mid", low_conga="808 Conga Lo", crash="808 Cymbal", cowbell="808 Cowbell"),
    kit("Chicago Jack Kit", "House",
        "The jacking 909 set: punchy kick, clap and snare, ringing open hat and ride, 909 toms, with 707 "
        "kick and clap alternates and an 808 cowbell. Classic Chicago and garage house at 120-126 BPM.",
        kick="909 Kick", kick2="707 Kick", snare="909 Snare", snare2="707 Snare", clap="909 Clap",
        rim="909 Rim", closed_hat="909 Closed Hat", open_hat="909 Open Hat", pedal_hat=("909 Closed Hat", {"decay": 35.0}),
        ride="909 Ride", crash="909 Crash", low_tom="909 Tom Lo", mid_tom="909 Tom Mid", high_tom="909 Tom Hi",
        tambourine="Synth Tambourine", cowbell="808 Cowbell"),
    kit("Tech House Kit", "House",
        "Tight, hard 909 kick with an 808 sub layer on Kick 2, short 707 clap, snappy hats, rim and "
        "maracas for the rolling sixteenths, a woody clave and a metallic clank for the groove. 124-128 BPM.",
        kick="909 Kick Hard", kick2=("808 Kick Long", {"gain": -6.0, "decay": 400.0}), snare=("909 Snare Tight", {"gain": -5.0}),
        clap="707 Clap", rim="909 Rim", closed_hat=("909 Closed Hat", {"decay": 45.0}), open_hat=("909 Open Hat", {"decay": 160.0}),
        pedal_hat="808 Closed Hat", ride="909 Ride", shaker="808 Maracas", claves="808 Clave",
        cowbell=("Metal Clank", {"gain": -12.0}), crash="909 Crash", tambourine="Synth Tambourine"),
    kit("Filter House Kit", "House",
        "French filter-disco flavor: a crunchy 12-bit kick, a smashed clap, phased open hats and gritty "
        "machine snares. Loop it under a filtered disco sample at 120-126 BPM and sidechain the music to the kick.",
        kick="French Kick", kick2="707 Kick", snare=("707 Snare", {"gain": -4.0}), clap="French Clap",
        rim="909 Rim", closed_hat="909 Closed Hat", open_hat="Phaser Hat", pedal_hat=("909 Closed Hat", {"decay": 30.0}),
        ride="909 Ride", crash="909 Crash", tambourine="Synth Tambourine", cowbell="808 Cowbell"),
    # --- Techno ---------------------------------------------------------------------
    kit("Berlin Techno Kit", "Techno",
        "Driving warehouse techno: a 909 kick with a dark reverb rumble baked in, plain 909 kick on Kick 2, "
        "clap, rim, open hat and ride for the offbeats, and a crash for the drops. 128-138 BPM.",
        kick="Rumble Kick", kick2="909 Kick", snare="909 Snare", clap="909 Clap", rim="909 Rim",
        closed_hat="909 Closed Hat", open_hat="909 Open Hat", pedal_hat="808 Closed Hat", ride="909 Ride",
        crash="909 Crash", low_tom="909 Tom Lo", mid_tom="909 Tom Mid", high_tom="909 Tom Hi",
        cowbell=("Metal Clank", {"gain": -12.0}), claves="808 Clave"),
    kit("Industrial Techno Kit", "Techno",
        "Distorted, bit-crushed kick, a noisy snare, crushed hats and metal clanks: hard and industrial "
        "techno at 135-150 BPM. Push it through the saturator for more grit.",
        kick="Industrial Kick", kick2="Rumble Kick", snare="Noise Snare", clap=("French Clap", {"gain": -4.0}),
        rim="Metal Clank", closed_hat="Crushed Hat", open_hat="909 Open Hat", pedal_hat=("Crushed Hat", {"decay": 25.0}),
        ride="909 Ride", crash="909 Crash", low_tom="909 Tom Lo", mid_tom="909 Tom Mid", high_tom="909 Tom Hi",
        cowbell=("Metal Clank", {"gain": -10.0, "tune": -5.0})),
    kit("Minimal Click Kit", "Techno",
        "Dry, small sounds for minimal techno and microhouse: short CR-78 kick and hats, rim and clave "
        "clicks, a soft 707 clap and a guiro scrape. Leave space; 122-128 BPM.",
        kick="CR78 Kick", kick2="707 Kick", snare="CR78 Snare", clap=("707 Clap", {"gain": -5.0}), rim="808 Rim",
        closed_hat="CR78 Hat", open_hat=("808 Open Hat", {"decay": 150.0}), pedal_hat=("CR78 Hat", {"decay": 20.0}),
        claves="808 Clave", guiro="CR78 Guiro", cowbell="808 Cowbell", shaker="808 Maracas",
        mute_conga="808 Conga Hi", low_conga="808 Conga Lo"),
    # --- Dance ----------------------------------------------------------------------
    kit("Big Room Kit", "Dance",
        "Festival big room: a layered, saturated kick with a bright click, a wide clap-snare, 909 hats, "
        "crash and a laser zap. Tune the kick pad to the drop's key; 126-130 BPM.",
        kick="Big Room Kick", kick2="909 Kick Hard", snare=("909 Snare", {"gain": -4.0}), clap="Future Clap",
        rim="909 Rim", closed_hat="909 Closed Hat", open_hat="909 Open Hat", pedal_hat=("909 Closed Hat", {"decay": 30.0}),
        ride="909 Ride", crash=("909 Crash", {"gain": -7.0}), low_tom="909 Tom Lo", mid_tom="909 Tom Mid",
        high_tom="909 Tom Hi", tambourine="Synth Tambourine", cowbell=("Laser Zap", {"gain": -10.0})),
    kit("Future House Kit", "Dance",
        "Bouncy future house: hard, short kick, clap layered with a snare and room, finger snap, crisp "
        "hats and a metallic perc. 124-128 BPM with a bassline that ducks under the kick.",
        kick="909 Kick Hard", kick2="707 Kick", snare="909 Snare Tight", clap="Future Clap", snare2="Finger Snap",
        rim="909 Rim", closed_hat=("909 Closed Hat", {"decay": 40.0}), open_hat=("909 Open Hat", {"decay": 200.0}),
        pedal_hat="808 Closed Hat", ride="909 Ride", crash="909 Crash", tambourine="Synth Tambourine",
        cowbell=("Metal Clank", {"gain": -11.0, "tune": 5.0}), claves="808 Clave"),
    kit("Eurodance Kit", "Dance",
        "90s eurodance and hands-up: 909 kick tuned up with a short decay, short open hat on every "
        "offbeat, a big clap panned left, rolling tambourine and 909 crash. 130-140 BPM.",
        kick=("909 Kick", {"tune": 2.0, "decay": 180.0}), kick2="909 Kick Hard", snare="909 Snare",
        clap=("909 Clap", {"pan": -0.17}), rim="909 Rim", closed_hat="909 Closed Hat",
        open_hat=("909 Open Hat", {"decay": 140.0}), pedal_hat="808 Closed Hat", ride="909 Ride",
        crash="909 Crash", low_tom="909 Tom Lo", mid_tom="909 Tom Mid", high_tom="909 Tom Hi",
        tambourine="Synth Tambourine", cowbell="808 Cowbell"),
    # --- Trap -----------------------------------------------------------------------
    kit("Atlanta Trap Kit", "Trap",
        "Classic trap: a long, saturated 808 on Kick 2 (tuned to F; set its tune to your key), a punchy 808 "
        "kick, crisp clap and snare on beat 3, 808 hats for rolls, open hat, rim and cowbell. 130-150 BPM, "
        "half-time feel.",
        kick=("808 Kick", {"gain": -1.0}), kick2=("Trap 808 F", {"gain": 0.0, "choke": 2}),
        snare="909 Snare Tight", clap="808 Clap", rim="808 Rim", closed_hat="808 Closed Hat",
        open_hat="808 Open Hat", pedal_hat=("808 Closed Hat", {"decay": 25.0}), crash="808 Cymbal",
        cowbell="808 Cowbell", low_tom="808 Tom Lo", mid_tom="808 Tom Mid", high_tom="808 Tom Hi",
        snare2="Finger Snap", claves="808 Clave"),
    kit("Rage Trap Kit", "Trap",
        "Rage and modern trap: a clipped, distorted 808 on Kick 2, hard kick, loud clap and snare, tight "
        "hats and a laser for transitions. Everything pushed to the edge; 140-160 BPM.",
        kick="909 Kick Hard", kick2=("Rage 808 F", {"choke": 2}), snare="Noise Snare", clap=("French Clap", {"gain": -2.0}),
        rim="808 Rim", closed_hat="808 Closed Hat", open_hat="808 Open Hat", pedal_hat="Crushed Hat",
        crash="909 Crash", cowbell=("Laser Zap", {"gain": -9.0}), snare2="909 Snare Tight",
        low_tom="808 Tom Lo", high_tom="808 Tom Hi"),
    kit("Phonk Kit", "Trap",
        "Drift phonk and Memphis: the saturated 808 cowbell for the melody (tune the pad), a distorted, "
        "lo-fi 808 on Kick 2, gritty Memphis hats and a crunchy clap. 130-160 BPM.",
        kick="808 Kick", kick2=("Phonk 808 F", {"choke": 2}), snare="808 Snare", clap="French Clap",
        rim="808 Rim", closed_hat="Memphis Closed Hat", open_hat="Memphis Open Hat",
        pedal_hat=("Memphis Closed Hat", {"decay": 25.0}), cowbell=("Phonk Cowbell", {"gain": -4.0}),
        crash="808 Cymbal", claves="808 Clave", low_tom="808 Tom Lo", high_tom="808 Tom Hi"),
    kit("UK Drill Kit", "Trap",
        "UK drill: a long gliding-style 808 on Kick 2, short kick, a crisp snare and clap for the "
        "displaced backbeat, hats tuned down for the skippy triplets, rim and a triangle on the one. 138-145 BPM.",
        kick=("808 Kick", {"decay": 300.0}), kick2=("Drill 808 F", {"choke": 2}), snare="909 Snare Tight",
        clap="808 Clap", rim="808 Rim", closed_hat=("808 Closed Hat", {"tune": -5.0}),
        open_hat=("808 Open Hat", {"tune": -5.0, "decay": 250.0}), pedal_hat=("808 Closed Hat", {"tune": -8.0}),
        triangle="Synth Triangle", crash="808 Cymbal", snare2="Finger Snap", cowbell="808 Cowbell"),
    # --- Hip-hop --------------------------------------------------------------------
    kit("Electro 808 Kit", "Hip-Hop",
        "Early-80s electro and Miami bass: the whole 808 set with a long boomy kick, snare, clap, "
        "congas, cowbell, clave and cymbal. Breakdance tempos 100-130 BPM.",
        kick="808 Kick Long", kick2="808 Kick", snare="808 Snare", clap="808 Clap", rim="808 Rim",
        closed_hat="808 Closed Hat", open_hat="808 Open Hat", crash="808 Cymbal", low_tom="808 Tom Lo",
        mid_tom="808 Tom Mid", high_tom="808 Tom Hi", cowbell="808 Cowbell", claves="808 Clave",
        mute_conga="808 Conga Hi", low_conga="808 Conga Lo", shaker="808 Maracas"),
    # --- Retro ----------------------------------------------------------------------
    kit("Synthwave Kit", "Retro",
        "Outrun and synthwave: big gated-reverb snare, electronic Simmons-style toms, a punchy machine "
        "kick, clap, crisp hats and a crash. 80-118 BPM.",
        kick="707 Kick", kick2="909 Kick Deep", snare="Gated Snare", clap="909 Clap", rim="909 Rim",
        closed_hat="909 Closed Hat", open_hat="909 Open Hat", pedal_hat="808 Closed Hat", crash="909 Crash",
        ride="909 Ride", low_tom="Simmons Tom Lo", mid_tom="Simmons Tom Mid", high_tom="Simmons Tom Hi",
        tambourine="Synth Tambourine", cowbell="808 Cowbell"),
]
