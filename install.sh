#!/bin/sh
# Cadenza installer for Linux (x86_64) and macOS (Apple Silicon and Intel).
#
#   curl -fsSL https://github.com/Pep3M/cadenza/releases/latest/download/install.sh | sh
#
# Options (or the environment variables next to them):
#   --version vX.Y.Z   CADENZA_VERSION      release to install (default: latest)
#   --bin-dir DIR      CADENZA_BIN_DIR      where the `cadenza` / `cadenza-app` commands go
#                                           (default: ~/.local/bin)
#   --app-dir DIR      CADENZA_APP_DIR      Linux: install folder (default: ~/.local/share/cadenza)
#                                           macOS: folder for Cadenza.app (default: /Applications,
#                                           ~/Applications when not writable)
#   --uninstall                             remove what this script installed
#
# Everything is installed for the current user; no root needed (except /Applications on
# some Macs). Downloads are checked against the release's SHA256SUMS.
set -eu

REPO="Pep3M/cadenza"
VERSION="${CADENZA_VERSION:-latest}"
BIN_DIR="${CADENZA_BIN_DIR:-$HOME/.local/bin}"
APP_DIR="${CADENZA_APP_DIR:-}"
UNINSTALL=0

say() { printf '%s\n' "cadenza: $*"; }
die() { printf '%s\n' "cadenza: error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
		--bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; BIN_DIR="$2"; shift 2 ;;
		--app-dir) [ $# -ge 2 ] || die "--app-dir needs a value"; APP_DIR="$2"; shift 2 ;;
		--uninstall) UNINSTALL=1; shift ;;
		-h|--help) sed -n '2,17p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
		*) die "unknown option '$1' (try --help)" ;;
	esac
done

case "$VERSION" in latest|v*) ;; *) VERSION="v$VERSION" ;; esac

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but was not found"; }

fetch() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --retry 3 -o "$2" "$1"
	elif command -v wget >/dev/null 2>&1; then
		wget -q -O "$2" "$1"
	else
		die "curl or wget is required"
	fi
}

fetch_stdout() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --retry 3 "$1"
	else
		wget -q -O - "$1"
	fi
}

sha256() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
	Linux)
		case "$ARCH" in
			x86_64|amd64) ;;
			*) die "no prebuilt Linux binaries for $ARCH yet; build from source (see README)" ;;
		esac
		PLATFORM="linux-x86_64"; EXT="tar.gz"
		[ -n "$APP_DIR" ] || APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/cadenza"
		;;
	Darwin)
		PLATFORM="macos-universal"; EXT="dmg"
		if [ -z "$APP_DIR" ]; then
			if [ -w /Applications ]; then APP_DIR="/Applications"; else APP_DIR="$HOME/Applications"; fi
		fi
		;;
	*) die "unsupported system '$OS'; on Windows use install.ps1 (see README)" ;;
esac

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_FILE="$DATA_HOME/applications/cadenza.desktop"
ICON_FILE="$DATA_HOME/icons/hicolor/scalable/apps/cadenza.svg"

# Linux installs own APP_DIR entirely: refuse to wipe a folder that holds anything else.
check_app_dir() {
	if [ -d "$APP_DIR" ] && [ -n "$(ls -A "$APP_DIR" 2>/dev/null)" ] && [ ! -f "$APP_DIR/cadenza-app" ]; then
		die "$APP_DIR is not empty and holds no Cadenza install; choose another --app-dir"
	fi
}

uninstall() {
	for b in cadenza cadenza-app; do
		if [ -f "$BIN_DIR/$b" ] && grep -q "installed by the Cadenza installer" "$BIN_DIR/$b" 2>/dev/null; then
			rm -f "$BIN_DIR/$b"
		fi
	done
	if [ "$OS" = Darwin ]; then
		rm -rf "$APP_DIR/Cadenza.app"
	else
		check_app_dir
		rm -rf "$APP_DIR"
		rm -f "$DESKTOP_FILE" "$ICON_FILE"
		command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA_HOME/applications" 2>/dev/null || true
	fi
	say "uninstalled (projects, settings and the style library in your user folders were kept)"
}

if [ "$UNINSTALL" = 1 ]; then
	uninstall
	exit 0
fi

# Resolve the tag: asset names carry the version without the leading "v".
if [ "$VERSION" = latest ]; then
	TAG="$(fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" |
		sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
	[ -n "$TAG" ] || die "could not find the latest release of $REPO (GitHub API rate limit? try --version)"
else
	TAG="$VERSION"
fi
ASSET="cadenza-${TAG#v}-$PLATFORM.$EXT"
BASE="https://github.com/$REPO/releases/download/$TAG"

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t cadenza)"
MOUNT=""
cleanup() {
	if [ -n "$MOUNT" ]; then hdiutil detach -quiet "$MOUNT" 2>/dev/null || true; fi
	rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

say "downloading $ASSET ($TAG)"
fetch "$BASE/$ASSET" "$TMP/$ASSET" || die "download failed: $BASE/$ASSET"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" || die "download failed: $BASE/SHA256SUMS"
EXPECTED="$(grep " \*\{0,1\}$ASSET\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$EXPECTED" ] || die "$ASSET is not listed in SHA256SUMS"
[ "$(sha256 "$TMP/$ASSET")" = "$EXPECTED" ] || die "checksum mismatch for $ASSET; aborting"

# A wrapper instead of a symlink: the binaries find their helpers (plugin scanner and
# sandbox, cadenza-app) next to their own path.
write_wrapper() {
	cat > "$BIN_DIR/$1" <<EOF
#!/bin/sh
# installed by the Cadenza installer ($TAG)
exec "$2" "\$@"
EOF
	chmod 755 "$BIN_DIR/$1"
}

mkdir -p "$BIN_DIR"

if [ "$OS" = Linux ]; then
	need tar
	tar -xzf "$TMP/$ASSET" -C "$TMP"
	SRC="$TMP/cadenza-${TAG#v}-$PLATFORM"
	[ -x "$SRC/cadenza-app" ] || die "unexpected archive layout"
	check_app_dir
	rm -rf "$APP_DIR"
	mkdir -p "$APP_DIR"
	cp -R "$SRC/." "$APP_DIR/"
	write_wrapper cadenza "$APP_DIR/cadenza"
	write_wrapper cadenza-app "$APP_DIR/cadenza-app"
	if [ -f "$APP_DIR/cadenza.desktop" ]; then
		mkdir -p "$(dirname "$DESKTOP_FILE")" "$(dirname "$ICON_FILE")"
		sed "s|^Exec=cadenza-app|Exec=\"$APP_DIR/cadenza-app\"|" "$APP_DIR/cadenza.desktop" > "$DESKTOP_FILE"
		cp "$APP_DIR/cadenza.svg" "$ICON_FILE"
		command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA_HOME/applications" 2>/dev/null || true
	fi
	if command -v ldconfig >/dev/null 2>&1 && ! ldconfig -p 2>/dev/null | grep -q libasound.so.2; then
		say "warning: ALSA (libasound2 / alsa-lib) was not found; install it for audio"
	fi
	say "installed $TAG in $APP_DIR"
else
	need hdiutil
	MOUNT="$TMP/mnt"
	mkdir -p "$MOUNT"
	hdiutil attach -quiet -nobrowse -readonly -mountpoint "$MOUNT" "$TMP/$ASSET" || die "could not open $ASSET"
	[ -d "$MOUNT/Cadenza.app" ] || die "unexpected disk image layout"
	mkdir -p "$APP_DIR"
	rm -rf "$APP_DIR/Cadenza.app"
	cp -R "$MOUNT/Cadenza.app" "$APP_DIR/" || die "cannot write to $APP_DIR (try --app-dir ~/Applications)"
	# The app is ad-hoc signed, not notarized: drop the quarantine flag so Gatekeeper
	# does not block the first launch.
	xattr -dr com.apple.quarantine "$APP_DIR/Cadenza.app" 2>/dev/null || true
	write_wrapper cadenza "$APP_DIR/Cadenza.app/Contents/MacOS/cadenza"
	write_wrapper cadenza-app "$APP_DIR/Cadenza.app/Contents/MacOS/cadenza-app"
	say "installed $TAG: $APP_DIR/Cadenza.app"
fi

say "commands: $BIN_DIR/cadenza, $BIN_DIR/cadenza-app"
case ":$PATH:" in
	*":$BIN_DIR:"*) ;;
	*) say "add $BIN_DIR to your PATH, e.g.: echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.profile" ;;
esac
