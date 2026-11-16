#!/bin/sh
# Takes over the screen and shows the server's PNG every INTERVAL seconds.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=SCRIPTDIR/../miqat.conf
. "$DIR/miqat.conf"

IMG=/var/tmp/miqat.png
PART=/var/tmp/miqat.part

log() { echo "$(date '+%F %T') $*"; }

take_over() {
  # Let KUAL close its menu before the UI goes away.
  sleep 2
  lipc-set-prop com.lab126.powerd preventScreenSaver 1
  # Stopping the GUI job signals its whole tree, which can include us (KOReader does the same).
  trap '' TERM
  log "stopping the Kindle UI"
  stop lab126_gui
  sleep 2
  log "UI stopped"
  trap restore TERM INT
  [ -n "$FRONTLIGHT" ] && lipc-set-prop com.lab126.powerd flIntensity "$FRONTLIGHT"
  eips -c
}

restore() {
  log "stopping"
  lipc-set-prop com.lab126.powerd preventScreenSaver 0
  start lab126_gui
  exit 0
}

reconnect_wifi() {
  log "reconnecting wifi"
  lipc-set-prop com.lab126.wifid enable 0
  sleep 2
  lipc-set-prop com.lab126.wifid enable 1
}

trap '' HUP
log "starting with $SERVER_URL"
take_over

frame=0
failures=0
while :; do
  if curl -fsS -m 30 -o "$PART" "$SERVER_URL"; then
    mv "$PART" "$IMG"
    failures=0
    [ "$frame" -eq 0 ] && log "first frame"
    if [ $((frame % FULL_REFRESH_EVERY)) -eq 0 ]; then
      eips -f -g "$IMG"
    else
      eips -g "$IMG"
    fi
    frame=$((frame + 1))
  else
    failures=$((failures + 1))
    log "fetch failed ($failures)"
    # The last frame stays up; mark it so a frozen clock isn't mistaken for the time.
    eips 1 1 " OFFLINE "
    [ $((failures % 3)) -eq 0 ] && reconnect_wifi
  fi
  # Sleep to the next INTERVAL boundary so minutes flip on time.
  # Background + wait so a TERM from stop.sh is handled right away, not after the sleep.
  sleep $((INTERVAL - $(date +%s) % INTERVAL)) &
  wait $!
done
