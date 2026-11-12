#!/usr/bin/env bash
# Archive-contract tests only. Fixtures are NOT a usable runtime overlay.
set -euo pipefail
project=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
testdir=$(mktemp -d)
trap 'rm -rf -- "$testdir"' EXIT
runtime="$testdir/runtime"
mkdir -p "$runtime" "$testdir/outside"
reject() {
    if "$project/scripts/make-initramfs.sh" "$runtime" "$testdir/image.gz" > "$testdir/log" 2>&1; then
        echo 'expected staging refusal' >&2; exit 1
    fi
    grep -q "$1" "$testdir/log"
    [[ ! -e "$testdir/image.gz" ]]
}
reject 'runtime missing'
mkdir -p "$runtime/bin" "$runtime/sbin" "$runtime/root/.ssh" "$runtime/etc"
# A known static ARM ELF suffices to test architecture checks, not helper behavior.
install -m555 "$project/dist/kindil" "$testdir/readonly-elf"
for tool in bin/busybox sbin/dropbear sbin/wpa_supplicant sbin/udhcpc; do
    # Nix-store executables are read-only. Fixtures must remain replaceable for
    # the deliberate wrong-architecture test below.
    install -m755 "$testdir/readonly-elf" "$runtime/$tool"
done
ln -s /bin/busybox "$runtime/bin/sh"
echo 'fixture-only-not-a-key' > "$runtime/root/.ssh/authorized_keys"
echo 'ssh = false' > "$runtime/etc/kindil.toml"
echo 'root:x:0:0:root:/root:/bin/sh' > "$runtime/etc/passwd"
ln -s "$testdir/outside" "$runtime/tmp"
reject 'unsafe runtime directory symlink'
unlink "$runtime/tmp"
install -m755 /bin/sh "$runtime/sbin/dropbear"
reject 'not ARM'
install -m755 "$testdir/readonly-elf" "$runtime/sbin/dropbear"
"$project/scripts/make-initramfs.sh" "$runtime" "$testdir/image.gz"
[[ $(stat -c %a "$testdir/image.gz") == 600 ]]
gzip -t "$testdir/image.gz"
gzip -dc "$testdir/image.gz" | cpio -it --quiet > "$testdir/entries"
grep -Eq '^(\./)?init$' "$testdir/entries"
before=$(sha256sum "$testdir/image.gz")
if "$project/scripts/make-initramfs.sh" "$runtime" "$testdir/image.gz" > "$testdir/log" 2>&1; then exit 1; fi
grep -q 'refusing to overwrite' "$testdir/log"
[[ "$before" == "$(sha256sum "$testdir/image.gz")" ]]
[[ -z $(find "$testdir" -name 'image.gz.tmp.*' -print) ]]
echo 'initramfs refusal, containment, atomic archive and permissions checks passed'
