#!/usr/bin/env bash
#
# The development loop for kindil.
#
# Both build paths use prebuilt Rust/ARM std and cached Zig, not a source-built
# cross toolchain. First Slint builds take longer; incremental work reuses Cargo
# artifacts. `nix build .#kindil-device` is the pinned release build.
#
# Usage: scripts/dev.sh <command>
#
#   test        run the test suite on the host
#   test-arm    run the test suite on the ARM target under qemu-arm
#   device      build dist/kindil (static ARMv7 musl)
#   sim         render every screen to dist/sim/*.png (simulated device)
#   desktop     interactive window (simulated device)
#   deploy H    build, then copy the binary and KUAL launcher to ssh host H
#   fmt         format the Rust and Nix sources
#   check       test + clippy + fmt --check
#   help        this text

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly PROJECT_DIR
cd "$PROJECT_DIR"

readonly TARGET="armv7-unknown-linux-musleabihf"
readonly DIST="$PROJECT_DIR/dist"

die() {
	echo "error: $*" >&2
	exit 1
}

# Resolve the current flake's musl-aware link driver; do not reuse a stale cache
# path after changing toolchains.
arm_linker() {
	if [[ -n "${ARM_CC:-}" ]]; then
		printf '%s' "$ARM_CC"
		return
	fi
	local out cc
	out=$(nix eval --raw ".#armv7-linker.outPath")
	cc="$out/bin/armv7l-unknown-linux-gnueabihf-gcc"
	if [[ ! -x "$cc" ]]; then nix build --no-link .#armv7-linker >&2; fi
	[[ -x "$cc" ]] || die "no ARM link driver; run 'nix build .#armv7-linker' or set ARM_CC"
	printf '%s' "$cc"
}

device_build() {
	local cc
	cc=$(arm_linker)
	export CC_armv7_unknown_linux_musleabihf="$cc"
	RUSTFLAGS="-C linker=$cc -C target-feature=+crt-static -C link-self-contained=no" \
		cargo build --locked --release --features kindle --target "$TARGET"
	mkdir -p "$DIST"
	install -m755 "target/$TARGET/release/kindil" "$DIST/kindil"

	local binary="$DIST/kindil"
	readelf -h "$binary" | grep -q 'Machine:.*ARM' || die "not an ARM binary"
	if readelf -l "$binary" | grep -q INTERP; then
		die "the binary is dynamically linked"
	fi
	echo "device binary: $binary ($(wc -c <"$binary" | tr -d " ") bytes, static ARMv7)"
}

sim() {
	mkdir -p "$DIST/sim"
	cargo run --quiet -- preview "$DIST/sim" --config examples/kindil.toml --books examples/books
	if command -v ffmpeg >/dev/null 2>&1; then
		for pgm in "$DIST"/sim/*.ppm; do
			ffmpeg -loglevel error -y -i "$pgm" "${pgm%.ppm}.png"
		done
		echo "PNGs in $DIST/sim"
	else
		echo "(install ffmpeg to convert the PGMs to PNG)"
	fi
}

# The Kindle has no rsync; stream a tar over SSH. The launcher's own files go
# first, then the binary, so a half-finished copy never runs a stale binary.
deploy() {
	local host="$1"
	[[ -n "$host" ]] || die "usage: scripts/dev.sh deploy root@KINDLE_IP"
	device_build
	tar -C kindle/extensions -cf - kindil |
		ssh "$host" 'mkdir -p /mnt/us/extensions /mnt/us/kindil && tar -C /mnt/us/extensions -xf -'
	ssh "$host" 'cat > /mnt/us/extensions/kindil/bin/kindil.new && chmod 755 /mnt/us/extensions/kindil/bin/kindil.new && mv /mnt/us/extensions/kindil/bin/kindil.new /mnt/us/extensions/kindil/bin/kindil' <"$DIST/kindil"
	ssh "$host" /mnt/us/extensions/kindil/bin/kindil --version
}

arm_tests() {
	local cc
	cc=$(arm_linker)
	export CC_armv7_unknown_linux_musleabihf="$cc"
	RUSTFLAGS="-C linker=$cc -C target-feature=+crt-static -C link-self-contained=no" \
		CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUNNER=qemu-arm \
		cargo test --locked --target "$TARGET"
}

case "${1:-help}" in
test) cargo test ;;
test-arm) arm_tests ;;
device) device_build ;;
sim) sim ;;
desktop) cargo run --features desktop -- desktop --config examples/kindil.toml --books examples/books ;;
deploy) deploy "${2:-}" ;;
fmt)
	cargo fmt
	nix fmt
	;;
check)
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test
	tests/launcher.sh
	tests/boot.sh
	;;
help) sed -n '3,24p' "$0" | sed 's/^# \{0,1\}//' ;;
*) die "unknown command: $1" ;;
esac
