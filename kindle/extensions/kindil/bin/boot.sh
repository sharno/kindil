#!/bin/sh
# Run by /etc/upstart/kindil.conf at boot: hands over to kindil's launcher,
# whose `boot` is the start-at-boot guard (launcher/src/boot.rs). If no
# launcher can run, Amazon's UI comes with its framework, as after any boot
# kindil fails in.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
[ -f /mnt/us/kindil/BOOT ] || exit 0
L=/var/local/kindil/launcher
"$L" --version >/dev/null 2>&1 || "$(dirname "$0")/kindil" launcher >/dev/null 2>&1
"$L" boot
case $? in
126 | 127)
  echo "$(date '+%F %T') boot: no launcher runs; Amazon's UI with its framework"
  if [ -f /mnt/us/DONT_START_FRAMEWORK ]; then
    rm -f /mnt/us/DONT_START_FRAMEWORK
    status x | grep -q start/ || start x
  fi
  ;;
esac
exit 0
