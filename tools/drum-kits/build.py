"""Builds the factory kits into the repo: build.py <repo> [--code-only]."""
import os, sys, json
import kitgen
import samples_synth
import kits_electronic

SAMPLE_SETS = [samples_synth.SAMPLES]
KIT_SETS = [kits_electronic.KITS]
for mod in ("samples_real", "kits_real"):
    try:
        m = __import__(mod)
        (SAMPLE_SETS if mod.startswith("samples") else KIT_SETS).append(m.SAMPLES if mod.startswith("samples") else m.KITS)
    except ModuleNotFoundError:
        pass

samples = {}
for s in SAMPLE_SETS:
    dup = set(samples) & set(s)
    assert not dup, dup
    samples.update(s)
kits = [k for ks in KIT_SETS for k in ks]
names = [k["name"] for k in kits]
assert len(names) == len(set(names)), "duplicate kit names"
# Only ship samples some kit uses.
used = {s for k in kits for _, s, _ in kitgen.expand(k["pads"])}
unused = sorted(set(samples) - used)
if unused:
    print("unused samples (not shipped):", unused)
samples = {n: s for n, s in samples.items() if n in used}

kitgen.resolve_gains(kits, samples)
repo = sys.argv[1]
if "--code-only" not in sys.argv:
    stats = kitgen.write_samples(repo, samples)
open(os.path.join(repo, "assets", "samples", "CREDITS.md"), "w").write(kitgen.credits_md(samples))
print(kitgen.write_rust(repo, kits, samples))
total = sum(os.path.getsize(os.path.join(repo, "assets", "samples", f)) for f in os.listdir(os.path.join(repo, "assets", "samples")))
print(f"{len(kits)} kits, {len(samples)} samples, {total/1e6:.1f} MB")
