#!/bin/sh
# KUAL entry point: detaches the session so KUAL can return before the UI stops.
# The PW1's busybox has no nohup, so plain `&`; session.sh ignores the HUP and
# TERM it gets when Amazon's UI process tree is torn down.
# Upstart's stop/start and eips live in sbin; KUAL has it on PATH, SSH does not.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
DIR="$(cd "$(dirname "$0")/.." && pwd)"
PIDFILE=/var/tmp/kindil.pid
if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  exit 0
fi
mkdir -p /mnt/us/kindil
"$DIR/bin/session.sh" >>/mnt/us/kindil/kindil.log 2>&1 </dev/null &
echo $! >"$PIDFILE"
