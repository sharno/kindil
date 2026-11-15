#!/bin/sh
# Runs on the Kindle, detached (scripts/device/release.sh starts it): installs
# the bundle in /mnt/us/kindil-deploy, then insists that the new version is
# the one running, with its own Wi-Fi joined and addressed (or just running,
# with Wi-Fi switched off), and still so a minute later. Otherwise it puts
# the whole extension back as it was, launcher files included, with the
# rollback binary sent along as kindil.rollback if there is one.
#
# Usage: install.sh RUN_ID   (logged, so the caller can find its outcome)
#
# shellcheck disable=SC2015 # `a && b || fail`: fail if any step fails, as meant
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
B=/mnt/us/kindil-deploy
E=/mnt/us/extensions/kindil
RUN=/var/local/kindil
# kindil stopping fully: its session restores Amazon's UI on the way out.
SETTLE=20
# How long kindil gets to come back up, then to stay so.
ONLINE_WAIT=180
HOLD=60
ID="${1:-manual}"
BACKUP="$RUN/extension-backup.tgz"
mkdir -p "$RUN"
exec >>"$RUN/deploy.log" 2>&1

log() { echo "$(date '+%F %T') install $ID: $*"; }
fail() {
  log "failed: $1"
  exit 1
}
# The new version runs (not a fallback the launcher fell back to), and has
# its Wi-Fi: kindil's own supplicant, joined, with an address.
up() {
  pidof kindil >/dev/null || return 1
  [ "$("$RUN/kindil" --version 2>/dev/null)" = "$version" ] || return 1
  [ -f "$RUN/wifi-off" ] && return 0
  # shellcheck disable=SC2009 # the PW1's busybox has no pgrep
  ps -o args | grep -q "^$RUN/wpa_supplicant" || return 1
  link="$(wpa_cli -i wlan0 status 2>/dev/null)"
  echo "$link" | grep -q "^wpa_state=COMPLETED" && echo "$link" | grep -q "^ip_address="
}
rollback() {
  log "$1; rolling back"
  killall kindil
  sleep "$SETTLE"
  rm -rf "$E" && mkdir -p "$E" && tar xzf "$BACKUP" -C "$E" || log "restoring the backup failed"
  if [ -f "$RUN/kindil.rollback" ]; then
    cp "$RUN/kindil.rollback" "$E/bin/kindil.new" && mv "$E/bin/kindil.new" "$E/bin/kindil"
  fi
  "$E/bin/start.sh"
  log "rolled back to $("$E/bin/kindil" --version 2>/dev/null)"
  exit 1
}

cd "$B" || fail "no $B"
# busybox sha256sum has no -c.
[ "$(sha256sum bundle.tgz | cut -d' ' -f1)" = "$(cat bundle.sha256)" ] || fail "checksum mismatch"
rm -rf x
mkdir x && tar xzf bundle.tgz -C x || fail "can't unpack"
new=x/extensions/kindil
version="$("$new/bin/kindil" --version)" || fail "the new binary won't run"
rm -f "$RUN/kindil.rollback"
if [ -f kindil.rollback ]; then
  cp kindil.rollback "$RUN/kindil.rollback" && chmod 755 "$RUN/kindil.rollback" ||
    fail "can't keep the rollback binary"
fi
# Everything as it is now, to put back if the new version doesn't come up.
tar czf "$BACKUP" -C "$E" . || fail "can't back up $E"
log "installing $version"

killall kindil
sleep "$SETTLE"
mkdir -p "$E/bin" || rollback "can't write $E"
# The launcher's fallback after repeated crashes: the version being replaced.
if [ -f "$E/bin/kindil" ]; then
  cp "$E/bin/kindil" "$E/bin/kindil.previous.new" &&
    mv "$E/bin/kindil.previous.new" "$E/bin/kindil.previous" ||
    rollback "can't keep the previous version"
fi
# Every file in the bundle, each written aside and moved into place.
(cd "$new" && find . -type f) >"$B/files" || rollback "can't list the bundle"
while read -r f; do
  mkdir -p "$E/$(dirname "$f")" &&
    cp "$new/$f" "$E/$f.new" &&
    mv "$E/$f.new" "$E/$f" ||
    rollback "can't install $f"
done <"$B/files"
chmod 755 "$E/bin/kindil" "$E"/bin/*.sh || rollback "can't make the launcher executable"
if [ -d "$E/boot" ]; then chmod 755 "$E"/boot/*.sh || rollback "can't make the boot job executable"; fi
"$E/bin/start.sh"

t0=$(date +%s)
until up; do
  [ $(($(date +%s) - t0)) -lt "$ONLINE_WAIT" ] || rollback "$version not up ${ONLINE_WAIT} s after installing"
  sleep 5
done
log "up $(($(date +%s) - t0)) s after installing"
sleep "$HOLD"
up || rollback "$version down again ${HOLD} s later"
log "done: $version"
