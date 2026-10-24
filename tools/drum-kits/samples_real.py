"""Factory samples cut from redistributable recordings (sources and licenses: docs/design/drum-kit-library.md;
fetched by fetch_sources.sh).
name -> Sample."""

import numpy as np

import drumlab as d
from kitgen import Credit, Sample

VCSL = Credit("Versilian Community Sample Library (VCSL)", "https://github.com/sgossner/VCSL", "Versilian Studios and contributors", "CC0-1.0")
SONICPI = Credit("Sonic Pi sample pack", "https://github.com/sonic-pi-net/sonic-pi/tree/dev/etc/samples", "Sonic Pi contributors (Freesound CC0 recordings)", "CC0-1.0")
BLACK_PEARL = Credit("AVL Drumkits: Black Pearl", "https://www.bandshed.net/avldrumkits/", "Glen MacArthur", "CC-BY-SA-3.0")
BLONDE_BOP = Credit("AVL Drumkits: Blonde Bop", "https://www.bandshed.net/avldrumkits/", "Glen MacArthur", "CC-BY-SA-3.0")
HOT_ROD = Credit("AVL Drumkits: Blonde Bop Hot Rod", "https://www.bandshed.net/avldrumkits/", "Glen MacArthur", "CC-BY-SA-3.0")
GMROCK = Credit("Hydrogen drumkit: GMRockKit", "https://sourceforge.net/projects/hydrogen/files/Sound%20Libraries/", "Hydrogen project", "GPL-2.0-or-later")
SF3007 = Credit("Hydrogen drumkit: SF3007-2011-Set-03", "https://sourceforge.net/projects/hydrogen/files/Sound%20Libraries/", "The recordist of the Sonor Force 3007 set (Hydrogen drumkit)", "WTFPL")
MILLO = Credit("Hydrogen drumkit: Millo MultiLayered 2", "https://sourceforge.net/projects/hydrogen/files/Sound%20Libraries/", "Milo (Hydrogen drumkit author)", "GPL-2.0-or-later")
GIMME = Credit("Hydrogen drumkit: Gimme A Hand 1.0", "https://sourceforge.net/projects/hydrogen/files/Sound%20Libraries/", "Hydrogen drumkit author", "GPL-2.0-or-later")
AFRIQUE = Credit("Hydrogen drumkit: circAfrique v4", "https://sourceforge.net/projects/hydrogen/files/Sound%20Libraries/", "Jim Harney", "CC-BY-SA-3.0")
CADENZA = None  # synthesized: kitgen credits Cadenza

BP = "hydrogen-AVLDrumkits-BlackPearl-2023/*/"
BB = "hydrogen-AVLDrumkits-BlondeBop/*/"
HR = "hydrogen-AVLDrumkits-BlondeBop-HotRod/*/"
GM = "hydrogen-GMRockKit/**/"
SF = "hydrogen-SF3007-2011-Set-03/*/"
ML = "hydrogen-Millo_MultiLayered2/*/"
GH = "hydrogen-Gimme_A_Hand_1.0/**/"
AF = "hydrogen-circAfrique_v4/**/"
V = "vcsl/**/"

SAMPLES = {}


def add(name, note, file, credit, **kw):
    assert name not in SAMPLES, name
    SAMPLES[name] = Sample(note=note, file=file, credit=credit, **kw)


def layers(name, note, files, credit, **kw):
    """Velocity layers 'name v1..vn' (softest first); returns the layer names."""
    names = []
    for i, f in enumerate(files):
        n = f"{name} v{i + 1}"
        add(n, note, f, credit, **kw)
        names.append(n)
    return names


def avl(prefix, piece, note, credit, picks=(2, 4, 5), **kw):
    return [f"{prefix}{piece}-{i}.wav" for i in picks]


# --- Studio kit (AVL Black Pearl: Pearl kit, Sabian cymbals; pop/rock) -----------------
STUDIO = {
    "kick": layers("Studio Kick", 36, avl(BP, "36-Pearl-22-Kick", 36, BLACK_PEARL), BLACK_PEARL, length=1.0),
    "snare": layers("Studio Snare", 38, avl(BP, "38-Pearl-14-Snare", 38, BLACK_PEARL), BLACK_PEARL, length=1.0),
    "rim": layers("Studio Side Stick", 37, avl(BP, "37-Pearl-14-SideStick", 37, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL),
    "snare_edge": layers("Studio Snare Edge", 40, avl(BP, "40-Pearl-14-SnareEdge", 40, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL, length=1.0),
    "closed_hat": layers("Studio Hat Closed", 42, avl(BP, "42-Sabian-13-HatClosed", 42, BLACK_PEARL), BLACK_PEARL),
    "pedal_hat": layers("Studio Hat Pedal", 44, avl(BP, "44-Sabian-13-HatPedal", 44, BLACK_PEARL, picks=(4,)), BLACK_PEARL),
    "open_hat": layers("Studio Hat Open", 46, avl(BP, "46-Sabian-13-HatSemi", 46, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL, length=2.0),
    "low_tom": layers("Studio Floor Tom", 41, avl(BP, "41-Pearl-16-FloorTom", 41, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL, length=1.6),
    "mid_tom": layers("Studio Tom", 45, avl(BP, "45-Pearl-12-Tom", 45, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL, length=1.4),
    "crash": layers("Studio Crash", 49, avl(BP, "49-SabianAA-16-Crash", 49, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL, length=3.0),
    "ride": layers("Studio Ride", 51, avl(BP, "51-SabianAAX-20-Ride", 51, BLACK_PEARL, picks=(3, 5)), BLACK_PEARL, length=3.0),
    "ride_bell": layers("Studio Ride Bell", 53, avl(BP, "53-SabianAAX-20-RideBell", 53, BLACK_PEARL, picks=(4,)), BLACK_PEARL, length=3.0),
    "tambourine": layers("Studio Tambourine", 54, avl(BP, "54-Tambourine", 54, BLACK_PEARL, picks=(4,)), BLACK_PEARL),
    "cowbell": layers("Studio Cowbell", 56, avl(BP, "56-Cowbell", 56, BLACK_PEARL, picks=(4,)), BLACK_PEARL),
    "china": layers("Studio China", 52, avl(BP, "62-Wuhan-18-ChinaCrash", 52, BLACK_PEARL, picks=(4,)), BLACK_PEARL, length=3.0),
    "splash": layers("Studio Splash", 55, avl(BP, "55-Zildjian-10-Splash", 55, BLACK_PEARL, picks=(4,)), BLACK_PEARL, length=2.0),
}

# --- Jazz kit (AVL Blonde Bop: 18" kick, 13" snare, 20" ride) ---------------------------
JAZZ = {
    "kick": layers("Jazz Kick", 36, avl(BB, "36-TAMA-18-Kick", 36, BLONDE_BOP, picks=(2, 4)), BLONDE_BOP, length=1.0),
    "snare": layers("Jazz Snare", 38, avl(BB, "38-TAMA-13-Snare", 38, BLONDE_BOP), BLONDE_BOP, length=1.0),
    "rim": layers("Jazz Side Stick", 37, avl(BB, "37-TAMA-13-SideStick", 37, BLONDE_BOP, picks=(4,)), BLONDE_BOP),
    "closed_hat": layers("Jazz Hat Closed", 42, avl(BB, "42-Zildjian-13-HatClosed", 42, BLONDE_BOP, picks=(3, 5)), BLONDE_BOP),
    "pedal_hat": layers("Jazz Hat Pedal", 44, avl(BB, "44-Zildjian-13-HatPedal", 44, BLONDE_BOP, picks=(4,)), BLONDE_BOP),
    "open_hat": layers("Jazz Hat Semi", 46, avl(BB, "46-Zildjian-13-HatSemi", 46, BLONDE_BOP, picks=(4,)), BLONDE_BOP, length=2.0),
    "low_tom": layers("Jazz Floor Tom", 41, avl(BB, "41-TAMA-14-FloorTom", 41, BLONDE_BOP, picks=(4,)), BLONDE_BOP, length=1.5),
    "mid_tom": layers("Jazz Tom", 45, avl(BB, "45-TAMA-10-Tom", 45, BLONDE_BOP, picks=(4,)), BLONDE_BOP, length=1.2),
    "ride": layers("Jazz Ride", 51, avl(BB, "51-SabianAAX-20-RideTip", 51, BLONDE_BOP), BLONDE_BOP, length=3.0),
    "ride_bell": layers("Jazz Ride Bell", 53, avl(BB, "53-SabianAAX-20-RideBell", 53, BLONDE_BOP, picks=(4,)), BLONDE_BOP, length=3.0),
    "crash": layers("Jazz Crash", 49, avl(BB, "49-Paiste2002-16-Crash", 49, BLONDE_BOP, picks=(4,)), BLONDE_BOP, length=3.0),
    "shaker": layers("Jazz Shakers", 70, avl(BB, "61-GonBops-3-Shakers", 70, BLONDE_BOP, picks=(4,)), BLONDE_BOP),
}

# --- Rods kit (AVL Blonde Bop Hot Rod: played with rods; soft, unplugged) -------------------
RODS = {
    "kick": layers("Rods Kick", 36, avl(HR, "36-TAMA-18-Kick-HR", 36, HOT_ROD, picks=(3, 5)), HOT_ROD, length=1.0),
    "snare": layers("Rods Snare", 38, avl(HR, "38-TAMA-13-SnareCenter-HR", 38, HOT_ROD), HOT_ROD, length=1.0),
    "rim": layers("Rods Side Stick", 37, avl(HR, "37-TAMA-13-SnareSidestick-HR", 37, HOT_ROD, picks=(4,)), HOT_ROD),
    "closed_hat": layers("Rods Hat Closed", 42, avl(HR, "42-Zildjian-13-HatClosed-HR", 42, HOT_ROD, picks=(3, 5)), HOT_ROD),
    "open_hat": layers("Rods Hat Semi", 46, avl(HR, "46-Zildjian-12-HatSemi-HR", 46, HOT_ROD, picks=(4,)), HOT_ROD, length=2.0),
    "pedal_hat": layers("Rods Hat Pedal", 44, avl(HR, "44-Zildjian-13-HatPedal-HR", 44, HOT_ROD, picks=(4,)), HOT_ROD),
    "low_tom": layers("Rods Floor Tom", 41, avl(HR, "41-TAMA-14-FloorTomCenter-HR", 41, HOT_ROD, picks=(4,)), HOT_ROD, length=1.5),
    "mid_tom": layers("Rods Tom", 45, avl(HR, "45-TAMA-10-TomCenter-HR", 45, HOT_ROD, picks=(4,)), HOT_ROD, length=1.2),
    "ride": layers("Rods Ride", 51, avl(HR, "51-SabianAAX-20-RideTip-HR", 51, HOT_ROD, picks=(3, 5)), HOT_ROD, length=3.0),
    "crash": layers("Rods Crash", 49, avl(HR, "49-Paiste2002-16-Crash-HR", 49, HOT_ROD, picks=(4,)), HOT_ROD, length=3.0),
    "tambourine": layers("Rods Tambourine", 54, avl(HR, "54-GP-10-Tambourine-HR", 54, HOT_ROD, picks=(4,)), HOT_ROD),
    "shaker": layers("Rods Shakers", 70, avl(HR, "61-GonBops-3-Shakers-HR", 70, HOT_ROD, picks=(4,)), HOT_ROD),
}

# --- Rock kit (Hydrogen GMRockKit) -------------------------------------------------------
GMV = ("Soft", "Med", "Hardest")
ROCK = {
    "kick": layers("Rock Kick", 36, [f"{GM}Kick-{v}.wav" for v in GMV], GMROCK, length=1.0),
    "snare": layers("Rock Snare", 38, [f"{GM}Snare-{v}.wav" for v in GMV], GMROCK, length=1.0),
    "rimshot": layers("Rock Rimshot", 40, [f"{GM}SnareRimshot-{v}.wav" for v in ("Med", "Hardest")], GMROCK, length=1.0),
    "rim": layers("Rock Side Stick", 37, [f"{GM}SideStick-Hard.wav"], GMROCK),
    "closed_hat": layers("Rock Hat Closed", 42, [f"{GM}HatClosed-{v}.wav" for v in GMV], GMROCK),
    "pedal_hat": layers("Rock Hat Pedal", 44, [f"{GM}HatPedal-Hard.wav"], GMROCK),
    "open_hat": layers("Rock Hat Open", 46, [f"{GM}HatOpen-{v}.wav" for v in ("Med", "Hardest")], GMROCK, length=2.5),
    "low_tom": layers("Rock Floor Tom", 41, [f"{GM}TomFloor-Hard.wav"], GMROCK, length=1.6),
    "mid_tom": layers("Rock Tom 2", 45, [f"{GM}Tom2-Hard.wav"], GMROCK, length=1.4),
    "high_tom": layers("Rock Tom 1", 48, [f"{GM}Tom1-Hard.wav"], GMROCK, length=1.2),
    "crash": layers("Rock Crash", 49, [f"{GM}Crash-{v}.wav" for v in ("Med", "Hardest")], GMROCK, length=3.0),
    "ride": layers("Rock Ride", 51, [f"{GM}Ride-Hard.wav"], GMROCK, length=3.0),
    "ride_bell": layers("Rock Ride Bell", 53, [f"{GM}Bell-Hard.wav"], GMROCK, length=3.0),
    "splash": layers("Rock Splash", 55, [f"{GM}Splash-Hard.wav"], GMROCK, length=2.0),
    "cowbell": layers("Rock Cowbell", 56, [f"{GM}Cowbell-Hard.wav"], GMROCK),
}

# --- Vintage / funk pieces (SF3007: small high-tuned snares, dry kicks) -----------------------
add("Vintage Kick", 36, f"{SF}01-bd16-r.flac", SF3007, length=0.8)
add("Vintage Kick 22", 36, f"{SF}02-bd22-r-1.flac", SF3007, length=0.9)
add("Funk Snare 10", 38, f"{SF}05-sd10-normal.flac", SF3007, length=0.8)
add("Funk Snare 10 Rimshot", 40, f"{SF}05-sd10-rimshot.flac", SF3007, length=0.8)
add("Funk Snare 12", 38, f"{SF}04-sd12-normal.flac", SF3007, length=0.8)
add("Vintage Snare 14", 38, f"{SF}06-sd145-normal.flac", SF3007, length=0.9)
add("Vintage Hat Closed", 42, f"{SF}07-hihat-closed-short.flac", SF3007)
add("Vintage Hat Open", 46, f"{SF}07-hihat-open-short.flac", SF3007, length=2.0)
add("Vintage Hat Pedal", 44, f"{SF}08-hihat-closed-short.flac", SF3007)
add("Vintage Tom 10", 48, f"{SF}10-tom10.flac", SF3007, length=1.0)
add("Vintage Tom 13", 45, f"{SF}10-tom13.flac", SF3007, length=1.2)
add("Vintage Tom 16", 41, f"{SF}10-tom16.flac", SF3007, length=1.4)
add("Vintage Ride", 51, f"{SF}15-ride-1.flac", SF3007, length=3.0)
add("Vintage Crash", 49, f"{SF}17-crash15ana.flac", SF3007, length=3.0)
add("Vintage Rim Click", 37, f"{SF}45-rimclick12-1.flac", SF3007)
add("Vintage Cowbell", 56, f"{SF}42-cowbell6.flac", SF3007)
add("Vintage Tambourine", 54, f"{SF}50-tambourine.flac", SF3007)
add("Vintage Clap", 39, f"{SF}46-clap-1.flac", SF3007)

# --- Dry pieces (Millo: tight studio kit) -------------------------------------------------
add("Dry Kick", 36, f"{ML}bd_04.flac", MILLO, length=0.8)
add("Dry Snare", 38, f"{ML}rsnare_04.flac", MILLO, length=0.8)
add("Dry Rim", 37, f"{ML}rim_03.flac", MILLO)
add("Dry Hat Closed", 42, f"{ML}hhclosed_04.flac", MILLO)
add("Dry Hat Open", 46, f"{ML}hhopen_02.flac", MILLO, length=1.5)
add("Dry Clap", 39, f"{ML}clap_03.flac", MILLO)

# --- Hand percussion (VCSL, Gimme A Hand) ----------------------------------------------------
CONGA = {
    "quinto": [f"{V}Quinto_HitN_v{v}_rr1_Sum.wav" for v in (2, 3)],
    "quinto_mute": [f"{V}Quinto_HitFM1_v2_rr1_Sum.wav"],
    "conga": [f"{V}Conga_HitN_v{v}_rr1_Sum.wav" for v in (2, 3)],
    "conga_mute": [f"{V}Conga_HitFM_v2_rr1_Sum.wav"],
    "tumba": [f"{V}Tumba_HitN_v{v}_rr1_Sum.wav" for v in (3, 4)],
}
PERC = {
    "quinto": layers("Conga Quinto", 63, CONGA["quinto"], VCSL),
    "quinto_mute": layers("Conga Quinto Muted", 62, CONGA["quinto_mute"], VCSL),
    "conga": layers("Conga Open", 63, CONGA["conga"], VCSL),
    "conga_mute": layers("Conga Muted", 62, CONGA["conga_mute"], VCSL),
    "tumba": layers("Conga Tumba", 64, CONGA["tumba"], VCSL),
    "bongo_hi": layers("Bongo Hi", 60, [f"{V}BongoH_Hit1_v{v}_rr1_Mid.wav" for v in (2, 3)], VCSL),
    "bongo_lo": layers("Bongo Lo", 61, [f"{V}BongoL_Hit1_v{v}_rr1_Mid.wav" for v in (2, 3)], VCSL),
    "bongo_hi_mute": layers("Bongo Hi Muted", 60, [f"{V}BongoH_HitMuted1_v3_rr1_Mid.wav"], VCSL),
    "claves": layers("Claves", 75, [f"{V}Claves1_Hit_v3_rr1*.wav"], VCSL),
    "cowbell": layers("Bongo Bell", 56, [f"{V}Cowbell1_Normal_v3_rr1*.wav"], VCSL),
    "cowbell_mute": layers("Bongo Bell Muted", 56, [f"{V}Cowbell1_Muted_v3_rr1*.wav"], VCSL),
    "campana": layers("Campana", 56, [f"{V}Cowbell2_Normal_v3_rr1*.wav"], VCSL),
    "guiro_short": layers("Guiro Short", 73, [f"{V}guiro_short.wav"], VCSL),
    "guiro_long": layers("Guiro Long", 74, [f"{V}guiro_long.wav"], VCSL, length=1.2),
    "shaker": layers("Shaker", 70, [f"{V}Mid_ShakerDouble_Down_rr1*.wav"], VCSL),
    "shaker_up": layers("Shaker Up", 70, [f"{V}Mid_ShakerDouble_Up_rr1*.wav"], VCSL),
    "large_shaker": layers("Large Shaker", 70, [f"{V}LShaker_Hit_rr1*.wav"], VCSL),
    "cabasa": layers("Cabasa", 69, [f"{V}Cabasa1_Hit_rr1*.wav"], VCSL),
    "tambourine": layers("Tambourine", 54, [f"{V}Tamb1_Hit_v2_rr1*.wav"], VCSL),
    "agogo_hi": layers("Agogo Hi", 67, [f"{V}Agogo_High_v2_rr1*.wav"], VCSL),
    "agogo_lo": layers("Agogo Lo", 68, [f"{V}Agogo_Low_v2_rr1*.wav"], VCSL),
    "woodblock": layers("Woodblock", 76, [f"{V}wood_click_f_rr1.wav"], VCSL),
    "cajon": layers("Cajon Bass", 36, [f"{V}Cajon_hit1_f_rr1*.wav"], VCSL),
    "cajon_slap": layers("Cajon Slap", 38, [f"{V}Cajon_hit3_f_rr1*.wav"], VCSL),
    "log_hi": layers("Log Drum Hi", 76, [f"{V}LogDrumHi_MedM_v2_rr1*.wav"], VCSL),
    "log_lo": layers("Log Drum Lo", 77, [f"{V}LogDrumLo_MedM_v2_rr1*.wav"], VCSL),
    "claps": layers("Group Clap", 39, [f"{V}Clap_rr1.wav"], VCSL),
    "solo_clap": layers("Hand Clap", 39, [f"{V}SoloClap_vl3.wav"], VCSL, fx=lambda x: d.reverb(x, 0.7, wet=0.45, damp_hz=7000)),
    "snap": layers("Snap", 39, [f"{GH}Fingersnap-Hard.wav"], GIMME, fx=lambda x: d.reverb(x, 0.5, wet=0.3, damp_hz=9000)),
    "maraca": layers("Maraca", 70, [f"{GH}Maraca-Hard.wav"], GIMME),
    "concert_bd": layers("Bass Drum", 36, [f"{V}BDrumNew_hit_v5_rr1_Sum.wav"], VCSL, length=1.5),
}
# Timbales: VCSL snares with the snares off, tuned up (thin metal-shell sound), and the
# shell of a tom for the cascara / paila pattern.
add("Timbal Hi", 65, f"{V}Snare3M_HitNS_v5_rr1*.wav", VCSL, semitones=5.0, hp=180, length=0.8)
add("Timbal Lo", 66, f"{V}Snare3M_HitNS_v4_rr1*.wav", VCSL, semitones=2.0, hp=150, length=0.9)
add("Paila", 37, f"{V}TomH_rimS_v4_rr1*.wav", VCSL, semitones=3.0, hp=400)
add("Tambora", 41, f"{V}TomL_HitM_v4_rr1*.wav", VCSL, semitones=-2.0, length=1.0)
add("Tambora Rim", 37, f"{V}TomL_rimS_v4_rr1*.wav", VCSL)

# Afro: djembe and dunun family (circAfrique).
AFRO = {
    "djembe_bass": layers("Djembe Bass", 36, [f"{AF}Djembe1 Bass.wav"], AFRIQUE),
    "djembe_tone": layers("Djembe Tone", 63, [f"{AF}Djembe1 Tone.wav"], AFRIQUE),
    "djembe_slap": layers("Djembe Slap", 62, [f"{AF}Djembe1 Slap.wav"], AFRIQUE),
    "djembe2_tone": layers("Djembe Hi Tone", 60, [f"{AF}Djembe3 Tone.wav"], AFRIQUE),
    "djembe2_slap": layers("Djembe Hi Slap", 61, [f"{AF}Djembe3 Slap.wav"], AFRIQUE),
    "dununba": layers("Dununba", 41, [f"{AF}Dununba1 Head Hit.wav"], AFRIQUE),
    "sangban": layers("Sangban", 45, [f"{AF}Sangban1 Head Hit.wav"], AFRIQUE),
    "kenkeni": layers("Kenkeni", 48, [f"{AF}Kenkeni1 Head Hit.wav"], AFRIQUE),
    "kenkeni_bell": layers("Kenkeni Bell", 67, [f"{AF}Kenkeni1 Bell Hit.wav"], AFRIQUE),
    "sangban_bell": layers("Sangban Bell", 68, [f"{AF}Sangban1 Bell Hit.wav"], AFRIQUE),
}

# --- Processed variants -----------------------------------------------------------------------


def boom_bap(x):
    """SP-1200 flavor: 12-bit at 26 kHz, a little saturation."""
    return d.sat(d.lofi(x, bits=12, rate=26040, lp=11000) * 1.5, 1.5)


def lofi_hip_hop(x):
    """Muffled tape: low-passed, 10-bit, slow saturation."""
    return d.sat(d.lofi(x, bits=10, rate=22050, lp=5200) * 1.2, 1.2)


def linn(x):
    """Early digital drum machine: 8-bit at 28 kHz."""
    return d.lofi(x, bits=8, rate=28000, lp=12000)


def gated(x):
    return d.gated_reverb(x, decay=2.6, hold=0.17, release=0.13, wet=0.8)


def room(x, decay=0.5, wet=0.3):
    return d.reverb(x, decay, wet=wet, damp_hz=8000)


for name, src, credit, fx, note, kw in [
    ("Boom Bap Kick", f"{GM}Kick-Hardest.wav", GMROCK, boom_bap, 36, {"lp": 7000, "length": 0.6}),
    ("Boom Bap Snare", f"{SF}06-sd145-normal.flac", SF3007, lambda x: boom_bap(room(x, 0.4, 0.25)), 38, {"length": 0.6}),
    ("Boom Bap Rim", f"{SF}45-rimclick12-1.flac", SF3007, boom_bap, 37, {}),
    ("Boom Bap Hat", f"{GM}HatClosed-Hard.wav", GMROCK, boom_bap, 42, {}),
    ("Boom Bap Open Hat", f"{GM}HatOpen-Med.wav", GMROCK, boom_bap, 46, {"length": 1.2}),
    ("Boom Bap Clap", f"{V}Clap_rr2.wav", VCSL, boom_bap, 39, {}),
    ("Lofi Kick", f"{ML}bd_03.flac", MILLO, lofi_hip_hop, 36, {"length": 0.6}),
    ("Lofi Snare", f"{ML}rsnare_03.flac", MILLO, lambda x: lofi_hip_hop(room(x, 0.6, 0.3)), 38, {"length": 0.7}),
    ("Lofi Rim", f"{ML}rim_02.flac", MILLO, lofi_hip_hop, 37, {}),
    ("Lofi Hat", f"{ML}hhclosed_03.flac", MILLO, lofi_hip_hop, 42, {"semitones": -2.0}),
    ("Lofi Open Hat", f"{ML}hhopen_01.flac", MILLO, lofi_hip_hop, 46, {"semitones": -2.0, "length": 1.0}),
    ("Lofi Shaker", f"{V}Mid_ShakerDouble_Up_rr2*.wav", VCSL, lofi_hip_hop, 70, {}),
    ("Linn Kick", f"{ML}bd_05.flac", MILLO, linn, 36, {"length": 0.5}),
    ("Linn Snare", f"{SF}04-sd12-normal.flac", SF3007, linn, 38, {"length": 0.5}),
    ("Linn Clap", f"{V}Clap_rr3.wav", VCSL, linn, 39, {}),
    ("Linn Rim", f"{SF}45-rimclick14-1.flac", SF3007, linn, 37, {}),
    ("Linn Hat", f"{SF}08-hihat-closed-short.flac", SF3007, linn, 42, {}),
    ("Linn Open Hat", f"{SF}08-hihat-open-short.flac", SF3007, linn, 46, {"length": 0.8}),
    ("Linn Tom Hi", f"{SF}10-tom10.flac", SF3007, linn, 48, {"length": 0.6}),
    ("Linn Tom Lo", f"{SF}10-tom14.flac", SF3007, linn, 41, {"length": 0.8}),
    ("Linn Conga", f"{V}Conga_HitN_v2_rr2_Sum.wav", VCSL, linn, 63, {}),
    ("Linn Cowbell", f"{SF}42-cowbell4.flac", SF3007, linn, 56, {}),
    ("Linn Tambourine", f"{SF}50-tambourine.flac", SF3007, linn, 54, {}),
    ("Gated Room Snare", f"{BP}38-Pearl-14-Snare-5.wav", BLACK_PEARL, gated, 38, {"length": 0.4}),
    ("Gated Room Tom Hi", f"{BP}45-Pearl-12-Tom-5.wav", BLACK_PEARL, gated, 48, {"length": 0.5}),
    ("Gated Room Tom Lo", f"{BP}41-Pearl-16-FloorTom-5.wav", BLACK_PEARL, gated, 41, {"length": 0.6}),
    ("Pop Snare", f"{BP}38-Pearl-14-Snare-4.wav", BLACK_PEARL, lambda x: d.sat(room(x, 0.7, 0.3) * 1.3, 1.3), 38, {"length": 0.7}),
    ("Disco Snare", f"{GM}Snare-Med.wav", GMROCK, lambda x: room(x, 0.35, 0.18), 38, {"length": 0.5, "lp": 9000}),
    ("Disco Kick", f"{GM}Kick-Med.wav", GMROCK, None, 36, {"length": 0.45, "hp": 40}),
    ("Funk Rim", f"{SF}45-rimclick12-2.flac", SF3007, None, 37, {"semitones": 2.0}),
    ("Afrobeats Rim", f"{BP}37-Pearl-14-SideStick-4.wav", BLACK_PEARL, lambda x: room(x, 0.3, 0.15), 37, {"semitones": 5.0}),
    ("Dembow Snare", f"{GM}Snare-Hard.wav", GMROCK, lambda x: d.sat(room(x, 0.4, 0.25), 1.6), 38, {"length": 0.5, "semitones": 2.0, "hp": 120}),
]:
    add(name, note, src, credit, fx=fx, **kw)


# --- Synthesized Latin / African pieces with no redistributable recording ---------------------


def guira(teeth=16, spacing=0.012, seed=81):
    """Güira (metal scraper): metallic grains, brighter than a guiro."""
    grains = []
    for i in range(teeth):
        g = d.filt(d.noise(0.02, seed + i), "bp", (3500, 11000)) * d.exp_env(0.02, 0.014)
        ring = sum(d.resonator(f, 0.03, 0.03) for f in (4200, 6100, 7900)) * 0.15
        grains.append(np.pad(d.mix(g, ring), (int(i * spacing * d.SR), 0)))
    x = d.mix(*grains)
    return x * np.linspace(0.7, 1.0, len(x))


def talking_drum(f0=170.0, f1=260.0, glide=0.12, decay=0.5):
    seconds = decay * 1.2
    body = np.sin(d.sweep_phase(f0, f1, glide, seconds)) * d.exp_env(seconds, decay)
    over = np.sin(2.3 * d.sweep_phase(f0, f1, glide, seconds)) * d.exp_env(seconds, decay * 0.4) * 0.3
    hit = d.filt(d.noise(0.01, 91), "bp", (400, 3000)) * d.exp_env(0.01, 0.008) * 0.4
    return d.mix(body, over, hit)


for name, note, fn in [
    ("Guira Long", 74, lambda: guira(16, 0.012)),
    ("Guira Short", 73, lambda: guira(5, 0.010, seed=95)),
    ("Talking Drum Up", 63, lambda: talking_drum(150, 250, 0.1)),
    ("Talking Drum Down", 64, lambda: talking_drum(260, 150, 0.12)),
    ("Talking Drum Hit", 62, lambda: talking_drum(210, 205, 0.05, 0.3)),
]:
    SAMPLES[name] = Sample(note=note, synth=fn)
