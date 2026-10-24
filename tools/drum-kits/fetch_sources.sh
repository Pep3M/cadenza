#!/usr/bin/env bash
# Downloads the redistributable recordings the genre kits are cut from into ./sources
# (or $CADENZA_KIT_SOURCES). Needs curl, tar and git. About 400 MB.
set -euo pipefail
cd "$(dirname "$0")"
DEST="${CADENZA_KIT_SOURCES:-sources}"
mkdir -p "$DEST"
SF="https://sourceforge.net/projects/hydrogen/files/Sound%20Libraries/Main%20sound%20libraries"

h2() { # folder, url: a Hydrogen .h2drumkit (gzip tar)
	local dir="$DEST/hydrogen-$1"
	[ -d "$dir" ] && return
	mkdir -p "$dir"
	curl -fL --retry 3 -o "$dir.h2drumkit" "$2"
	tar -xzf "$dir.h2drumkit" -C "$dir"
	rm "$dir.h2drumkit"
}

sparse() { # folder, repo, paths...: a sparse shallow clone
	local dir="$DEST/$1" repo="$2"
	shift 2
	[ -d "$dir" ] && return
	git clone --depth 1 --filter=blob:none --sparse "$repo" "$dir"
	git -C "$dir" sparse-checkout set "$@"
}

h2 AVLDrumkits-BlackPearl-2023 "$SF/AVLDrumkits-BlackPearl-2023.h2drumkit"
h2 AVLDrumkits-BlondeBop "$SF/AVLDrumkits-BlondeBop.h2drumkit"
h2 AVLDrumkits-BlondeBop-HotRod "$SF/AVLDrumkits-BlondeBop-HotRod.h2drumkit"
h2 SF3007-2011-Set-03 "$SF/SF3007-2011-Set-03.h2drumkit"
h2 Gimme_A_Hand_1.0 "$SF/Gimme%20A%20Hand%201.0.h2drumkit"
h2 circAfrique_v4 "$SF/circAfrique%20v4.h2drumkit"
h2 Millo_MultiLayered2 "http://prdownloads.sf.net/hydrogen/Millo_MultiLayered2.h2drumkit"
sparse hydrogen-GMRockKit https://github.com/hydrogen-music/hydrogen data/drumkits/GMRockKit
sparse vcsl https://github.com/sgossner/VCSL Idiophones Membranophones
echo "sources in $DEST"
