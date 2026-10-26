"""Kit builder library used by `build.py`: sample specs, rendering, level matching, code
generation.

Inputs: `samples_*.py` (SAMPLES: name -> Sample spec) and `kits_*.py` (KITS).
Outputs:
- <repo>/assets/samples/<name>.flac (48 kHz, 16-bit, mono unless the spec says stereo)
- <repo>/assets/samples/CREDITS.md (per file: source, author, license)
- <repo>/crates/dsp/src/presets/drum_sampler.rs (FactoryPreset statics + KIT_SAMPLES)

Source recordings are read from $CADENZA_KIT_SOURCES or ./sources (fetch_sources.sh).
"""

import json
import os
import subprocess
import sys
import textwrap
from dataclasses import dataclass, field

import numpy as np
import soundfile as sf
from scipy import signal

import drumlab as d

SR = d.SR
HERE = os.path.dirname(os.path.abspath(__file__))
SOURCES = os.environ.get("CADENZA_KIT_SOURCES", os.path.join(HERE, "sources"))


@dataclass
class Credit:
    source: str  # human name ("Versilian Community Sample Library")
    url: str
    author: str
    license: str  # SPDX-ish ("CC0-1.0", "GPL-2.0-or-later", "CC-BY-4.0")


CADENZA = Credit("Cadenza drum lab (synthesized)", "https://github.com/pep3marquez/cadenza", "Cadenza contributors", "GPL-3.0-or-later")


@dataclass
class Sample:
    """A factory sample: synthesized (`synth` returns an array) or cut from a source file."""

    note: int  # GM note it is meant for (list_factory_samples)
    synth: object = None  # callable -> np.ndarray (mono) or (n, 2)
    file: str = None  # path relative to sources/
    credit: Credit = None
    start: float = None  # seconds into the file; None = detect the onset
    length: float = None  # seconds kept (None = to the end / tail trim)
    gain_db: float = 0.0
    semitones: float = 0.0  # pitch shift by resampling (changes length)
    hp: float = None
    lp: float = None
    fx: object = None  # callable(np.ndarray) -> np.ndarray applied after cutting
    stereo: bool = False
    floor_db: float = -60.0
    fade_ms: float = 8.0
    channel: int = None  # pick one channel of a stereo source (None = mid)
    limit_db: float = 3.0  # transient limiting before normalization (one-shot "mastering")


def decode(path):
    """Any audio file -> float64 array (n, ch) at SR, via ffmpeg."""
    info = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries", "stream=channels", "-of", "csv=p=0", path],
        capture_output=True, text=True, check=True,
    )
    ch = int(info.stdout.strip().split(",")[0])
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-acodec", "pcm_f32le", "-ar", str(SR), "-"],
        capture_output=True, check=True,
    ).stdout
    x = np.frombuffer(raw, dtype=np.float32).astype(np.float64)
    return x.reshape(-1, ch)


def resolve(pattern):
    """A source path, globbing `*` / `**` (exactly one match required)."""
    import glob
    hits = sorted(glob.glob(os.path.join(SOURCES, pattern), recursive=True))
    assert len(hits) == 1, f"{pattern}: {len(hits)} matches {hits[:4]}"
    return hits[0]


def onset(x, thr_db=-30.0):
    """First sample within thr_db of the peak, backed off 1 ms."""
    env = np.abs(x)
    peak = env.max() or 1.0
    idx = np.nonzero(env > peak * 10 ** (thr_db / 20))[0]
    return max(0, (idx[0] if len(idx) else 0) - int(0.001 * SR))


def shift(x, semitones):
    if not semitones:
        return x
    ratio = 2 ** (semitones / 12)
    n = int(round(len(x) / ratio))
    return signal.resample_poly(x, 1000, int(round(1000 * ratio)), axis=0)[:n] if n else x


def render(s: Sample):
    if s.synth is not None:
        x = np.asarray(s.synth(), dtype=np.float64)
    else:
        x = decode(resolve(s.file))
        if not s.stereo:
            x = x[:, s.channel] if s.channel is not None else x.mean(axis=1)
        elif x.shape[1] == 1:
            x = np.repeat(x, 2, axis=1)
        mono = x if x.ndim == 1 else x.mean(axis=1)
        start = int(s.start * SR) if s.start is not None else onset(mono)
        x = x[start:]
        if s.length:
            x = x[: int(s.length * SR)]
    x = shift(x, s.semitones)
    if s.hp:
        x = signal.sosfilt(signal.butter(2, s.hp, "highpass", fs=SR, output="sos"), x, axis=0)
    if s.lp:
        x = signal.sosfilt(signal.butter(2, s.lp, "lowpass", fs=SR, output="sos"), x, axis=0)
    if s.fx:
        x = s.fx(x)
    return finish(x, s)


def limit(x, reduce_db, attack_ms=0.3, release_ms=12.0):
    """Peak limiter (lookahead = attack) taking reduce_db off the highest peaks."""
    if reduce_db <= 0:
        return x
    mono = np.abs(x) if x.ndim == 1 else np.abs(x).max(axis=1)
    thr = mono.max() * 10 ** (-reduce_db / 20)
    want = np.minimum(1.0, thr / np.maximum(mono, 1e-12))
    # Lookahead min over the attack window, then a smooth release.
    a = max(1, int(attack_ms * SR / 1000))
    padded = np.concatenate([want, np.ones(a)])
    from numpy.lib.stride_tricks import sliding_window_view
    look = sliding_window_view(padded, a + 1).min(axis=1)[: len(want)]
    rel = np.exp(-1.0 / (release_ms * SR / 1000))
    g = np.empty_like(look)
    cur = 1.0
    for i, v in enumerate(look):
        cur = v if v < cur else v + (cur - v) * rel
        g[i] = cur
    return x * (g if x.ndim == 1 else g[:, None])


def finish(x, s: Sample):
    """DC removal, tail trim under floor_db with a fade, short fade-in, peak -1 dBFS
    (+ gain_db, which is then baked in only when negative)."""
    x = signal.sosfilt(signal.butter(1, 15, "highpass", fs=SR, output="sos"), x, axis=0)
    mono = np.abs(x) if x.ndim == 1 else np.abs(x).max(axis=1)
    peak = mono.max() or 1.0
    above = np.nonzero(mono > peak * 10 ** (s.floor_db / 20))[0]
    end = (above[-1] + 1) if len(above) else 1
    x = limit(x[:end].copy(), s.limit_db)
    fade = min(int(s.fade_ms * SR / 1000), len(x))
    if fade > 1:
        ramp = np.linspace(1, 0, fade) ** 2
        x[-fade:] *= ramp if x.ndim == 1 else ramp[:, None]
    fin = min(int(0.0005 * SR), len(x))
    if fin > 1:
        ramp = np.linspace(0, 1, fin)
        x[:fin] *= ramp if x.ndim == 1 else ramp[:, None]
    peak = np.abs(x).max() or 1.0
    return x * (10 ** ((-1.0 + min(s.gain_db, 0.0)) / 20) / peak)


def write_samples(repo, samples, only=None):
    out = os.path.join(repo, "assets", "samples")
    os.makedirs(out, exist_ok=True)
    wanted = {f"{n}.flac" for n in samples}
    for f in os.listdir(out):
        if f.endswith(".flac") and f not in wanted:
            os.remove(os.path.join(out, f))
    stats = {}
    for name, s in samples.items():
        if only and name not in only:
            continue
        x = render(s)
        sf.write(os.path.join(out, f"{name}.flac"), x.astype(np.float32), SR, subtype="PCM_16", format="FLAC")
        stats[name] = (len(x) / SR, x.ndim)
    return stats


def credits_md(samples):
    by = {}
    for name, s in sorted(samples.items()):
        c = s.credit or CADENZA
        by.setdefault((c.source, c.url, c.author, c.license), []).append(name)
    lines = [
        "# Factory samples: credits and licenses",
        "",
        "The drum kits of Cadenza's `drum_sampler` use these files. They are shipped with the",
        "application (next to the binaries) and copied into the user's factory sample folder on",
        "first use. Each source below is redistributable under the license named; files marked",
        "*synthesized* were rendered from scratch for Cadenza and are covered by Cadenza's own",
        "license. Edits made to source recordings: trimmed, faded, normalized, converted to 48 kHz",
        "16-bit FLAC and, where noted in the kit descriptions, pitched, filtered, saturated or",
        "bit-reduced.",
        "",
    ]
    for (source, url, author, lic), names in sorted(by.items(), key=lambda kv: (kv[0][0] != CADENZA.source, kv[0][0])):
        lines += [f"## {source}", "", f"- Source: <{url}>", f"- Author: {author}", f"- License: {lic}", ""]
        lines += [f"- {n}.flac" for n in names]
        lines.append("")
    return "\n".join(lines)


def rs_str(s):
    return json.dumps(s, ensure_ascii=False)


def zone_rs(note, sample, o):
    z = f"FactoryZone::pad({rs_str(sample + '.flac')}, {note})"
    for key, fmt in (
        ("gain", "gain({})"), ("pan", "pan({:.2f})"), ("choke", "choke({})"), ("decay", "decay({})"),
        ("tune", "tune({})"), ("attack", "attack({})"), ("rr", "rr({})"),
    ):
        if key in o and o[key] not in (None, 0, 0.0):
            v = o[key]
            z += "." + (fmt.format(v) if "{:" in fmt else fmt.format(f"{float(v):.1f}" if isinstance(v, float) else v))
    if "vel" in o:
        z += f".vel({o['vel'][0]:.2f}, {o['vel'][1]:.2f})"
    if o.get("reverse"):
        z += ".reversed()"
    return z


GAINS_FILE = os.path.join(HERE, "kit_gains.json")


def kit_gain(name):
    try:
        return json.load(open(GAINS_FILE)).get(name)
    except FileNotFoundError:
        return None


def kit_rs(k):
    desc = k["description"]
    assert '"' not in desc and "\\" not in desc, k["name"]
    wrapped = textwrap.wrap(desc, 84)
    desc_rs = '"' + " \\\n\t\t ".join(wrapped) + '"'
    params = dict(k.get("params", {}))
    if kit_gain(k["name"]) is not None:
        params["gain"] = kit_gain(k["name"])
    params = ", ".join(f"({rs_str(p)}, {float(v):.1f})" for p, v in params.items())
    zones = ",\n\t\t\t".join(zone_rs(n, s, o) for n, s, o in expand(k["pads"]))
    return (
        f"\tsampled(\n\t\t{rs_str(k['name'])},\n\t\t{desc_rs},\n\t\t&[{params}],\n\t\t&[\n\t\t\t{zones},\n\t\t],\n\t)"
        f"\n\t.category({rs_str(k['category'])})"
    )


def expand(pads):
    """Pads -> zones: a pad is (note, sample, opts) or (note, [layers], opts) where a layer
    is a sample name (round robin when opts has 'rr') or (sample, vel_low, vel_high)."""
    out = []
    for note, sample, *rest in pads:
        o = dict(rest[0]) if rest else {}
        if isinstance(sample, str):
            out.append((note, sample, o))
            continue
        for layer in sample:
            lo = dict(o)
            if isinstance(layer, tuple):
                name, vl, vh = layer
                lo["vel"] = (vl, vh)
            else:
                name = layer
            out.append((note, name, lo))
    return out


def write_rust(repo, kits, samples):
    used = sorted({s for k in kits for _, s, _ in expand(k["pads"])})
    missing = [s for s in used if s not in samples]
    assert not missing, f"kits use unknown samples: {missing}"
    body = ",\n".join(kit_rs(k) for k in kits)
    listing = ",\n".join(f"\t({rs_str(n + '.flac')}, {samples[n].note})" for n in sorted(samples))
    code = f"""//! Factory presets of the `drum_sampler`: the synthesized kits plus the genre kits made of
//! the bundled samples (`assets/samples`, credits in `assets/samples/CREDITS.md`).
//! Generated by the kit builder (see `docs/design/drum-kit-library.md`): edit the specs
//! there and regenerate rather than editing by hand.

use super::{{FactoryPreset, FactoryZone, SYNTH_KIT, TIGHT_SYNTH_KIT, sampled}};

/// The bundled samples (file name, the General MIDI drum note they are made for).
pub static KIT_SAMPLES: [(&str, u8); {len(samples)}] = [
{listing},
];

pub(super) static PRESETS: [FactoryPreset; {len(kits) + 2}] = [
	SYNTH_KIT,
	TIGHT_SYNTH_KIT,
{body},
];
"""
    path = os.path.join(repo, "crates", "dsp", "src", "presets", "drum_sampler.rs")
    open(path, "w").write(code)
    return path


# --- kit builder -------------------------------------------------------------------

# role: (note, loudness target in dB relative to the kick, pan, choke). A pad's gain is
# computed so its hit lands on the target (see hit_loudness); kit opts "gain" offset it.
ROLES = {
    "kick": (36, 0.0, 0.0, 0), "kick2": (35, -1.0, 0.0, 0),
    "snare": (38, -1.0, 0.0, 0), "snare2": (40, -4.0, 0.0, 0),
    "clap": (39, -2.0, 0.0, 0), "rim": (37, -8.0, 0.0, 0),
    "closed_hat": (42, -12.0, 0.15, 1), "pedal_hat": (44, -15.0, 0.15, 1), "open_hat": (46, -11.0, 0.15, 1),
    "crash": (49, -9.0, -0.3, 0), "crash2": (57, -9.0, 0.3, 0), "ride": (51, -12.0, 0.3, 0),
    "ride_bell": (53, -12.0, 0.3, 0), "splash": (55, -11.0, -0.2, 0), "china": (52, -10.0, 0.35, 0),
    "low_tom": (41, -4.0, -0.3, 0), "floor_tom": (43, -4.0, -0.4, 0), "mid_tom": (45, -4.0, 0.0, 0),
    "high_tom": (48, -4.0, 0.3, 0), "tambourine": (54, -13.0, 0.25, 0), "cowbell": (56, -10.0, -0.1, 0),
    "hi_bongo": (60, -8.0, 0.3, 0), "low_bongo": (61, -8.0, 0.2, 0),
    "mute_conga": (62, -8.0, -0.2, 0), "open_conga": (63, -7.0, -0.15, 0), "low_conga": (64, -7.0, -0.3, 0),
    "high_timbale": (65, -7.0, 0.35, 0), "low_timbale": (66, -7.0, 0.25, 0),
    "high_agogo": (67, -11.0, -0.3, 0), "low_agogo": (68, -11.0, -0.25, 0),
    "cabasa": (69, -15.0, 0.3, 0), "shaker": (70, -15.0, -0.25, 0),
    "short_guiro": (73, -12.0, 0.3, 0), "guiro": (74, -12.0, 0.3, 0), "claves": (75, -10.0, 0.2, 0),
    "hi_woodblock": (76, -10.0, 0.3, 0), "low_woodblock": (77, -10.0, 0.2, 0),
    "triangle": (81, -16.0, 0.35, 0),
}

# Loudness a kick hit should have inside the kit (the kit's gain then sets the level).
REF_DB = -9.0


def k_weight(x):
    """BS.1770 K-weighting at 48 kHz (high shelf + high pass)."""
    b1, a1 = [1.53512485958697, -2.69169618940638, 1.19839281085285], [1.0, -1.69065929318241, 0.73248077421585]
    b2, a2 = [1.0, -2.0, 1.0], [1.0, -1.99004745483398, 0.99007225036621]
    return signal.lfilter(b2, a2, signal.lfilter(b1, a1, x))


def hit_loudness(x):
    """Loudest 150 ms (K-weighted mean square, dB) of a hit, padded with silence: about
    how long the ear integrates a transient, so short hats are not undercounted."""
    mono = x if x.ndim == 1 else x.mean(axis=1)
    w = int(0.15 * SR)
    y = k_weight(np.concatenate([mono, np.zeros(w)]))
    c = np.concatenate([[0.0], np.cumsum(y ** 2)])
    ms = (c[w:] - c[:-w]) / w
    return 10 * np.log10(ms.max() + 1e-12)


_LOUD = {}


def sample_loudness(name, samples):
    if name not in _LOUD:
        _LOUD[name] = hit_loudness(render(samples[name]))
    return _LOUD[name]


def resolve_gains(kits, samples):
    """Turns each pad's loudness target (+ offset) into a zone gain."""
    for k in kits:
        pads = []
        for note, sample, o in k["pads"]:
            o = dict(o)
            target = o.pop("target", None)
            if target is not None:
                names = [sample] if isinstance(sample, str) else [l if isinstance(l, str) else l[0] for l in sample]
                loud = max(sample_loudness(n, samples) for n in names)
                o["gain"] = round(float(np.clip(REF_DB + target + o.get("gain", 0.0) - loud, -30.0, 6.0)), 1)
            pads.append((note, sample, o))
        k["pads"] = pads


PADS = [36, 38, 42, 46, 39, 37, 44, 49, 41, 45, 48, 51, 35, 40, 54, 56]
TAKEOVER = [35, 40, 44, 54, 56, 51, 49, 48, 45, 41, 37, 39, 46, 42, 38, 36]


def pad_layout(notes):
    used = set(notes)
    layout = list(PADS)
    free = [PADS.index(n) for n in TAKEOVER if n not in used]
    extra = sorted(n for n in used if n not in PADS)
    for n, slot in zip(extra, free):
        layout[slot] = n
    return layout


def kit(name, category, description, params=None, **roles):
    """roles: role=sample | (sample, {opts}) | ([layers], {opts}); opts override the role's
    gain/pan/choke and add decay/tune/attack/rr/reverse. Gains are relative (samples are
    peak-normalized); `params` are device params (gain, velocity...)."""
    pads = []
    for role, spec in roles.items():
        note, gain, pan, choke = ROLES[role]
        sample, opts = (spec if isinstance(spec, tuple) else (spec, {}))
        o = {"target": gain, "pan": pan, "choke": choke}
        o.update(opts)
        pads.append((note, sample, o))
    notes = [n for n, _, _ in pads]
    assert len(notes) == len(set(notes)), f"{name}: two roles on one note"
    assert len(notes) <= 16, f"{name}: {len(notes)} pads"
    layout = pad_layout(notes)
    hidden = [n for n in notes if n not in layout]
    assert not hidden, f"{name}: notes without a pad {hidden}"
    assert 60 <= len(description) <= 400, f"{name}: description {len(description)}"
    return {"name": name, "category": category, "description": description, "params": params or {}, "pads": pads}


def rr(*names):
    """Round-robin layers: every sample in group 1."""
    return (list(names), {"rr": 1})


def vel(*layers):
    """Velocity layers, softest first, splitting 0..1 evenly."""
    n = len(layers)
    return [(s, round(i / n, 2), round((i + 1) / n, 2) if i < n - 1 else 1.0) for i, s in enumerate(layers)]
