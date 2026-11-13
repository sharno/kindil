#!/bin/sh
# Run by /etc/upstart/kindil.conf whenever Amazon's UI starts: at boot, and
# again each time kindil hands the screen back. Starts kindil once per boot,
# only while /mnt/us/kindil/BOOT exists, and stops trying after boots where
# kindil didn't stay up: then the Kindle boots to Amazon's UI as before.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
FLAG=/mnt/us/kindil/BOOT
STATE=/var/local/kindil
BOOT_ID=/proc/sys/kernel/random/boot_id
START="$(dirname "$0")/start.sh"
PIDFILE=/var/tmp/kindil.pid
# The boot kindil last started in: Amazon's UI coming back later in that boot
# (Exit to Kindle, a crash) must not start kindil again.
BOOTED="$STATE/booted"
# Boots in a row where kindil started but didn't stay up for SETTLE seconds.
TRIES="$STATE/boot-tries"
LIMIT=2
SETTLE=120

log() { echo "$(date '+%F %T') boot: $*"; }

[ -f "$FLAG" ] || exit 0
mkdir -p "$STATE"
boot="$(cat "$BOOT_ID")"
[ "$(cat "$BOOTED" 2>/dev/null)" = "$boot" ] && exit 0
echo "$boot" >"$BOOTED"
tries="$(cat "$TRIES" 2>/dev/null || echo 0)"
if [ "$tries" -ge "$LIMIT" ]; then
  log "kindil didn't stay up for $LIMIT boots; staying with Amazon's UI (turn start at boot on again in KUAL)"
  exit 0
fi
echo $((tries + 1)) >"$TRIES"
# Let Amazon's UI get going; start.sh stops it again.
sleep 10
# Nothing runs kindil this early in a boot, and a pid file left by the last
# boot could name an unrelated process now: start.sh would think kindil runs.
rm -f "$PIDFILE"
log "starting kindil"
"$START"
sleep "$SETTLE"
if kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then
  rm -f "$TRIES"
  log "kindil is up"
fi
