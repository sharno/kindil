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
# Amazon's services that a standalone boot stops (bin/boot.sh, which keeps
# the same list).
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
  lipc-set-prop com.lab126.powerd preventScreenSaver 0
  lipc-set-prop com.lab126.blanket load usb
  lipc-set-prop com.lab126.blanket load splash
  log "starting the Kindle UI"
  wifi_back
  # A standalone boot stopped these and X: Amazon's UI needs them all.
  for job in $SERVICES; do running "$job" || start "$job"; done
  if running x; then start lab126_gui; else start x; fi
  rm -f /var/tmp/kindil.pid
}

# Starting or running, by upstart's goal.
running() { status "$1" 2>/dev/null | grep -q start/; }

# kindil ran Wi-Fi itself (src/net.rs): its DHCP client and wpa_supplicant
# go, and wifid starts Amazon's driver and supplicant jobs afresh (stopping
# wifim unloads the driver kindil may have loaded or unloaded itself).
KINDIL_WIFI="/var/run/kindil-udhcpc.pid /var/run/kindil-wpa.pid"
wifi_back() {
  ran=""
  for pidfile in $KINDIL_WIFI; do
    if [ -f "$pidfile" ]; then
      kill "$(cat "$pidfile")" 2>/dev/null
      rm -f "$pidfile"
      ran=1
    fi
  done
  if [ -n "$ran" ] && ! running wifid; then
    stop wifis
    stop wifim
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
pick
if [ ! -x "$BIN" ]; then
  log "missing $BIN"
  exit 1
fi
# powerd then ignores the power button and never sleeps on its own: kindil
# hears the button through kernel uevents and runs sleep itself.
lipc-set-prop com.lab126.powerd preventScreenSaver 1
# Stopping the GUI job signals its whole tree, which can include us.
trap '' TERM
# A standalone boot never started it.
if running lab126_gui; then
  # Let KUAL close its menu before the UI goes away.
  sleep 2
  log "stopping the Kindle UI"
  stop lab126_gui
  sleep 2
fi
trap restore EXIT
# Amazon's screen modules would draw over kindil in USB drive mode and after
# it; kindil draws its own screens.
lipc-set-prop com.lab126.blanket unload usb
lipc-set-prop com.lab126.blanket unload splash
eips -c

if [ -f "$CONFIG" ]; then
  set -- --config "$CONFIG"
else
  set --
fi
while :; do
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
