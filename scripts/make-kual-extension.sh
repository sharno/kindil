#!/usr/bin/env bash
#
# Package kindil as a KUAL extension, ready to drop onto a jailbroken
# Kindle's user partition.
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

# The binary built by the flake.
readonly BINARY="$PROJECT_DIR/result/bin/kindil"

if [[ ! -x "$BINARY" ]]; then
	echo "error: $BINARY not found." >&2
	echo "Build it first: nix build .#packages.armv7l-linux.kindil" >&2
	exit 1
fi

# The Kindle's debug serial is 1.8 V; a wrong adapter can corrupt the
# bootloader, so the extension must not touch it.
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/$EXT_NAME"

install -m755 "$BINARY" "$OUT_DIR/$EXT_NAME/$EXT_NAME"

cat > "$OUT_DIR/$EXT_NAME/config.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<extension>
	<information>
		<name>kindil</name>
		<version>0.1.0</version>
		<author>kindil</author>
		<id>kindil</id>
	</information>
	<menus>
		<menu type="json" name="kindil">
			<item name="kindil" priority="0" action="kindil" />
		</menu>
	</menus>
	<files>
		<file name="kindil" />
	</files>
</extension>
XML

echo "KUAL extension written to $OUT_DIR"
echo
echo "Install it on the Kindle with the device mounted over USB:"
echo "  cp -r $OUT_DIR/$EXT_NAME /mnt/us/extensions/"
echo "  chmod 755 /mnt/us/extensions/$EXT_NAME/kindil"
