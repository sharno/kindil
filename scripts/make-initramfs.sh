#!/usr/bin/env bash
# Stage an EXPERIMENTAL RAM-only userspace image. Never installs or flashes it.
# Usage: make-initramfs.sh RUNTIME_OVERLAY OUTPUT.cpio.gz
# Supply a matched PW1 runtime, including prebuilt static helper binaries,
# modules/firmware and SSH authorized_keys. This script does not download
# arbitrary executable binaries or guess kernel/module/partition identities.
set -euo pipefail
project=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[[ $# == 2 ]] || { echo "usage: $0 RUNTIME_OVERLAY NEW_OUTPUT.cpio.gz" >&2; exit 1; }
runtime=$(realpath "$1")
output=$(realpath -m "$2")
[[ ! -e "$output" && ! -L "$output" ]] || { echo "refusing to overwrite $output" >&2; exit 1; }
[[ -x "$project/dist/kindil" ]] || { echo 'run scripts/dev.sh device first' >&2; exit 1; }
for tool in bin/busybox sbin/dropbear sbin/wpa_supplicant sbin/udhcpc bin/sh; do
    [[ -f "$runtime/$tool" || -L "$runtime/$tool" ]] || { echo "runtime missing $tool" >&2; exit 1; }
done
[[ -s "$runtime/root/.ssh/authorized_keys" ]] || { echo 'supply root/.ssh/authorized_keys (no default passwords)' >&2; exit 1; }
[[ -s "$runtime/etc/kindil.toml" ]] || { echo 'supply etc/kindil.toml with YOUR Miqat config path/network settings' >&2; exit 1; }
[[ -s "$runtime/etc/passwd" ]] || { echo 'supply compatible account files (etc/passwd)' >&2; exit 1; }
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
archive=$(mktemp "${output}.tmp.XXXXXX")
trap 'rm -rf -- "$stage"; rm -f -- "$archive"' EXIT
cp -a "$runtime/." "$stage/"
# Never follow overlay symlinks when installing files or changing permissions.
for dir in bin sbin dev proc sys run tmp mnt mnt/us mnt/us/books var var/log var/log/kindil etc etc/dropbear root root/.ssh; do
    [[ ! -L "$stage/$dir" ]] || { echo "unsafe runtime directory symlink: $dir" >&2; exit 1; }
done
for file in init sbin/kindil-dhcp root/.ssh/authorized_keys; do
    [[ ! -L "$stage/$file" ]] || { echo "unsafe runtime file symlink: $file" >&2; exit 1; }
done
install -m755 "$project/dist/kindil" "$stage/init"
mkdir -p "$stage/sbin" "$stage/dev" "$stage/proc" "$stage/sys" "$stage/run" "$stage/tmp" "$stage/mnt/us/books" "$stage/var/log/kindil" "$stage/etc/dropbear"
ln -s ../init "$stage/sbin/kindil-dhcp"
chmod 700 "$stage/root/.ssh"
chmod 600 "$stage/root/.ssh/authorized_keys"
chmod 1777 "$stage/tmp"
# Reject dynamic/wrong-architecture helper binaries rather than silently
# producing an archive which cannot run. Follow only expected applet symlinks.
for tool in init bin/sh bin/busybox sbin/dropbear sbin/wpa_supplicant sbin/udhcpc; do
    path="$stage/$tool"
    if [[ -L "$path" ]]; then
        target=$(readlink "$path")
        if [[ "$target" == /* ]]; then path="$stage/$target"; fi
    fi
    binary=$(realpath "$path")
    [[ "$binary" == "$stage/"* ]] || { echo "unsafe runtime symlink: $tool" >&2; exit 1; }
    [[ -x "$binary" ]] || { echo "$tool is not executable" >&2; exit 1; }
    readelf -h "$binary" | grep -q 'Machine:.*ARM' || { echo "$tool is not ARM" >&2; exit 1; }
    if readelf -l "$binary" | grep -q INTERP; then echo "$tool requires a dynamic loader" >&2; exit 1; fi
done
(cd "$stage" && find . -print0 | sort -z | cpio --quiet --null -o -H newc --owner=0:0 | gzip -n) > "$archive"
# Atomic no-clobber publication; mktemp's 0600 mode protects bundled credentials.
ln "$archive" "$output"
echo "Experimental RAM image: $output"
echo 'NOT boot-validated. Kernel drivers, exact modules/firmware, storage and recovery must be verified on PW1 before booting.'
