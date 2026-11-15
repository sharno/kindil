#!/bin/sh
# Runs on the Kindle, detached (scripts/device/release.sh starts it): installs
# the bundle in /mnt/us/kindil-deploy, then insists that kindil comes back
# with its own Wi-Fi online (or just running, with Wi-Fi switched off) and is
# still so a minute later. Otherwise it puts the rollback binary back: the one
# sent along as kindil.rollback, or else the one being replaced.
#
# Usage: install.sh RUN_ID   (logged, so the caller can find its outcome)
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
B=/mnt/us/kindil-deploy
E=/mnt/us/extensions/kindil
RUN=/var/local/kindil
# kindil stopping fully: its session restores Amazon's UI on the way out.
SETTLE=20
# How long kindil gets to come back online, then to stay so.
ONLINE_WAIT=180
HOLD=60
ID="${1:-manual}"
mkdir -p "$RUN"
exec >>"$RUN/deploy.log" 2>&1

log() { echo "$(date '+%F %T') install $ID: $*"; }
up() {
  pidof kindil >/dev/null || return 1
  # Wi-Fi switched off in kindil: running is all there is to check.
  [ -f "$RUN/wifi-off" ] && return 0
  # shellcheck disable=SC2009 # the PW1's busybox has no pgrep
  ps -o args | grep -q "^$RUN/wpa_supplicant" &&
    wget -q -T 3 -O /dev/null http://example.com 2>/dev/null
}
rollback() {
  log "$1; rolling back to $("$RUN/kindil.rollback" --version)"
  killall kindil
  sleep "$SETTLE"
  cp "$RUN/kindil.rollback" "$E/bin/kindil.new" && mv "$E/bin/kindil.new" "$E/bin/kindil"
  "$E/bin/start.sh"
  log "rolled back"
  exit 1
}

cd "$B" || { log "failed: no $B"; exit 1; }
# busybox sha256sum has no -c.
[ "$(sha256sum bundle.tgz | cut -d' ' -f1)" = "$(cat bundle.sha256)" ] ||
  { log "failed: checksum mismatch"; exit 1; }
rm -rf x
if ! { mkdir x && tar xzf bundle.tgz -C x; }; then
  log "failed: can't unpack"
  exit 1
fi
new=x/extensions/kindil
version="$("$new/bin/kindil" --version)" || { log "failed: the new binary won't run"; exit 1; }
if [ -f kindil.rollback ]; then
  cp kindil.rollback "$RUN/kindil.rollback"
else
  cp -p "$E/bin/kindil" "$RUN/kindil.rollback"
fi
chmod 755 "$RUN/kindil.rollback"
log "installing $version"

killall kindil
sleep "$SETTLE"
mkdir -p "$E/bin"
# The launcher's fallback after repeated crashes: the version being replaced.
if [ -f "$E/bin/kindil" ]; then
  cp "$E/bin/kindil" "$E/bin/kindil.previous.new" && mv "$E/bin/kindil.previous.new" "$E/bin/kindil.previous"
fi
# Every file in the bundle, each written aside and moved into place.
(cd "$new" && find . -type f) | while read -r f; do
  mkdir -p "$E/$(dirname "$f")"
  cp "$new/$f" "$E/$f.new" && mv "$E/$f.new" "$E/$f"
done
chmod 755 "$E/bin/kindil" "$E"/bin/*.sh
[ -d "$E/boot" ] && chmod 755 "$E"/boot/*.sh
"$E/bin/start.sh"

t0=$(date +%s)
until up; do
  [ $(($(date +%s) - t0)) -lt "$ONLINE_WAIT" ] || rollback "not up ${ONLINE_WAIT} s after installing"
  sleep 5
done
log "up $(($(date +%s) - t0)) s after installing"
sleep "$HOLD"
up || rollback "down again ${HOLD} s later"
log "done: $version"
