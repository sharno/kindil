#!/usr/bin/env bash
#
# Build a minimal initramfs that runs kindile as PID 1, replacing the Kindle
# OS entirely.
#
# The kernel's command line then needs:
#
#   root=/dev/ram0 rootfstype=ramfs rdinit=/sbin/init
#
# which is what Quill OS does for the same hardware family. Boot it with
# `kexec` from the stock firmware, or from the U-Boot prompt.
#
# Usage: scripts/make-initramfs.sh [OUTPUT_DIR]

set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
readonly OUT_DIR="${1:-$PROJECT_DIR/result/initramfs}"
readonly BINARY="$PROJECT_DIR/result/bin/kindile"

if [[ ! -x "$BINARY" ]]; then
	echo "error: $BINARY not found." >&2
	echo "Build it first: nix build .#packages.armv7l-linux.kindile" >&2
	exit 1
fi

# The Kindle's own init is not what we want; kindile *is* the system.
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/sbin" "$OUT_DIR/dev" "$OUT_DIR/proc" "$OUT_DIR/sys" "$OUT_DIR/tmp"

install -m755 "$BINARY" "$OUT_DIR/sbin/init"

# A minimal /etc for the timezone and motd.
mkdir -p "$OUT_DIR/etc"
printf 'UTC\n' > "$OUT_DIR/etc/timezone"

# A minimal /inittab is not needed: kindile is PID 1 and does not fork.
# But the kernel wants the directory entries to exist for devtmpfs.

echo "initramfs written to $OUT_DIR"
echo
echo "To boot it:"
echo "  1. Copy the tree onto the device, e.g. as /mnt/us/initramfs"
echo "  2. From a root shell on the Kindle:"
echo "       kexec -l <kernel> --initrd=<initramfs.cpio.gz> --append='root=/dev/ram0 rootfstype=ramfs rdinit=/sbin/init'"
echo
echo "Note: the initramfs must be gzipped cpio. 'find . | cpio -o -H newc | gzip' produces one."
