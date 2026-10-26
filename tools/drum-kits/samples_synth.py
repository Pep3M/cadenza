"""Synthesized factory samples (drum lab recipes). name -> Sample."""

import numpy as np

import drumlab as d
from kitgen import Sample

F1 = 43.65  # trap 808s tuned to F1: the pad's tune knob moves them to the song key
G1 = 49.0


def S(note, fn, **kw):
    return Sample(note=note, synth=fn, **kw)


def layered_big_room():
    body = d.kick_909(start=300, end=G1 * 1.0, sweep=0.022, decay=0.45, click=0.5, drive=1.6)
    top = d.filt(d.noise(0.02, 51), "bp", (2500, 9000)) * d.exp_env(0.02, 0.012) * 0.5
    return d.sat(d.mix(body, top), 1.4)


def dembow_snare():
    # Snappy, bright, short room: the 2000s reggaeton snare on the "tun" offbeats.
    x = d.snare_909(body=215, decay=0.26, snappy=1.0, tone_decay=0.1)
    return d.reverb(x, 0.35, wet=0.25, damp_hz=7000)


def timbal(freq=640.0, decay=0.45, ring=0.6):
    """Digital timbale: a bright metallic-shell drum (pitched skin + shell ring)."""
    seconds = decay * 1.2
    skin = d.resonator(freq, decay * 0.5, seconds, drop=0.08, drop_s=0.004)
    shell = sum(d.resonator(freq * r, decay * a, seconds) for r, a in ((2.31, 1.0), (3.47, 0.7), (5.12, 0.5))) / 3
    stick = d.filt(d.noise(0.006, 61), "hp", 2500) * d.exp_env(0.006, 0.004)
    return d.mix(skin, shell * ring, stick * 0.6)


def guiro_scrape(teeth=9, spacing=0.018):
    ticks = [np.pad(d.filt(d.noise(0.02, 40 + i), "bp", (2000, 6000)) * d.exp_env(0.02, 0.012), (int(i * spacing * d.SR), 0)) for i in range(teeth)]
    return d.mix(*ticks)


def phased(x, rate=0.6, stages=4, depth=0.7):
    """A swept all-pass phaser (French house hats)."""
    n = len(x)
    t = np.arange(n) / d.SR
    fc = 900 * (2 ** (depth * 3 * (0.5 + 0.5 * np.sin(2 * np.pi * rate * t))))
    y = x.copy()
    for _ in range(stages):
        a = (np.tan(np.pi * fc / d.SR) - 1) / (np.tan(np.pi * fc / d.SR) + 1)
        out = np.zeros(n)
        x1 = y1 = 0.0
        for i in range(n):
            out[i] = a[i] * y[i] + x1 - a[i] * y1
            x1, y1 = y[i], out[i]
        y = out
    return (x + y) * 0.5


def phonk_cowbell():
    return d.sat(d.cowbell_808(decay=0.42) * 1.5, 2.5)


def memphis_hat(open_s=0.05):
    return d.lofi(d.hat(open_s, "808", hp=6500, seed=9), bits=8, rate=22050, lp=9000)


def future_clap():
    x = d.clap(bursts=4, spacing=0.009, tail=0.18, center=1500, width=(900, 5000), seed=71)
    s = d.snare_909(body=200, decay=0.16, snappy=0.9)
    return d.reverb(d.mix(x, s * 0.4), 0.6, wet=0.3, damp_hz=9000)


def gated_snare_80s():
    return d.gated_reverb(d.snare_909(body=180, decay=0.2, snappy=0.8), decay=2.5, hold=0.16, release=0.14, wet=0.75)


def industrial_kick():
    return d.lofi(d.sat(d.kick_909(start=220, end=48, sweep=0.03, decay=0.35, click=0.6, drive=4.0), 2.0), bits=10, rate=32000, lp=12000)


SAMPLES = {
    # --- 808 ------------------------------------------------------------------------
    "808 Kick": S(36, lambda: d.kick_808(tune=52, decay=0.45, drop=0.7, drop_s=0.01, click=0.2)),
    "808 Kick Long": S(36, lambda: d.kick_808(tune=48, decay=1.2, drop=0.6, drop_s=0.012, click=0.15)),
    "808 Snare": S(38, lambda: d.snare_808()),
    "808 Clap": S(39, lambda: d.clap(bursts=4, spacing=0.012, tail=0.5, center=1100)),
    "808 Rim": S(37, d.rim_808),
    "808 Clave": S(75, d.clave),
    "808 Cowbell": S(56, lambda: d.cowbell_808(0.32)),
    "808 Closed Hat": S(42, lambda: d.hat(0.2, "808", hp=7500)),
    "808 Open Hat": S(46, lambda: d.hat(0.9, "808", hp=7000)),
    "808 Cymbal": S(49, lambda: d.cymbal(2.4, "crash", seed=8)),
    "808 Conga Hi": S(62, lambda: d.conga_808(370, 0.16)),
    "808 Conga Mid": S(63, lambda: d.conga_808(280, 0.2)),
    "808 Conga Lo": S(64, lambda: d.conga_808(190, 0.24)),
    "808 Tom Lo": S(41, lambda: d.tom(90, 0.45, drop=0.25, drop_s=0.04, noise_amt=0.05)),
    "808 Tom Mid": S(45, lambda: d.tom(130, 0.38, drop=0.25, drop_s=0.04, noise_amt=0.05)),
    "808 Tom Hi": S(48, lambda: d.tom(180, 0.32, drop=0.25, drop_s=0.04, noise_amt=0.05)),
    "808 Maracas": S(70, lambda: d.maracas(decay=0.08, seconds=0.15)),
    # Trap 808 basses (pad tune moves them to the key).
    "Trap 808 F": S(35, lambda: d.bass_808(F1, decay=2.2, knock=1.0, knock_s=0.018, drive=1.6)),
    "Trap 808 Clean F": S(35, lambda: d.bass_808(F1, decay=1.8, knock=0.3, knock_s=0.01, drive=0.6, click=0.1)),
    "Rage 808 F": S(35, lambda: d.clip(d.bass_808(F1, decay=1.8, knock=1.5, knock_s=0.02, drive=3.0), 0.55)),
    "Drill 808 F": S(35, lambda: d.bass_808(F1, decay=2.6, knock=0.5, knock_s=0.025, drive=2.0)),
    "Phonk 808 F": S(35, lambda: d.lofi(d.clip(d.bass_808(F1, decay=1.4, knock=1.2, drive=3.5), 0.6), bits=10, rate=30000, lp=9000)),
    # --- 909 ------------------------------------------------------------------------
    "909 Kick": S(36, lambda: d.kick_909()),
    "909 Kick Deep": S(36, lambda: d.kick_909(start=200, end=46, sweep=0.035, decay=0.6, click=0.2, drive=0.4)),
    "909 Kick Hard": S(36, lambda: d.kick_909(start=320, end=55, sweep=0.022, decay=0.3, click=0.55, drive=2.2)),
    "909 Snare": S(38, lambda: d.snare_909()),
    "909 Snare Tight": S(38, lambda: d.snare_909(body=210, decay=0.3, snappy=0.9, tone_decay=0.1)),
    "909 Clap": S(39, lambda: d.clap(bursts=4, spacing=0.009, tail=0.32, center=1250, seed=6)),
    "909 Rim": S(37, lambda: d.mix(d.resonator(1900, 0.02, 0.1), d.resonator(520, 0.025, 0.1) * 0.5)),
    "909 Closed Hat": S(42, lambda: d.hat(0.2, "909", hp=8000, crush=(6, None))),
    "909 Open Hat": S(46, lambda: d.hat(0.8, "909", hp=7500, crush=(6, None))),
    "909 Crash": S(49, lambda: d.cymbal(2.2, "crash", seed=12, crush=(6, None))),
    "909 Ride": S(51, lambda: d.cymbal(2.0, "ride", seed=14, crush=(6, None))),
    "909 Tom Lo": S(41, lambda: d.tom(95, 0.4, drop=0.6, drop_s=0.06, noise_amt=0.25)),
    "909 Tom Mid": S(45, lambda: d.tom(135, 0.34, drop=0.6, drop_s=0.06, noise_amt=0.25)),
    "909 Tom Hi": S(48, lambda: d.tom(185, 0.3, drop=0.6, drop_s=0.06, noise_amt=0.25)),
    # --- 707 / CR-78 ----------------------------------------------------------------
    "707 Kick": S(36, d.kick_707),
    "707 Snare": S(38, lambda: d.snare_909(body=230, decay=0.3, snappy=0.7, tone_decay=0.1)),
    "707 Clap": S(39, lambda: d.clap(bursts=3, spacing=0.008, tail=0.24, center=1400, seed=9)),
    "CR78 Kick": S(36, d.kick_cr78),
    "CR78 Snare": S(38, lambda: d.mix(d.resonator(310, 0.08, 0.3, drop=0.05), d.filt(d.noise(0.3, 33), "bp", (2000, 8000)) * d.exp_env(0.3, 0.2) * 0.8)),
    "CR78 Hat": S(42, lambda: d.filt(d.noise(0.2, 35), "bp", (7000, 14000)) * d.exp_env(0.2, 0.15)),
    "CR78 Guiro": S(74, lambda: guiro_scrape()),
    # --- techno / industrial ------------------------------------------------------------
    "Rumble Kick": S(36, lambda: d.kick_rumble(d.kick_909(start=240, end=47, sweep=0.03, decay=0.32, click=0.4, drive=1.4))),
    "Industrial Kick": S(36, industrial_kick),
    "Noise Snare": S(38, lambda: d.sat(d.noise_snare(decay=0.28), 2.0)),
    "Metal Clank": S(37, d.metal_clank),
    "Crushed Hat": S(42, lambda: d.lofi(d.hat(0.18, "909", hp=6000, seed=21), bits=5, rate=16000, lp=12000)),
    # --- EDM / pop ------------------------------------------------------------------
    "Big Room Kick": S(36, layered_big_room),
    "Future Clap": S(39, future_clap),
    "Finger Snap": S(39, d.snap),
    "Gated Snare": S(38, gated_snare_80s),
    "Simmons Tom Lo": S(41, lambda: d.simmons_tom(110, 0.55)),
    "Simmons Tom Mid": S(45, lambda: d.simmons_tom(150, 0.5)),
    "Simmons Tom Hi": S(48, lambda: d.simmons_tom(200, 0.45)),
    "Laser Zap": S(49, lambda: d.zap()),
    "Synth Tambourine": S(54, lambda: d.tambourine()),
    "Synth Triangle": S(81, lambda: d.mix(*(d.resonator(f, 1.6 * a, 2.0) * a for f, a in ((4630, 1.0), (6210, 0.6), (8140, 0.4), (9850, 0.25))))),
    "French Kick": S(36, lambda: d.lofi(d.sat(d.mix(d.kick_909(start=260, end=54, decay=0.32, click=0.4, drive=1.0), d.kick_707() * 0.5), 2.5), bits=12, rate=32000, lp=11000)),
    "French Clap": S(39, lambda: d.lofi(d.sat(d.clap(bursts=4, spacing=0.010, tail=0.2, center=1300, seed=8) * 3.0, 3.0), bits=12, rate=32000, lp=12000)),
    "Phaser Hat": S(46, lambda: phased(d.hat(0.3, "909", hp=6500, seed=17, crush=(6, None)))),
    # --- reggaeton / phonk ------------------------------------------------------------
    "Digital Timbal Hi": S(65, lambda: timbal(720, 0.4)),
    "Digital Timbal Lo": S(66, lambda: timbal(520, 0.5)),
    "Phonk Cowbell": S(56, phonk_cowbell),
    "Memphis Closed Hat": S(42, lambda: memphis_hat(0.18)),
    "Memphis Open Hat": S(46, lambda: memphis_hat(0.7)),
}
