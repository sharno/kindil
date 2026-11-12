#!/usr/bin/env bash
#
# Build a minimal initramfs that runs kindil as PID 1, replacing the Kindle OS.
#
# The kernel's command line then needs:
#
#   root=/dev/ram0 rootfstype=ramfs rdinit=/sbin/init
#
# which is what Quill OS does for the same hardware family. Boot it with
# `kexec` from the stock firmware, or from the U-Boot prompt.
#
# kindil mounts devtmpfs on /dev and creates the device nodes itself when it is
# started with `--init`, so the initramfs does not need udev.
#
# Usage: scripts/make-initramfs.sh [OUTPUT_DIR]

set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
readonly OUT_DIR="${1:-$PROJECT_DIR/result/initramfs}"

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
mkdir -p "$OUT_DIR/sbin" "$OUT_DIR/dev" "$OUT_DIR/proc" "$OUT_DIR/sys" "$OUT_DIR/tmp" "$OUT_DIR/etc"

install -m755 "$BINARY" "$OUT_DIR/sbin/init"

# kindil reads the timezone from $TZ, /etc/TZ or /etc/localtime. There is no tz
# database here, so the POSIX string is the only option; adjust it as needed.
printf 'UTC0\n' >"$OUT_DIR/etc/TZ"

# The kernel needs the mount points to exist; kindil mounts devtmpfs itself.
chmod 1777 "$OUT_DIR/tmp"

readonly ARCHIVE="$OUT_DIR.cpio.gz"
if command -v cpio >/dev/null 2>&1 && command -v gzip >/dev/null 2>&1; then
	(cd "$OUT_DIR" && find . | cpio -o -H newc 2>/dev/null | gzip -9) >"$ARCHIVE"
	echo "initramfs written to $OUT_DIR"
	echo "gzipped cpio:      $ARCHIVE"
else
	echo "initramfs tree written to $OUT_DIR"
	echo "(install cpio and gzip to produce the .cpio.gz automatically)"
fi

echo
echo "To boot it:"
echo "  1. Copy the tree onto the device, e.g. as /mnt/us/initramfs"
echo "  2. From a root shell on the Kindle:"
echo "       kexec -l <kernel> --initrd=<initramfs.cpio.gz> --append='root=/dev/ram0 rootfstype=ramfs rdinit=/sbin/init'"
echo
echo "kindil notices it is PID 1 and mounts devtmpfs / creates the device nodes"
echo "itself, so no udev or init script is needed."
