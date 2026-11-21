#!/bin/sh
# Run by /etc/upstart/kindil.conf at boot: hands over to kindil's launcher,
# whose `boot` is the start-at-boot guard (launcher/src/boot.rs); it exits 0
# whatever the boot. Any other exit (no launcher runs, or it crashed) brings
# Amazon's UI with its framework, as after any boot kindil fails in.
# OVERLAYS (or the same file under /mnt/us's FUSE layer) has the launcher lay
# kindil's runtime overlays over a few of Amazon's files, start at boot or
# not (launcher/src/overlay.rs).
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
[ -f /mnt/us/kindil/BOOT ] || [ -f /mnt/us/kindil/OVERLAYS ] ||
  [ -f /mnt/base-us/kindil/OVERLAYS ] || exit 0
L=/var/local/kindil/launcher
SKIP=/mnt/us/DONT_START_FRAMEWORK
# The launcher's SERVICES (launcher/src/main.rs; a test keeps them alike).
SERVICES="cmd appmgrd archive dpmd dynconfig otaupd otav3 rcm stackdumpd todo deviced cron"
"$L" --version >/dev/null 2>&1 || "$(dirname "$0")/kindil" launcher >/dev/null 2>&1
"$L" boot
code=$?
[ "$code" = 0 ] && exit 0
log() { echo "$(date '+%F %T') boot: $*"; }
running() { status "$1" 2>/dev/null | grep -q start/; }
# A session it started restores Amazon's UI itself when kindil ends.
pid=$(cat /var/tmp/kindil.pid 2>/dev/null)
if [ -n "$pid" ] && grep -q '(launcher) [^Z]' "/proc/$pid/stat" 2>/dev/null; then
  log "the launcher failed ($code); kindil's session runs and restores on its own"
  exit 0
fi
log "the launcher failed ($code); Amazon's UI with its framework"
[ -f "$SKIP" ] || exit 0
rm -f "$SKIP"
for job in powerd wifid $SERVICES; do
  running "$job" || start "$job"
done
if ! running x; then
  # Stopped by a standalone boot: X brings up the rest of Amazon's UI.
  start x
elif running lab126_gui; then
  stop lab126_gui
  start lab126_gui
fi
# Otherwise Amazon's UI is still to come, and sees the flag gone.
exit 0
