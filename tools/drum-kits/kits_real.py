"""Genre kits built (mostly) from recordings."""

from kitgen import kit, vel
from samples_real import JAZZ, PERC, RODS, ROCK, STUDIO, AFRO as AF


def L(names, **opts):
    """A pad of velocity layers (softest first)."""
    return (vel(*names) if len(names) > 1 else names[0], opts) if opts else (vel(*names) if len(names) > 1 else names[0])


P = {k: v for k, v in PERC.items()}

KITS = [
    # --- Acoustic -------------------------------------------------------------------------
    kit("Studio Kit", "Acoustic",
        "A recorded Pearl kit with Sabian cymbals, three velocity layers on kick, snare and hats: "
        "pop, rock, ballads and singer-songwriter. Side stick, snare edge, floor tom, crash, ride, "
        "china, splash, tambourine and cowbell.",
        kick=L(STUDIO["kick"]), snare=L(STUDIO["snare"]), snare2=L(STUDIO["snare_edge"]), rim=L(STUDIO["rim"]),
        closed_hat=L(STUDIO["closed_hat"]), pedal_hat=L(STUDIO["pedal_hat"]), open_hat=L(STUDIO["open_hat"]),
        low_tom=L(STUDIO["low_tom"]), mid_tom=L(STUDIO["mid_tom"]), crash=L(STUDIO["crash"]), ride=L(STUDIO["ride"]),
        ride_bell=L(STUDIO["ride_bell"]), china=L(STUDIO["china"]), splash=L(STUDIO["splash"]),
        tambourine=L(STUDIO["tambourine"]), cowbell=L(STUDIO["cowbell"])),
    kit("Rock Kit", "Acoustic",
        "Big rock kit with velocity layers: punchy kick, cracking snare and rimshot, three toms, "
        "open hats, crash, ride and bell. Rock, pop-punk and indie at 90-180 BPM.",
        kick=L(ROCK["kick"]), snare=L(ROCK["snare"]), snare2=L(ROCK["rimshot"]), rim=L(ROCK["rim"]),
        closed_hat=L(ROCK["closed_hat"]), pedal_hat=L(ROCK["pedal_hat"]), open_hat=L(ROCK["open_hat"]),
        low_tom=L(ROCK["low_tom"]), mid_tom=L(ROCK["mid_tom"]), high_tom=L(ROCK["high_tom"]),
        crash=L(ROCK["crash"]), ride=L(ROCK["ride"]), ride_bell=L(ROCK["ride_bell"]), splash=L(ROCK["splash"]),
        cowbell=L(ROCK["cowbell"])),
    kit("Jazz Kit", "Acoustic",
        "Small bebop kit: open-tuned 18-inch kick, crisp 13-inch snare, a washy 20-inch ride with "
        "velocity layers and bell, hats and toms. Swing, bebop and jazz ballads; play the ride.",
        kick=L(JAZZ["kick"]), snare=L(JAZZ["snare"]), rim=L(JAZZ["rim"]), closed_hat=L(JAZZ["closed_hat"]),
        pedal_hat=L(JAZZ["pedal_hat"]), open_hat=L(JAZZ["open_hat"]), low_tom=L(JAZZ["low_tom"]),
        mid_tom=L(JAZZ["mid_tom"]), ride=(vel(*JAZZ["ride"]), {"target": -6.0}), ride_bell=L(JAZZ["ride_bell"]),
        crash=L(JAZZ["crash"]), shaker=L(JAZZ["shaker"])),
    kit("Unplugged Rods Kit", "Acoustic",
        "The jazz kit played with rods: soft, woody hits for unplugged sets, folk, acoustic pop and "
        "quiet verses. Shakers and tambourine for the lift.",
        kick=L(RODS["kick"]), snare=L(RODS["snare"]), rim=L(RODS["rim"]), closed_hat=L(RODS["closed_hat"]),
        pedal_hat=L(RODS["pedal_hat"]), open_hat=L(RODS["open_hat"]), low_tom=L(RODS["low_tom"]),
        mid_tom=L(RODS["mid_tom"]), ride=L(RODS["ride"]), crash=L(RODS["crash"]),
        tambourine=L(RODS["tambourine"]), shaker=L(RODS["shaker"])),
    # --- Funk -----------------------------------------------------------------------------
    kit("70s Funk Kit", "Funk",
        "Dry, deadened 70s kit: tight 16-inch kick, a high-tuned 10-inch snare for backbeats and "
        "ghost notes (rimshot on Snare 2), short hats, cowbell and tambourine. 95-115 BPM.",
        kick="Vintage Kick", kick2="Vintage Kick 22", snare="Funk Snare 10", snare2="Funk Snare 10 Rimshot",
        rim="Funk Rim", clap="Vintage Clap", closed_hat=("Vintage Hat Closed", {"decay": 120.0}),
        pedal_hat="Vintage Hat Pedal", open_hat=("Vintage Hat Open", {"decay": 450.0}),
        low_tom="Vintage Tom 16", mid_tom="Vintage Tom 13", high_tom="Vintage Tom 10",
        ride="Vintage Ride", crash="Vintage Crash", cowbell="Vintage Cowbell", tambourine="Vintage Tambourine"),
    kit("Minneapolis Kit", "Funk",
        "Early-80s drum machine funk: crunchy 8-bit kick and snare, a clap and rim detuned down, "
        "electronic-sounding toms and conga, cowbell and tambourine. Add a flanger on the snare. 100-125 BPM.",
        kick="Linn Kick", snare=("Linn Snare", {"tune": -2.0}), clap=("Linn Clap", {"tune": -4.0}),
        rim=("Linn Rim", {"tune": -3.0}), closed_hat="Linn Hat", open_hat="Linn Open Hat",
        pedal_hat=("Linn Hat", {"decay": 40.0}), low_tom="Linn Tom Lo", high_tom="Linn Tom Hi",
        mid_tom=("Linn Tom Hi", {"tune": -4.0}), open_conga="Linn Conga", cowbell="Linn Cowbell",
        tambourine="Linn Tambourine", kick2="808 Kick", crash="909 Crash"),
    kit("Disco Kit", "Funk",
        "Live disco kit: short, punchy kick for four on the floor, a dry snare, open hats on the "
        "offbeats (choked by the closed hat), congas, tambourine and cowbell. 110-128 BPM.",
        kick="Disco Kick", snare="Disco Snare", clap=L(P["claps"]), rim=L(STUDIO["rim"]),
        closed_hat=L(STUDIO["closed_hat"]), pedal_hat=L(STUDIO["pedal_hat"]),
        open_hat=(vel(*STUDIO["open_hat"]), {"decay": 380.0}), crash=L(STUDIO["crash"]), ride=L(STUDIO["ride"]),
        low_tom=L(STUDIO["low_tom"]), mid_tom=L(STUDIO["mid_tom"]), tambourine=L(P["tambourine"]),
        cowbell=L(P["cowbell"]), open_conga=L(P["conga"]), low_conga=L(P["tumba"])),
    # --- Pop ------------------------------------------------------------------------------
    kit("Modern Pop Kit", "Pop",
        "Current pop and pop-trap: a layered acoustic kick, a clean 808 on Kick 2 (tuned to F), "
        "a bright snare with room, hand clap and finger snaps for the backbeat, tight hats, "
        "shaker and tambourine. 90-125 BPM.",
        kick=L(STUDIO["kick"]), kick2=("Trap 808 Clean F", {"choke": 2}), snare="Pop Snare", clap=L(P["solo_clap"]),
        snare2=L(P["snap"]), rim=L(STUDIO["rim"]), closed_hat=L(STUDIO["closed_hat"]),
        pedal_hat=L(STUDIO["pedal_hat"]), open_hat=L(STUDIO["open_hat"]), shaker=L(P["shaker"]),
        tambourine=L(P["tambourine"]), crash=L(STUDIO["crash"]), cowbell=("808 Cowbell", {"target": -12.0})),
    kit("80s Pop Kit", "Pop",
        "Gated-reverb 80s: a huge gated snare and toms, crunchy drum-machine kick, clap, hats, "
        "cowbell and tambourine. Power ballads, new wave and synth-pop at 100-130 BPM.",
        kick="Linn Kick", snare="Gated Room Snare", clap="Linn Clap", rim="Linn Rim", closed_hat="Linn Hat",
        open_hat="Linn Open Hat", pedal_hat=("Linn Hat", {"decay": 40.0}), high_tom="Gated Room Tom Hi",
        low_tom="Gated Room Tom Lo", mid_tom=("Gated Room Tom Hi", {"tune": -3.0}), crash=L(STUDIO["crash"]),
        cowbell="Linn Cowbell", tambourine="Linn Tambourine", kick2="909 Kick"),
    # --- Hip-hop --------------------------------------------------------------------------
    kit("Boom Bap Kit", "Hip-Hop",
        "90s boom bap: 12-bit sampler grit on a low-passed kick, a fat snare with room, rim, "
        "dusty hats and clap. Swing it 55-62% at 85-95 BPM.",
        kick="Boom Bap Kick", snare="Boom Bap Snare", rim="Boom Bap Rim", clap="Boom Bap Clap",
        closed_hat="Boom Bap Hat", open_hat="Boom Bap Open Hat", pedal_hat=("Boom Bap Hat", {"decay": 40.0}),
        kick2="Vintage Kick", snare2="Vintage Snare 14", shaker="Lofi Shaker", tambourine="Vintage Tambourine",
        crash="Vintage Crash", ride="Vintage Ride"),
    kit("Lo-Fi Kit", "Hip-Hop",
        "Lo-fi and chillhop: muffled tape kick and snare, a soft rim, hats pitched down, a "
        "shaker and some room. Lazy swing at 70-90 BPM.",
        kick="Lofi Kick", snare="Lofi Snare", rim="Lofi Rim", closed_hat="Lofi Hat", open_hat="Lofi Open Hat",
        pedal_hat=("Lofi Hat", {"decay": 35.0}), shaker="Lofi Shaker", clap="Boom Bap Clap",
        snare2="Jazz Snare v2", ride=L(JAZZ["ride"][:1]), cowbell=("Woodblock v1", {"target": -12.0})),
    # --- Reggaeton ------------------------------------------------------------------------
    kit("Classic Dembow Kit", "Reggaeton",
        "2000s reggaeton: punchy kick on every beat, the bright snare for the dembow offbeats, "
        "timbales for fills and accents, rim, hats and congas. 88-100 BPM.",
        kick="808 Kick", kick2="909 Kick", snare="Dembow Snare", snare2="909 Snare Tight", rim="808 Rim",
        clap="808 Clap", closed_hat="909 Closed Hat", open_hat="909 Open Hat", high_timbale="Timbal Hi",
        low_timbale="Timbal Lo", open_conga=L(P["conga"]), low_conga=L(P["tumba"]), crash="909 Crash",
        cowbell=L(P["campana"]), shaker=L(P["maraca"])),
    kit("Perreo Kit", "Reggaeton",
        "Modern reggaeton and perreo: a long 808 on Kick 2 (tuned to F), punchy kick, snare and "
        "rim for the dembow, clap, tight hats, timbales and a shaker. 88-100 BPM.",
        kick="808 Kick", kick2=("Trap 808 F", {"choke": 2}), snare="Dembow Snare", rim=("808 Rim", {"target": -5.0}),
        clap="Hand Clap v1", closed_hat="808 Closed Hat", open_hat="808 Open Hat",
        pedal_hat=("808 Closed Hat", {"decay": 30.0}), high_timbale="Timbal Hi", low_timbale="Timbal Lo",
        shaker=L(P["shaker"]), snare2=L(P["snap"]), crash="808 Cymbal", open_conga=L(P["conga"])),
    kit("Latin Trap Kit", "Reggaeton",
        "Latin trap: trap drums (long 808 on Kick 2, clap, rolling hats) with a guira, claves and "
        "bongos for the Caribbean color. 130-150 BPM half-time.",
        kick="808 Kick", kick2=("Drill 808 F", {"choke": 2}), snare="909 Snare Tight", clap="808 Clap",
        rim="808 Rim", closed_hat="808 Closed Hat", open_hat="808 Open Hat", pedal_hat=("808 Closed Hat", {"decay": 25.0}),
        guiro="Guira Long", short_guiro="Guira Short", claves=L(P["claves"]), hi_bongo=L(P["bongo_hi"]),
        low_bongo=L(P["bongo_lo"]), crash="808 Cymbal", snare2=L(P["snap"])),
    # --- Latin ----------------------------------------------------------------------------
    kit("Salsa Percussion", "Latin",
        "The salsa section: quinto, conga and tumba for the tumbao, bongos and bongo bell (on High "
        "Agogo), the campana (Cowbell), timbales with the paila shell (Rim), claves, guiro, "
        "maracas and a bombo kick. 160-210 BPM in two.",
        kick=L(P["concert_bd"]), rim="Paila", mute_conga=L(P["quinto_mute"]), open_conga=L(P["conga"]),
        low_conga=L(P["tumba"]), hi_bongo=L(P["bongo_hi"]), low_bongo=L(P["bongo_lo"]),
        high_timbale="Timbal Hi", low_timbale="Timbal Lo", cowbell=L(P["campana"]), high_agogo=L(P["cowbell"]),
        claves=L(P["claves"]), guiro=L(P["guiro_long"]), short_guiro=L(P["guiro_short"]), shaker=L(P["maraca"]),
        snare=L(P["quinto"])),
    kit("Cumbia Kit", "Latin",
        "Cumbia: the guacharaca scrape (Guiro), tambora drum and rim, llamador and alegre congas, "
        "maracas, cowbell, plus a dry kick, snare and hats for modern cumbia. 85-110 BPM.",
        kick="Dry Kick", snare="Dry Snare", closed_hat="Dry Hat Closed", open_hat="Dry Hat Open",
        rim="Tambora Rim", low_tom="Tambora", mute_conga=L(P["conga_mute"]), open_conga=L(P["quinto"]),
        low_conga=L(P["tumba"]), guiro=L(P["guiro_long"]), short_guiro=L(P["guiro_short"]),
        shaker=L(P["maraca"]), cowbell=L(P["cowbell_mute"]), clap="Dry Clap", claves=L(P["claves"])),
    kit("Bachata Kit", "Latin",
        "Bachata: the guira scrape and hit, bongos (open and muted), bongo bell, conga and tumba, "
        "a soft kick and maracas. 120-140 BPM; the low bongo answers on beat 4.",
        kick=L(P["concert_bd"]), hi_bongo=L(P["bongo_hi"]), low_bongo=L(P["bongo_lo"]),
        mute_conga=L(P["bongo_hi_mute"]), open_conga=L(P["conga"]), low_conga=L(P["tumba"]),
        guiro="Guira Long", short_guiro="Guira Short", cowbell=L(P["cowbell"]), shaker=L(P["maraca"]),
        claves=L(P["claves"]), rim="Paila", snare=L(P["cajon_slap"])),
    # --- Afro -----------------------------------------------------------------------------
    kit("Afrobeats Kit", "Afro",
        "Afrobeats and afropop: a tight kick, the pitched-up side stick that leads the groove, "
        "clap and snap, constant shaker and shekere (Cabasa), congas and talking drum. 95-115 BPM.",
        kick="Dry Kick", rim="Afrobeats Rim", clap=L(P["solo_clap"]), snare2=L(P["snap"]), snare="Dry Snare",
        shaker=L(P["shaker"]), cabasa=L(P["cabasa"]), open_conga=L(P["conga"]), low_conga=L(P["tumba"]),
        mute_conga="Talking Drum Hit", closed_hat="Dry Hat Closed", open_hat="Dry Hat Open",
        hi_bongo="Talking Drum Up", low_bongo="Talking Drum Down", tambourine=L(P["tambourine"])),
    kit("Amapiano Kit", "Afro",
        "Amapiano drums: a soft, deep kick, woody side stick and wood log hits, a constant "
        "shaker, group clap, congas and a short open hat. Pair it with the Log Drum synth preset. 108-115 BPM.",
        kick="909 Kick Deep", rim="Afrobeats Rim", clap=L(P["claps"]), shaker=L(P["shaker"]),
        cabasa=L(P["large_shaker"]), hi_woodblock=L(P["log_hi"]), low_woodblock=L(P["log_lo"]),
        open_conga=L(P["conga"]), low_conga=L(P["tumba"]), closed_hat="808 Closed Hat",
        open_hat=("808 Open Hat", {"decay": 250.0}), snare2=L(P["snap"]), claves=L(P["woodblock"]),
        tambourine=L(P["tambourine"])),
    kit("Afro House Kit", "Afro",
        "Afro house: 909 kick under djembe bass, tone and slap, the dunun family on the toms with "
        "the kenkeni bell (High Agogo), cabasa, shaker, clap and 909 hats. 118-124 BPM.",
        kick="909 Kick", kick2=L(AF["djembe_bass"]), mute_conga=L(AF["djembe_slap"]), open_conga=L(AF["djembe_tone"]),
        hi_bongo=L(AF["djembe2_tone"]), low_bongo=L(AF["djembe2_slap"]), low_tom=L(AF["dununba"]),
        mid_tom=L(AF["sangban"]), high_tom=L(AF["kenkeni"]), high_agogo=L(AF["kenkeni_bell"]),
        cabasa=L(P["cabasa"]), shaker=L(P["shaker"]), clap="909 Clap", closed_hat="909 Closed Hat",
        open_hat="909 Open Hat", rim="909 Rim"),
]
