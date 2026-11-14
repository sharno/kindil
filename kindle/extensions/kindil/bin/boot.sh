#!/bin/sh
# Run by /etc/upstart/kindil.conf once powerd, wifid and volumd start, before
# X and Amazon's UI (jobs installed before 0.6.2 run it whenever Amazon's UI
# starts instead). Starts kindil once per boot, only while
# /mnt/us/kindil/BOOT exists, and stops trying after boots where kindil
# didn't stay up: then the Kindle boots to Amazon's UI as before.
# BOOT holding "standalone" also skips Amazon's UI from the next boot on;
# any boot where kindil doesn't stay up brings it back.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
FLAG=/mnt/us/kindil/BOOT
# Amazon's framework job skips the Java framework while this exists; kindil
# then also skips X and the rest of Amazon's UI.
SKIP=/mnt/us/DONT_START_FRAMEWORK
STATE=/var/local/kindil
BOOT_ID=/proc/sys/kernel/random/boot_id
START="$(dirname "$0")/start.sh"
PIDFILE=/var/tmp/kindil.pid
# The boot kindil last started in: the job running again later in that boot
# (Amazon's UI coming back, with jobs from before 0.6.2) must not start
# kindil again.
BOOTED="$STATE/booted"
# Boots in a row where kindil started but didn't stay up for SETTLE seconds.
TRIES="$STATE/boot-tries"
LIMIT=2
SETTLE=120
# How long a boot may take to bring up what kindil waits for.
PATIENCE=120
# Amazon's services that kindil doesn't use, which a standalone boot stops:
# connections (cmd, with tmd and phd), apps, archive, power management
# helpers, dynamic config, updates, crash dumps and sync. kindil keeps
# powerd, wifid and volumd. Amazon's UI needs them all back; session.sh
# keeps the same list (tests/launcher.sh checks).
SERVICES="cmd appmgrd archive dpmd dynconfig otaupd otav3 rcm stackdumpd todo deviced"

log() { echo "$(date '+%F %T') boot: $*"; }

standalone() { [ "$(cat "$FLAG" 2>/dev/null)" = standalone ]; }

# Starting or running, by upstart's goal.
running() { status "$1" 2>/dev/null | grep -q start/; }

# Waits until the check "$@" passes, for up to PATIENCE seconds.
await() {
  n=0
  until "$@"; do
    n=$((n + 1))
    [ "$n" -lt $((PATIENCE * 2)) ] || return 1
    usleep 500000
  done
}

# Whether the lipc service $1 answers for its property $2.
answers() { lipc-get-prop "$1" "$2" >/dev/null 2>&1; }

# Runs upstart's $1 (start or stop) on each of SERVICES that isn't so already.
services() {
  for job in $SERVICES; do
    case "$1" in
    start) running "$job" || start "$job" ;;
    stop) ! running "$job" || stop "$job" ;;
    esac
  done
}

# A standalone boot has no Amazon UI to fall back on: bring it back with its
# framework, now and for the next boots.
amazon_ui() {
  [ -f "$SKIP" ] || return 0
  rm -f "$SKIP"
  log "starting Amazon's UI with its framework"
  # kindil stops wifid while it runs Wi-Fi itself; session.sh starts it
  # again, unless kindil never got that far.
  running wifid || start wifid
  services start
  if ! running x; then
    # Stopped by a standalone boot: X brings up the rest of Amazon's UI.
    start x
  elif running lab126_gui; then
    stop lab126_gui
    start lab126_gui
  fi
  # Otherwise Amazon's UI is still to come, and sees the flag gone.
}

[ -f "$FLAG" ] || exit 0
mkdir -p "$STATE"
boot="$(cat "$BOOT_ID")"
[ "$(cat "$BOOTED" 2>/dev/null)" = "$boot" ] && exit 0
echo "$boot" >"$BOOTED"
tries="$(cat "$TRIES" 2>/dev/null || echo 0)"
if [ "$tries" -ge "$LIMIT" ]; then
  log "kindil didn't stay up for $LIMIT boots; staying with Amazon's UI (turn start at boot on again in KUAL)"
  amazon_ui
  exit 0
fi
echo $((tries + 1)) >"$TRIES"
if [ -f "$SKIP" ]; then
  # kindil draws to the framebuffer itself: X, about 9 s of the boot, and
  # all that waits for it go unused.
  log "skipping X and Amazon's UI"
  stop x
  # Amazon's boot spinner stops only when X's language picker starts: left
  # running, it keeps drawing over kindil.
  stop early_progress
  await answers com.lab126.powerd state || log "powerd isn't answering"
  await answers com.lab126.wifid enable || log "wifid isn't answering"
  # What X's language picker does at boot to set up the front light.
  lipc-set-prop com.lab126.powerd flStartup 1
else
  # Let Amazon's UI get going; start.sh stops it again.
  await running lab126_gui || log "Amazon's UI didn't start"
  sleep 10
fi
# Nothing runs kindil this early in a boot, and a pid file left by the last
# boot could name an unrelated process now: start.sh would think kindil runs.
rm -f "$PIDFILE"
log "starting kindil"
"$START"
if [ -f "$SKIP" ]; then
  # After kindil starts, which these 8 s would otherwise hold up, and after
  # powerd answers: deviced starts once powerd has.
  log "stopping Amazon's services kindil doesn't use"
  services stop
fi
sleep "$SETTLE"
if kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then
  rm -f "$TRIES"
  log "kindil is up"
  # Proven on this boot: the next one can skip Amazon's UI.
  if standalone; then touch "$SKIP"; fi
else
  log "kindil didn't stay up"
  amazon_ui
fi
