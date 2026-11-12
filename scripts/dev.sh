#!/usr/bin/env bash
#
# The development loop for kindil.
#
# The interesting command is `device`: it cross-compiles the real ARM binary in
# a few seconds, using rustup's prebuilt `armv7-unknown-linux-musleabihf` std and
# a cached Nix cross GCC purely as the link driver. (Rust's musl targets are
# self-contained, so the driver's own libc is never used.)
#
# `nix build .#packages.armv7l-linux.kindil` is the reproducible build, but it
# compiles a dedicated static cross GCC the first time, which takes most of an
# hour. Use it for releases, not for iteration.
#
# Usage: scripts/dev.sh <command>
#
#   test        run the test suite on the host
#   test-arm    run the test suite on the ARM target under qemu-arm
#   device      build dist/kindil (static ARMv7 musl)
#   sim         render the screens to dist/sim/*.png
#   kual        package the KUAL extension into dist/
#   initramfs   package an initramfs tree into dist/
#   fmt         format the Rust and Nix sources
#   check       test + clippy + fmt --check
#   help        this text

set -euo pipefail

readonly PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"

readonly TARGET="armv7-unknown-linux-musleabihf"
readonly DIST="$PROJECT_DIR/dist"

die() {
	echo "error: $*" >&2
	exit 1
}

# The cross GCC used as a link driver for the device build.
arm_linker() {
	if [[ -n "${ARM_CC:-}" ]]; then
		printf '%s' "$ARM_CC"
		return
	fi
	local cached="$PROJECT_DIR/.cache/arm-cc"
	if [[ -s "$cached" ]]; then
		cat "$cached"
		return
	fi
	local out cc
	out=$(nix eval --raw ".#armv7-linker.outPath" 2>/dev/null || true)
	cc="$out/bin/armv7l-unknown-linux-gnueabihf-gcc"
	[[ -x "$cc" ]] || die "no ARM link driver; run 'nix build .#armv7-linker' or set ARM_CC"
	mkdir -p "$(dirname "$cached")"
	printf '%s' "$cc" >"$cached"
	printf '%s' "$cc"
}

device_build() {
	local cc
	cc=$(arm_linker)
	RUSTFLAGS="-C linker=$cc -C target-feature=+crt-static" \
		cargo build --release --target "$TARGET"
	mkdir -p "$DIST"
	install -m755 "target/$TARGET/release/kindil" "$DIST/kindil"

	local binary="$DIST/kindil"
	readelf -h "$binary" | grep -q 'Machine:.*ARM' || die "not an ARM binary"
	if readelf -l "$binary" | grep -q INTERP; then
		die "the binary is dynamically linked"
	fi
	echo "device binary: $binary ($(stat -c %s "$binary") bytes, static ARMv7)"
}

sim() {
	mkdir -p "$DIST/sim"
	cargo run --quiet -- --sim "$DIST/sim"
	if command -v ffmpeg >/dev/null 2>&1; then
		for pgm in "$DIST"/sim/*.pgm; do
			ffmpeg -loglevel error -y -i "$pgm" "${pgm%.pgm}.png"
		done
		echo "PNGs in $DIST/sim"
	else
		echo "(install ffmpeg to convert the PGMs to PNG)"
	fi
}

arm_tests() {
	local cc
	cc=$(arm_linker)
	RUSTFLAGS="-C linker=$cc -C target-feature=+crt-static" \
		CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUNNER=qemu-arm \
		cargo test --target "$TARGET"
}

case "${1:-help}" in
test) cargo test ;;
test-arm) arm_tests ;;
device) device_build ;;
sim) sim ;;
kual) device_build && ./scripts/make-kual-extension.sh "$DIST/kual" ;;
initramfs) device_build && ./scripts/make-initramfs.sh "$DIST/initramfs" ;;
fmt)
	cargo fmt
	nix fmt
	;;
check)
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test
	;;
help | *) sed -n '3,26p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
