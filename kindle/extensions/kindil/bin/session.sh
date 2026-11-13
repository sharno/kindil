#!/bin/sh
# Runs kindil as an app on the stock OS: Amazon's UI steps aside while it runs
# and always comes back afterwards, whether kindil exits, crashes or is killed.
# Nothing here survives a reboot, so a reboot is always a way back.
# Upstart's stop/start and eips live in sbin; KUAL has it on PATH, SSH does not.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
RUN="$(dirname "$0")"
BIN="$RUN/kindil"
EXT=/mnt/us/extensions/kindil/bin
CONFIG=/mnt/us/kindil/kindil.toml
# Present while a new version is on trial; kindil removes it once it has run
# for two minutes (src/update.rs). Still present after it exits: roll back.
TRYING="$RUN/trying"
# kindil's exit code after staging an update: run again.
RESTART=75

log() { echo "$(date '+%F %T') $*"; }

restore() {
  lipc-set-prop com.lab126.powerd preventScreenSaver 0
  lipc-set-prop com.lab126.blanket load usb
  lipc-set-prop com.lab126.blanket load splash
  log "starting the Kindle UI"
  start lab126_gui
  rm -f /var/tmp/kindil.pid
}

# Copies the binary to run into /var/local, which stays when a computer takes
# /mnt/us. A staged update (kindil.next, written by kindil after checking its
# signature) gets one trial run; otherwise the current binary runs.
pick() {
  if [ -f "$EXT/kindil.next" ] && mv -f "$EXT/kindil.next" "$EXT/kindil.trying"; then
    if cp "$EXT/kindil.trying" "$BIN"; then
      touch "$TRYING"
      log "trying the update"
      return
    fi
    rm -f "$EXT/kindil.trying"
  fi
  if [ -f "$EXT/kindil" ]; then
    cmp -s "$EXT/kindil" "$BIN" || cp "$EXT/kindil" "$BIN"
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
# Let KUAL close its menu before the UI goes away.
sleep 2
# powerd then ignores the power button and never sleeps on its own: kindil
# hears the button through kernel uevents and runs sleep itself.
lipc-set-prop com.lab126.powerd preventScreenSaver 1
# Stopping the GUI job signals its whole tree, which can include us.
trap '' TERM
log "stopping the Kindle UI"
stop lab126_gui
sleep 2
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
  elif [ "$status" -ne "$RESTART" ]; then
    break
  fi
  pick
done
