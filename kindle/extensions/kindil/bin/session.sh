#!/bin/sh
# Runs kindil as an app on the stock OS: Amazon's UI steps aside while it runs
# and always comes back afterwards, whether kindil exits, crashes or is killed.
# Nothing here survives a reboot, so a reboot is always a way back.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$DIR/bin/kindil"
CONFIG=/mnt/us/kindil/kindil.toml

log() { echo "$(date '+%F %T') $*"; }

restore() {
  lipc-set-prop com.lab126.powerd preventScreenSaver 0
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
lipc-set-prop com.lab126.powerd preventScreenSaver 1
# Stopping the GUI job signals its whole tree, which can include us.
trap '' TERM
log "stopping the Kindle UI"
stop lab126_gui
sleep 2
trap restore EXIT
eips -c

if [ -f "$CONFIG" ]; then
  set -- --config "$CONFIG"
else
  set --
fi
log "kindil $("$BIN" --version)"
"$BIN" "$@" kindle
log "kindil exited ($?)"
