#!/bin/sh
# Stops the dashboard and brings the Kindle UI back. With the UI stopped KUAL is gone,
# so run this over SSH (USBNetwork), or just hold the power button ~15 s to reboot.
PIDFILE=/var/tmp/miqat.pid
[ -f "$PIDFILE" ] && kill "$(cat "$PIDFILE")" 2>/dev/null
rm -f "$PIDFILE"
