#!/usr/bin/env bash
#
# Package kindil as a KUAL extension, ready to drop onto a jailbroken Kindle's
# user partition.
#
# KUAL extensions live under /mnt/us/extensions/<name>/ and are described by a
# config.xml that KUAL parses to build its menu. See:
#   https://www.mobileread.com/forums/showthread.php?t=203326
#
# Usage: scripts/make-kual-extension.sh [OUTPUT_DIR]

set -euo pipefail

readonly EXT_NAME="kindil"
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
readonly OUT_DIR="${1:-$PROJECT_DIR/result/kual-extension}"

# The binary built by `scripts/dev.sh device` (or by the flake, if you copy it
# to result/bin/kindil).
for candidate in "$PROJECT_DIR/dist/kindil" "$PROJECT_DIR/result/bin/kindil"; do
	if [[ -x "$candidate" ]]; then
		readonly BINARY="$candidate"
		break
	fi
done
if [[ -z "${BINARY:-}" ]]; then
	echo "error: no device binary found." >&2
	echo "Build it first: scripts/dev.sh device (or nix build .#packages.armv7l-linux.kindil)" >&2
	exit 1
fi

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/$EXT_NAME"

install -m755 "$BINARY" "$OUT_DIR/$EXT_NAME/$EXT_NAME"

# Launcher. Amazon's UI still owns the panel and the touchscreen when kindil
# runs as a normal KUAL extension, so the "full screen" entry stops the
# framework and restarts it when kindil exits.
cat >"$OUT_DIR/$EXT_NAME/run.sh" <<'SH'
#!/bin/sh
# Run kindil alongside Amazon's UI.
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
"$DIR/kindil"
SH

cat >"$OUT_DIR/$EXT_NAME/run-fullscreen.sh" <<'SH'
#!/bin/sh
# Run kindil alone: stop Amazon's UI first, and bring it back on exit.
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

framework() {
	if command -v initctl >/dev/null 2>&1; then
		initctl "$1" framework 2>/dev/null || true
	else
		/etc/init.d/framework "$1" 2>/dev/null || true
	fi
}

framework stop
trap 'framework start' EXIT INT TERM
"$DIR/kindil"
SH

chmod 755 "$OUT_DIR/$EXT_NAME/run.sh" "$OUT_DIR/$EXT_NAME/run-fullscreen.sh"

cat >"$OUT_DIR/$EXT_NAME/config.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<extension>
	<information>
		<name>kindil</name>
		<version>0.2.0</version>
		<author>sharno</author>
		<id>kindil</id>
	</information>
	<menus>
		<menu type="json" name="kindil">
			<item name="kindil" priority="0" action="run.sh" />
			<item name="kindil (full screen)" priority="1" action="run-fullscreen.sh" />
		</menu>
	</menus>
	<files>
		<file name="run.sh" />
		<file name="run-fullscreen.sh" />
	</files>
</extension>
XML

echo "KUAL extension written to $OUT_DIR/$EXT_NAME"
echo
echo "Install it on the Kindle with the device mounted over USB:"
echo "  cp -r $OUT_DIR/$EXT_NAME /mnt/us/extensions/"
echo "  chmod 755 /mnt/us/extensions/$EXT_NAME/$EXT_NAME"
