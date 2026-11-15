#!/bin/sh
# Runs kindil as an app on the stock OS: Amazon's UI steps aside while it runs
# and always comes back afterwards, whether kindil exits, crashes or is killed.
# Nothing here survives a reboot, so a reboot is always a way back, except
# for a standalone start at boot, whose own guard is bin/boot.sh.
# Upstart's stop/start and eips live in sbin; KUAL has it on PATH, SSH does not.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
RUN="$(dirname "$0")"
BIN="$RUN/kindil"
EXT=/mnt/us/extensions/kindil/bin
CONFIG=/mnt/us/kindil/kindil.toml
SKIP=/mnt/us/DONT_START_FRAMEWORK
CHARGER=/sys/devices/platform/aplite_charger.0/charging
# Amazon's services kindil has no use for, stopped while it runs (boot.sh
# keeps the same list, to bring them back after a boot kindil fails in).
SERVICES="cmd appmgrd archive dpmd dynconfig otaupd otav3 rcm stackdumpd todo deviced"
# Present while a new version is on trial; kindil removes it once it has run
# for two minutes (src/update.rs). Still present after it exits: roll back.
TRYING="$RUN/trying"
# kindil's exit code after staging an update: run again.
RESTART=75
# A crash restarts kindil, unless it crashed this often within this many
# seconds: then that version is broken, and the one before it (kept by the
# last update) gets the same chances before Amazon's UI comes back.
CRASHES=3
WINDOW=300
crashes=""
# The slot that runs: kindil, or kindil.previous once kindil kept crashing.
# Only for this session, so the slots on disk never change behind an update.
SLOT="$EXT/kindil"

log() { echo "$(date '+%F %T') $*"; }

restore() {
  # A standalone boot skipped Amazon's framework: without it, Amazon's UI is
  # an empty screen. boot.sh sets the flag again after a boot kindil stays up.
  rm -f "$SKIP"
  # Nothing may stop a service once this starts bringing them back.
  if [ -n "$STOPPER" ]; then
    kill "$STOPPER" 2>/dev/null
    wait "$STOPPER" 2>/dev/null
  fi
  # kindil may have paused charging for the temperature (src/power.rs);
  # powerd, back now, keeps its own range.
  echo 1 >"$CHARGER" 2>/dev/null
  running powerd || start powerd
  if [ -n "$GUI" ]; then
    lipc-set-prop com.lab126.blanket load usb
    lipc-set-prop com.lab126.blanket load splash
  fi
  log "starting the Kindle UI"
  wifi_back
  # Amazon's UI needs them all, and X.
  for job in $SERVICES; do running "$job" || start "$job"; done
  if running x; then start lab126_gui; else start x; fi
  rm -f /var/tmp/kindil.pid
}

# Starting or running, by upstart's goal.
running() { status "$1" 2>/dev/null | grep -q start/; }

# kindil ran Wi-Fi itself (src/net.rs): its DHCP client and wpa_supplicant
# go, and wifid starts Amazon's supplicant afresh. The driver stays loaded:
# unloading it can hang, and wifid's driver job is fine with it loaded.
KINDIL_WIFI="/var/run/kindil-udhcpc.pid /var/run/kindil-wpa.pid"

# Ends the process a pid file names and confirms it is gone: TERM, then KILL
# after 3 s, so nothing of kindil's Wi-Fi overlaps what starts next.
end_pid() {
  pid="$(cat "$1" 2>/dev/null)"
  rm -f "$1"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 0
  kill "$pid" 2>/dev/null
  n=0
  while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 30 ]; do
    usleep 100000
    n=$((n + 1))
  done
  kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null
  return 0
}

wifi_back() {
  ran=""
  for pidfile in $KINDIL_WIFI; do
    if [ -f "$pidfile" ]; then
      end_pid "$pidfile"
      ran=1
    fi
  done
  if [ -n "$ran" ] && ! running wifid; then
    stop wifis
  fi
  running wifid || start wifid
}

# Copies the binary to run into /var/local, which stays when a computer takes
# /mnt/us. A staged update (kindil.next, written by kindil after checking its
# signature) gets one trial run; otherwise the current binary runs.
# Records a crash; fails once there were too many lately.
crashed() {
  now=$(date +%s)
  crashes="$(for t in $crashes; do [ $((now - t)) -lt $WINDOW ] && echo "$t"; done) $now"
  [ "$(echo "$crashes" | wc -w)" -lt "$CRASHES" ]
}

# Comparing the 16 MB binaries takes over 10 s at boot. Instead the copy
# records its source and keeps the source's modification time (cp -p sets it
# last, so a cut-short copy never matches): the same source with the same
# time means the copy is current.
FROM="$BIN.from"
current() {
  [ "$(cat "$FROM" 2>/dev/null)" = "$1" ] && [ ! "$1" -nt "$BIN" ] && [ ! "$BIN" -nt "$1" ]
}
copy() { rm -f "$FROM" && cp -p "$1" "$BIN" && echo "$1" >"$FROM"; }

pick() {
  if [ -f "$EXT/kindil.next" ] && mv -f "$EXT/kindil.next" "$EXT/kindil.trying"; then
    if copy "$EXT/kindil.trying"; then
      touch "$TRYING"
      log "trying the update"
      return
    fi
    rm -f "$EXT/kindil.trying"
  fi
  if [ -f "$SLOT" ]; then
    current "$SLOT" || copy "$SLOT"
  fi
}

trap '' HUP
# A trial cut short by a reboot or a kill never settled: drop it, so the
# current binary runs and nothing keeps the untried one.
if [ -f "$TRYING" ]; then
  rm -f "$TRYING" "$EXT/kindil.trying"
  log "dropped an update whose trial was cut short"
fi
# Event watchers an earlier kindil left behind (orphans, now init's); they
# ignore TERM.
for watcher in $(pidof lipc-wait-event); do
  [ "$(cut -d' ' -f4 "/proc/$watcher/stat" 2>/dev/null)" = 1 ] && kill -9 "$watcher"
done
pick
if [ ! -x "$BIN" ]; then
  log "missing $BIN"
  exit 1
fi
# Stopping the GUI job signals its whole tree, which can include us.
trap '' TERM
# A standalone boot never started it.
GUI=""
if running lab126_gui; then
  GUI=1
  # Let KUAL close its menu before the UI goes away.
  sleep 2
  log "stopping the Kindle UI"
  stop lab126_gui
  sleep 2
fi
# kindil draws to the framebuffer itself: X has nothing to do while it runs.
# The way back starts it again, and Amazon's UI with it.
running x && stop x
# kindil does powerd's work itself: battery, front light, charging limits,
# sleep and the power button. Stopped first, and before kindil starts:
# stopping powerd switches the front light off, which kindil then sets.
if running powerd; then
  log "stopping powerd"
  stop powerd
fi
# The rest in the background, so kindil starts at once; for a minute, since
# at boot some may still be on their way up. restore() stops this first.
(
  n=0
  while [ "$n" -lt 12 ]; do
    for job in $SERVICES; do ! running "$job" || stop "$job"; done
    n=$((n + 1))
    sleep 5
  done
) >/dev/null 2>&1 &
STOPPER=$!
trap restore EXIT
# Amazon's screen modules would draw over kindil in USB drive mode and after
# it; kindil draws its own screens. Only Amazon's UI loads them.
if [ -n "$GUI" ]; then
  lipc-set-prop com.lab126.blanket unload usb
  lipc-set-prop com.lab126.blanket unload splash
fi
eips -c

if [ -f "$CONFIG" ]; then
  set -- --config "$CONFIG"
else
  set --
fi
# kindil's own Wi-Fi children (src/net.rs) from a run that crashed or was
# killed: stopped before it runs again, which starts its Wi-Fi afresh.
stop_children() {
  for pidfile in $KINDIL_WIFI; do end_pid "$pidfile"; done
}

while :; do
  stop_children
  log "$("$BIN" --version)"
  "$BIN" "$@" kindle
  status=$?
  log "kindil exited ($status)"
  if [ -f "$TRYING" ]; then
    # The trial ended before kindil kept it: back to the previous binary.
    rm -f "$TRYING" "$EXT/kindil.trying"
    log "the update ended early; back to the previous version"
    [ "$status" -eq 0 ] && break
  else
    case "$status" in
    "$RESTART") ;;
    # Exit to Kindle, or stopped on purpose (Ctrl-C, kill).
    0 | 130 | 143) break ;;
    *)
      if crashed; then
        log "restarting after a crash"
      elif [ "$SLOT" = "$EXT/kindil" ] && [ -f "$EXT/kindil.previous" ]; then
        SLOT="$EXT/kindil.previous"
        crashes=""
        log "crashed $CRASHES times in $WINDOW s; trying the previous version"
      else
        log "crashed $CRASHES times in $WINDOW s; giving up"
        break
      fi
      sleep 2
      ;;
    esac
  fi
  pick
done
