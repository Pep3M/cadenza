"""Runs the kit audition and moves each kit's gain so its true peak lands on TARGET.
calibrate.py <repo> [rounds]"""
import json, os, subprocess, sys
REPO = sys.argv[1]; ROUNDS = int(sys.argv[2]) if len(sys.argv) > 2 else 2
HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = -1.6  # true-peak ceiling
LOUD = -14.0  # momentary loudness aimed at when the ceiling allows
GF = os.path.join(HERE, "kit_gains.json")
for r in range(ROUNDS):
    out = subprocess.run(
        ["cargo", "test", "-q", "-p", "cadenza-engine", "--release", "--test", "drum_kits", "audition_kits", "--", "--ignored", "--nocapture"],
        cwd=REPO, capture_output=True, text=True, env={**os.environ, "CADENZA_KIT_AUDITION_DIR": os.path.join(HERE, "audition")},
    ).stdout
    gains = json.load(open(GF)) if os.path.exists(GF) else {}
    rows = {}
    for line in out.splitlines():
        if len(line) > 40 and "|" in line and not line.startswith("kit "):
            name = line[:24].strip(); cols = line[25:].split()
            m, lufs, tp = float(cols[1]), float(cols[2]), float(cols[3])
            rows[name] = (m, tp)
    for name, (m, tp) in rows.items():
        old = gains.get(name, 0.0)
        step = min(LOUD - m, TARGET - tp)
        gains[name] = round(max(-24.0, min(12.0, old + step)), 1)
    json.dump(gains, open(GF, "w"), indent=1, sort_keys=True)
    worst = max(min(abs(tp - TARGET), abs(m - LOUD)) for m, tp in rows.values())
    print(f"round {r}: {len(rows)} kits, worst TP error {worst:.1f}")
    for n, (m, tp) in sorted(rows.items(), key=lambda kv: kv[1][0]):
        print(f"  {n:24s} M {m:6.1f}  TP {tp:5.1f}  gain {gains[n]}")
    subprocess.run([sys.executable, os.path.join(HERE, "build.py"), REPO, "--code-only"], check=True, capture_output=True)
    subprocess.run(["cargo", "fmt", "-p", "cadenza-dsp"], cwd=REPO, check=True)
