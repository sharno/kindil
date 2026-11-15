#!/usr/bin/env bash
#
# Installs a signed GitHub release of kindil on the Kindle over SSH.
#
# Usage: scripts/device/release.sh VERSION [--rollback VERSION] [--wait HOURS]
#
# Downloads the release's binary and signature, checks the signature against
# the key kindil's own updater trusts (src/update.rs), bundles it with the
# launcher files from the release's tag, and waits (up to --wait hours,
# default 12) for the Kindle to answer. There scripts/device/install.sh
# installs it and rolls back unless kindil comes back up with its Wi-Fi: to
# --rollback's release if given, else to the version being replaced.
# See scripts/device/lib.sh for KINDIL_DEVICE and KINDIL_SSH_KEY.

set -euo pipefail
# shellcheck source=scripts/device/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
ROOT="$(cd "$DEVICE_DIR/../.." && pwd)"
REPO="sharno/kindil"

version="" rollback="" hours=12
while [[ $# -gt 0 ]]; do
	case "$1" in
	--rollback) rollback="${2:?--rollback needs a version}" && shift 2 ;;
	--wait) hours="${2:?--wait needs hours}" && shift 2 ;;
	-*) die "unknown option $1" ;;
	*) version="${1#v}" && shift ;;
	esac
done
[[ -n "$version" ]] || die "usage: scripts/device/release.sh VERSION [--rollback VERSION] [--wait HOURS]"

KEY="$(sed -n 's/^const PUBLIC_KEY: &str = "\(.*\)";$/\1/p' "$ROOT/src/update.rs")"
[[ -n "$KEY" ]] || die "no PUBLIC_KEY in src/update.rs"
# minisign as installed, or from nixpkgs.
run_minisign() {
	if type -P minisign >/dev/null; then
		minisign "$@"
	else
		nix shell nixpkgs#minisign -c minisign "$@"
	fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# fetch VERSION DIR: the release's binary, its signature checked.
fetch() {
	mkdir -p "$2"
	gh release download "v$1" --repo "$REPO" -D "$2" -p kindil -p kindil.minisig
	run_minisign -Vm "$2/kindil" -x "$2/kindil.minisig" -P "$KEY" >/dev/null ||
		die "v$1: bad signature"
	grep -q "trusted comment: kindil v$1$" "$2/kindil.minisig" ||
		die "v$1: the signature is for another version"
}

fetch "$version" "$WORK/new"
mkdir -p "$WORK/stage"
# Releases are tagged on GitHub: fetch the tag if this clone lacks it.
git -C "$ROOT" rev-parse -q --verify "refs/tags/v$version" >/dev/null ||
	git -C "$ROOT" fetch -q origin "refs/tags/v$version:refs/tags/v$version"
git -C "$ROOT" archive "v$version" kindle/extensions | tar -x -C "$WORK/stage"
install -m755 "$WORK/new/kindil" "$WORK/stage/kindle/extensions/kindil/bin/kindil"
COPYFILE_DISABLE=1 tar -C "$WORK/stage/kindle" --no-xattrs -czf "$WORK/bundle.tgz" extensions
shasum -a 256 "$WORK/bundle.tgz" | cut -d' ' -f1 >"$WORK/bundle.sha256"
files=("$WORK/bundle.tgz" "$WORK/bundle.sha256" "$DEVICE_DIR/install.sh")
if [[ -n "$rollback" ]]; then
	fetch "${rollback#v}" "$WORK/rollback"
	cp "$WORK/rollback/kindil" "$WORK/kindil.rollback"
	files+=("$WORK/kindil.rollback")
fi
echo "v$version bundled and verified; waiting for the Kindle"

wait_reachable "$hours"
id="$(date +%s)"
k 'mkdir -p /mnt/us/kindil-deploy && rm -f /mnt/us/kindil-deploy/kindil.rollback'
put "${files[@]}" /mnt/us/kindil-deploy
k "setsid sh /mnt/us/kindil-deploy/install.sh $id </dev/null >/dev/null 2>&1 &"
echo "installing v$version (run $id)"

# install.sh logs "install ID: done", "rolled back" or "failed". The Kindle
# drops off the network while kindil restarts: keep asking.
for _ in $(seq 1 90); do
	outcome="$(k "grep 'install $id: ' /var/local/kindil/deploy.log" || true)"
	if grep -q "install $id: \(done\|rolled back\|failed\)" <<<"$outcome"; then
		echo "$outcome"
		grep -q "install $id: done" <<<"$outcome"
		exit
	fi
	sleep 10
done
die "no outcome after 15 minutes; see /var/local/kindil/deploy.log on the Kindle"
