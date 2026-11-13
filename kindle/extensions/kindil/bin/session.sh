#!/bin/sh
# Runs kindil as an app on the stock OS: Amazon's UI steps aside while it runs
# and always comes back afterwards, whether kindil exits, crashes or is killed.
# Nothing here survives a reboot, so a reboot is always a way back.
# Upstart's stop/start and eips live in sbin; KUAL has it on PATH, SSH does not.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
BIN="$(dirname "$0")/kindil"
CONFIG=/mnt/us/kindil/kindil.toml

log() { echo "$(date '+%F %T') $*"; }

restore() {
  lipc-set-prop com.lab126.powerd preventScreenSaver 0
  lipc-set-prop com.lab126.blanket load usb
  lipc-set-prop com.lab126.blanket load splash
  log "starting the Kindle UI"
  start lab126_gui
  rm -f /var/tmp/kindil.pid
}

trap '' HUP
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
log "$("$BIN" --version)"
"$BIN" "$@" kindle
log "kindil exited ($?)"
