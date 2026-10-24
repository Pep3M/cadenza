"""Cadenza drum lab: synthesized drum-machine voices (clean-room recipes) rendered to
mono 48 kHz arrays. Every function returns float64 numpy arrays; `finish` trims and
normalizes. Recipes: docs/design/drum-kit-library.md."""

import numpy as np
from scipy import signal

SR = 48_000


def t_axis(seconds):
    return np.arange(int(seconds * SR)) / SR


def rng(seed):
    return np.random.default_rng(seed)


def noise(seconds, seed=1):
    return rng(seed).uniform(-1, 1, int(seconds * SR))


def exp_env(seconds, decay_s, attack_s=0.0):
    """Exponential decay reaching -60 dB at decay_s, optional linear attack."""
    t = t_axis(seconds)
    env = np.exp(-6.907755 * t / max(decay_s, 1e-4))
    if attack_s > 0:
        env *= np.clip(t / attack_s, 0, 1)
    return env


def sweep_phase(f_start, f_end, sweep_s, seconds, curve="exp"):
    """Phase (radians) of a sine gliding f_start -> f_end with a time constant."""
    t = t_axis(seconds)
    if curve == "exp":
        f = f_end + (f_start - f_end) * np.exp(-t / max(sweep_s, 1e-5))
    else:
        f = np.where(t < sweep_s, f_start + (f_end - f_start) * t / sweep_s, f_end)
    return 2 * np.pi * np.cumsum(f) / SR


def sos(kind, freq, order=2, q=None):
    if kind == "bp":
        lo, hi = freq
        return signal.butter(order, [lo, hi], btype="bandpass", fs=SR, output="sos")
    return signal.butter(order, freq, btype={"lp": "lowpass", "hp": "highpass"}[kind], fs=SR, output="sos")


def filt(x, kind, freq, order=2):
    return signal.sosfilt(sos(kind, freq, order), x)


def peak_bp(x, freq, q):
    b, a = signal.iirpeak(freq, q, fs=SR)
    return signal.lfilter(b, a, x)


def resonator(freq, decay_s, seconds, drop=0.0, drop_s=0.01, phase=0.0):
    """A decaying sine (bridged-T style) with an optional small pitch drop."""
    ph = sweep_phase(freq * (1 + drop), freq, drop_s, seconds) + phase
    return np.sin(ph) * exp_env(seconds, decay_s)


def sat(x, drive=1.0):
    """tanh saturation, gain compensated to unity peak for small signals."""
    if drive <= 0:
        return x
    return np.tanh(x * drive) / np.tanh(drive)


def clip(x, level=0.7):
    return np.clip(x, -level, level) / level


def bitcrush(x, bits=12, rate=None):
    y = x
    if rate:
        step = SR / rate
        idx = (np.floor(np.arange(len(y)) / step) * step).astype(int)
        y = y[np.clip(idx, 0, len(y) - 1)]
    q = 2 ** (bits - 1)
    return np.round(y * q) / q


def reverb_ir(decay_s, seconds=None, seed=7, damp_hz=6000, predelay_s=0.0):
    seconds = seconds or decay_s * 1.1
    ir = noise(seconds, seed) * exp_env(seconds, decay_s)
    ir = filt(ir, "lp", damp_hz, 1)
    ir = np.concatenate([np.zeros(int(predelay_s * SR)), ir])
    return ir / np.sqrt(np.sum(ir ** 2))


def reverb(x, decay_s, wet=0.3, damp_hz=6000, seed=7, predelay_s=0.01):
    ir = reverb_ir(decay_s, seed=seed, damp_hz=damp_hz, predelay_s=predelay_s)
    w = signal.fftconvolve(x, ir)
    out = np.zeros(len(w))
    out[: len(x)] += x * (1 - wet)
    return out + w * wet * 0.35


def gate(x, hold_s, release_s):
    n = len(x)
    t = np.arange(n) / SR
    g = np.where(t < hold_s, 1.0, np.exp(-6.9 * (t - hold_s) / max(release_s, 1e-3)))
    return x * g


def pad_to(x, seconds):
    n = int(seconds * SR)
    return np.pad(x, (0, max(0, n - len(x))))[: max(n, 1)]


def mix(*parts):
    n = max(len(p) for p in parts)
    out = np.zeros(n)
    for p in parts:
        out[: len(p)] += p
    return out


def finish(x, peak_db=-1.0, floor_db=-66.0, fade_ms=4.0):
    """Remove DC, trim the tail under floor_db (relative to peak) with a short fade,
    normalize the peak."""
    x = np.asarray(x, dtype=np.float64)
    x = signal.sosfilt(signal.butter(1, 12, "highpass", fs=SR, output="sos"), x)
    peak = np.max(np.abs(x)) or 1.0
    thr = peak * 10 ** (floor_db / 20)
    above = np.nonzero(np.abs(x) > thr)[0]
    end = (above[-1] + 1) if len(above) else 1
    x = x[:end].copy()
    fade = min(int(fade_ms * SR / 1000), len(x))
    if fade > 1:
        x[-fade:] *= np.linspace(1, 0, fade)
    peak = np.max(np.abs(x)) or 1.0
    return x * (10 ** (peak_db / 20) / peak)


# --- metal (808/909 hats and cymbals) -----------------------------------------------

METAL_HZ = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0]


def metal(seconds, scale=1.0, seed=3):
    t = t_axis(seconds)
    r = rng(seed)
    x = np.zeros(len(t))
    for f in METAL_HZ:
        x += signal.square(2 * np.pi * f * scale * t + r.uniform(0, 2 * np.pi))
    return x / len(METAL_HZ)


# --- voices ------------------------------------------------------------------------


def kick_808(tune=50.0, decay=0.6, drop=0.6, drop_s=0.012, click=0.15, drive=0.0, seconds=None):
    seconds = seconds or decay * 1.2 + 0.05
    body = resonator(tune, decay, seconds, drop=drop, drop_s=drop_s)
    c = filt(noise(0.006, 11), "lp", 3000) * exp_env(0.006, 0.004) * click
    x = mix(body, c)
    return sat(x, drive) if drive else x


def bass_808(tune=43.65, decay=2.2, knock=1.0, knock_s=0.02, drive=1.5, click=0.25, seconds=None):
    """Trap 808: long sine with a pitch knock and saturation for harmonics."""
    seconds = seconds or decay * 1.05 + 0.05
    ph = sweep_phase(tune * (1 + knock), tune, knock_s, seconds)
    env = exp_env(seconds, decay) * np.clip(t_axis(seconds) / 0.002, 0, 1)
    x = np.sin(ph) * env
    c = filt(noise(0.005, 21), "bp", (1500, 6000)) * exp_env(0.005, 0.004) * click
    return sat(mix(x, c), drive)


def kick_909(start=260.0, end=52.0, sweep=0.03, decay=0.38, click=0.35, drive=0.6, seconds=None):
    seconds = seconds or decay * 1.25 + 0.03
    ph = sweep_phase(start, end, sweep, seconds)
    body = np.sin(ph) * exp_env(seconds, decay, attack_s=0.0008)
    # 909 body is a triangle-ish wave shaped toward sine: a little 2nd/3rd harmonic.
    body = sat(body, 1.2)
    c = filt(noise(0.008, 5), "bp", (1200, 9000)) * exp_env(0.008, 0.006) * click
    imp = np.zeros(int(0.002 * SR))
    imp[0] = click * 1.5
    imp = filt(imp, "lp", 6000)
    x = mix(body, c, imp)
    return sat(x, drive)


def kick_707(seconds=0.35):
    return kick_909(start=320, end=64, sweep=0.012, decay=0.18, click=0.6, drive=1.0, seconds=seconds)


def kick_cr78(seconds=0.4):
    return kick_808(tune=54, decay=0.28, drop=0.25, drop_s=0.01, click=0.0)


def kick_rumble(base, verb_decay=1.4, lp=180, amount=0.6, duck_s=0.06):
    """Techno rumble: the kick through a long dark reverb, ducked under the kick."""
    ir = reverb_ir(verb_decay, seed=17, damp_hz=900, predelay_s=0.02)
    tail = signal.fftconvolve(sat(base, 3.0), ir)
    tail = filt(tail, "lp", lp, 4)
    tail = filt(tail, "hp", 35, 2)
    t = np.arange(len(tail)) / SR
    duck = np.clip((t - duck_s) / 0.05, 0, 1)
    tail *= duck
    tail /= np.max(np.abs(tail)) + 1e-9
    return mix(base, tail * amount)


def snare_808(tones=(238.0, 476.0), tone_decay=0.2, snappy=0.7, noise_decay=0.38, seconds=0.6):
    t1 = resonator(tones[0], tone_decay, seconds, drop=0.05)
    t2 = resonator(tones[1], tone_decay * 0.7, seconds, drop=0.05) * 0.6
    n = filt(noise(seconds, 9), "hp", 1800) * exp_env(seconds, noise_decay)
    n = filt(n, "lp", 9000)
    return mix(t1, t2, n * snappy)


def snare_909(body=190.0, decay=0.42, snappy=0.85, tone_decay=0.16, seconds=0.7):
    b1 = resonator(body, tone_decay, seconds, drop=0.35, drop_s=0.008)
    b2 = resonator(body * 1.75, tone_decay * 0.6, seconds, drop=0.3, drop_s=0.006) * 0.5
    n = noise(seconds, 13)
    n = filt(filt(n, "hp", 900), "lp", 10000) * exp_env(seconds, decay)
    return mix(b1, b2, n * snappy)


def noise_snare(decay=0.42, hp=600, lp=7000, body=200.0, seconds=0.5, seed=31):
    n = filt(filt(noise(seconds, seed), "hp", hp), "lp", lp) * exp_env(seconds, decay)
    b = resonator(body, 0.05, seconds, drop=0.5, drop_s=0.005) * 0.6
    return mix(n, b)


def clap(bursts=4, spacing=0.011, tail=0.4, center=1150, width=(700, 2600), seconds=0.6, seed=4):
    x = np.zeros(int(seconds * SR))
    n = noise(seconds, seed)
    for i in range(bursts):
        s = int(i * spacing * SR * rng(seed + i).uniform(0.85, 1.15))
        e = np.zeros(len(x))
        seg = exp_env(seconds, 0.012)
        e[s:] = seg[: len(x) - s]
        x += e * (1.0 if i < bursts - 1 else 1.0)
    s = int((bursts - 1) * spacing * SR)
    tl = np.zeros(len(x))
    tl[s:] = exp_env(seconds, tail)[: len(x) - s] * 0.55
    x = (x + tl) * n
    x = filt(x, "bp", width, 2)
    x = peak_bp(x, center, 1.2) * 0.6 + x * 0.6
    return x


def snap(seconds=0.25, seed=41):
    n = filt(noise(seconds, seed), "bp", (1800, 7000)) * exp_env(seconds, 0.05)
    tone = resonator(2100, 0.03, seconds) * 0.5
    return mix(n, tone)


def hat(open_s=0.05, kind="808", hp=7000, seed=3, crush=None):
    seconds = open_s * 1.3 + 0.02
    m = metal(seconds, seed=seed)
    if kind == "909":
        m = m * 0.6 + noise(seconds, seed + 1) * 0.4
    x = filt(m, "bp", (hp * 0.9, min(hp * 2.4, 21000)), 2)
    x = filt(x, "hp", hp, 2)
    if crush:
        # 909-style: a low-bit cymbal sample, then the analog VCA envelope.
        x = bitcrush(x / (np.max(np.abs(x)) + 1e-9), *crush)
        x = filt(x, "hp", hp * 0.8, 2)
    return x * exp_env(seconds, open_s, attack_s=0.0005)


def cymbal(decay=2.5, kind="crash", seed=6, crush=None):
    seconds = decay * 1.05
    m = metal(seconds, scale=1.47, seed=seed) * 0.5 + metal(seconds, scale=2.13, seed=seed + 2) * 0.5
    n = noise(seconds, seed + 5)
    if kind == "crash":
        src = filt(m * 0.5 + n * 0.5, "hp", 3000, 2)
        src = filt(src, "lp", 14000, 2)
        env = exp_env(seconds, decay, attack_s=0.002)
    else:  # ride: wash plus a bell ping
        src = filt(m * 0.6 + n * 0.4, "hp", 4000, 2)
        src = filt(src, "lp", 13000, 2)
        env = exp_env(seconds, decay)
    if crush:
        src = bitcrush(src / (np.max(np.abs(src)) + 1e-9), *crush)
    x = src * env
    if kind == "ride":
        bell = sum(resonator(f, decay * 0.6, seconds) for f in (3013, 4521, 5233)) / 3
        x = x * 0.6 + bell * 0.5
    return x


def cowbell_808(decay=0.3, seconds=None):
    seconds = seconds or decay * 1.3
    t = t_axis(seconds)
    x = signal.square(2 * np.pi * 540 * t) + signal.square(2 * np.pi * 800 * t)
    env = exp_env(seconds, decay) * 0.7 + exp_env(seconds, 0.03) * 0.3
    x = peak_bp(x * env, 850, 3.5)
    return filt(x, "hp", 300)


def rim_808(seconds=0.12):
    a = resonator(1680, 0.025, seconds)
    b = resonator(450, 0.03, seconds) * 0.6
    c = filt(noise(0.004, 2), "hp", 2000) * exp_env(0.004, 0.003)
    return mix(a, b, c)


def clave(freq=2500.0, decay=0.04, seconds=0.12):
    return resonator(freq, decay, seconds)


def tom(freq=110.0, decay=0.35, drop=0.5, drop_s=0.05, noise_amt=0.15, seconds=None, seed=8):
    seconds = seconds or decay * 1.2
    b = resonator(freq, decay, seconds, drop=drop, drop_s=drop_s)
    n = filt(noise(seconds, seed), "lp", 4000) * exp_env(seconds, 0.03) * noise_amt
    return mix(b, n)


def simmons_tom(freq=150.0, decay=0.5, seconds=None):
    seconds = seconds or decay * 1.2
    b = resonator(freq, decay, seconds, drop=1.2, drop_s=0.12)
    n = filt(noise(seconds, 19), "bp", (300, 3000)) * exp_env(seconds, 0.06) * 0.25
    return mix(b, n)


def conga_808(freq=330.0, decay=0.18):
    return tom(freq, decay, drop=0.1, drop_s=0.01, noise_amt=0.05)


def maracas(decay=0.05, seconds=0.12, seed=12):
    return filt(noise(seconds, seed), "hp", 5000) * exp_env(seconds, decay, attack_s=0.004)


def shaker(decay=0.09, attack=0.018, seconds=0.2, seed=14, bright=6500):
    """A shaker stroke: grainy noise with a soft attack."""
    r = rng(seed)
    n = noise(seconds, seed) * (r.uniform(0, 1, int(seconds * SR)) ** 3 * 2.5)
    x = filt(filt(n, "bp", (bright * 0.5, min(bright * 2.6, 21000)), 2), "hp", 2500, 1)
    return x * exp_env(seconds, decay, attack_s=attack)


def tambourine(decay=0.25, seconds=0.4, seed=16):
    jingles = sum(resonator(f, decay * r, seconds) for f, r in ((5612, 1.0), (7203, 0.8), (8901, 0.7), (10520, 0.6)))
    n = filt(noise(seconds, seed), "hp", 6000) * exp_env(seconds, decay * 0.6)
    hit = filt(noise(0.01, seed + 1), "bp", (200, 1200)) * exp_env(0.01, 0.006) * 0.4
    return mix(jingles * 0.15, n, hit)


def zap(f0=3000.0, f1=80.0, sweep=0.06, decay=0.25):
    seconds = decay * 1.2
    return np.sin(sweep_phase(f0, f1, sweep, seconds)) * exp_env(seconds, decay)


def metal_clank(seed=23, decay=0.35, seconds=0.5):
    fs = [431, 1017, 1733, 2879, 4111]
    x = sum(resonator(f * rng(seed).uniform(0.95, 1.05), decay * rng(seed + i).uniform(0.4, 1), seconds) for i, f in enumerate(fs))
    n = filt(noise(0.01, seed), "hp", 1500) * exp_env(0.01, 0.008)
    return mix(x / len(fs), n * 0.5)


def gated_reverb(x, decay=2.5, hold=0.15, release=0.15, wet=0.7, damp=7000):
    return gate(reverb(x, decay, wet=wet, damp_hz=damp, predelay_s=0.008), hold, release)


def lofi(x, bits=12, rate=26040, lp=None):
    y = bitcrush(x, bits, rate)
    return filt(y, "lp", lp or rate * 0.45, 4)


def write(path, x, subtype="PCM_16"):
    import soundfile as sf
    sf.write(path, finish(x).astype(np.float32), SR, subtype=subtype, format="FLAC")
