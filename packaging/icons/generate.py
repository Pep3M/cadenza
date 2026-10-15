#!/usr/bin/env python3
"""Regenerates every Cadenza icon from one design.

Writes, next to this script:
  cadenza.svg           full-bleed icon (Linux desktop entry, AppImage, sources of the rest)
  cadenza-macos.svg     macOS layout: 824/1024 body with margin and shadow (Apple's grid)
  cadenza.icns          macOS bundle icon            (needs `iconutil`, macOS only)
  cadenza.ico           Windows executable icon      (16..256 px PNG entries)
  cadenza-256.png       window / taskbar icon embedded by cadenza-app

Rendering needs `rsvg-convert` (librsvg). The outputs are committed, so builds and CI
never run this script.
"""

import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent


def mark(scale: float, offset: float) -> str:
	"""The C ring and the pulse, in a 1024 grid, placed into the body."""
	t = f'translate({offset} {offset}) scale({scale})'
	return f'''
  <g transform="{t}">
    <circle cx="512" cy="512" r="300" fill="url(#glow)"/>
    <path d="M688 337 A248 248 0 1 0 688 687" fill="none" stroke="url(#ring)"
          stroke-width="92" stroke-linecap="round"/>
    <path d="M436 512 H486 L518 424 L566 600 L606 480 L630 512 H688" fill="none"
          stroke="#f4f1ec" stroke-width="38" stroke-linecap="round" stroke-linejoin="round"/>
  </g>'''


DEFS = '''
  <defs>
    <linearGradient id="body" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#2b3039"/>
      <stop offset="1" stop-color="#121418"/>
    </linearGradient>
    <linearGradient id="edge" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.16"/>
      <stop offset="0.5" stop-color="#ffffff" stop-opacity="0.03"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0.06"/>
    </linearGradient>
    <linearGradient id="ring" x1="0.2" y1="0" x2="0.6" y2="1">
      <stop offset="0" stop-color="#ffc07e"/>
      <stop offset="1" stop-color="#ea7f35"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.5" r="0.5">
      <stop offset="0" stop-color="#f0a35e" stop-opacity="0.22"/>
      <stop offset="1" stop-color="#f0a35e" stop-opacity="0"/>
    </radialGradient>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feGaussianBlur in="SourceAlpha" stdDeviation="14"/>
      <feOffset dy="12" result="b"/>
      <feComponentTransfer><feFuncA type="linear" slope="0.45"/></feComponentTransfer>
      <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
  </defs>'''


def icon(x: float, size: float, radius: float, shadow: bool) -> str:
	f = ' filter="url(#shadow)"' if shadow else ''
	return f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{DEFS}
  <rect x="{x}" y="{x}" width="{size}" height="{size}" rx="{radius}" fill="url(#body)"{f}/>
  <rect x="{x + 3}" y="{x + 3}" width="{size - 6}" height="{size - 6}" rx="{radius - 3}" fill="none"
        stroke="url(#edge)" stroke-width="6"/>{mark(size / 1024, x)}
</svg>
'''


def render(svg: Path, png: Path, px: int) -> None:
	subprocess.run(["rsvg-convert", "-w", str(px), "-h", str(px), "-o", str(png), str(svg)], check=True)


def write_ico(pngs: list[tuple[int, bytes]], out: Path) -> None:
	header = struct.pack("<HHH", 0, 1, len(pngs))
	offset = 6 + 16 * len(pngs)
	entries, data = b"", b""
	for px, blob in pngs:
		dim = 0 if px >= 256 else px
		entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset + len(data))
		data += blob
	out.write_bytes(header + entries + data)


def main() -> None:
	if not shutil.which("rsvg-convert"):
		sys.exit("rsvg-convert (librsvg) is required")
	flat = HERE / "cadenza.svg"
	macos = HERE / "cadenza-macos.svg"
	flat.write_text(icon(32, 960, 216, shadow=False))
	macos.write_text(icon(100, 824, 185, shadow=True))

	with tempfile.TemporaryDirectory() as tmp:
		tmp = Path(tmp)
		render(flat, HERE / "cadenza-256.png", 256)

		ico = []
		for px in (16, 24, 32, 48, 64, 128, 256):
			p = tmp / f"ico-{px}.png"
			render(flat, p, px)
			ico.append((px, p.read_bytes()))
		write_ico(ico, HERE / "cadenza.ico")

		if shutil.which("iconutil"):
			iconset = tmp / "cadenza.iconset"
			iconset.mkdir()
			for px in (16, 32, 128, 256, 512):
				render(macos, iconset / f"icon_{px}x{px}.png", px)
				render(macos, iconset / f"icon_{px}x{px}@2x.png", px * 2)
			subprocess.run(["iconutil", "-c", "icns", "-o", str(HERE / "cadenza.icns"), str(iconset)], check=True)
		else:
			print("iconutil not found (macOS only): cadenza.icns left unchanged")


if __name__ == "__main__":
	main()
