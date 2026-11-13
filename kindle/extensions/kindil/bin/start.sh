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
# Run from /var/local, not /mnt/us: in USB drive mode the computer gets /mnt/us
# while kindil keeps running. Copied only when changed, to spare the flash.
RUN=/var/local/kindil
mkdir -p "$RUN" /mnt/us/kindil
for f in session.sh kindil; do
  cmp -s "$DIR/bin/$f" "$RUN/$f" 2>/dev/null || cp "$DIR/bin/$f" "$RUN/$f"
done
# /var/local is 64 MB and also holds Amazon's settings: keep the log small.
LOG="$RUN/kindil.log"
if [ -f "$LOG" ] && [ "$(wc -c <"$LOG")" -gt 262144 ]; then
  tail -c 131072 "$LOG" >"$LOG.old" && mv "$LOG.old" "$LOG"
fi
"$RUN/session.sh" >>"$LOG" 2>&1 </dev/null &
echo $! >"$PIDFILE"
