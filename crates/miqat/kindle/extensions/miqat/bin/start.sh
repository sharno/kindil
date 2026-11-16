#!/bin/sh
# KUAL entry point: detaches the dashboard loop so KUAL can return before the UI stops.
# The PW1's busybox has no nohup (and maybe no setsid), so plain `&` it is; loop.sh
# ignores the HUP and TERM it gets when the UI's process tree is torn down.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
PIDFILE=/var/tmp/miqat.pid
if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  exit 0
fi
"$DIR/bin/loop.sh" >>"$DIR/miqat.log" 2>&1 </dev/null &
echo $! >"$PIDFILE"
