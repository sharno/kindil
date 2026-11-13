#!/bin/sh
# Adds (install) or removes (remove) kindil's boot job: the one system file
# kindil ever writes. Run as root over SSH:
#   sh /mnt/us/extensions/kindil/boot/install.sh install
# With the job installed, KUAL's "Start at boot: on/off" decides whether
# kindil starts at boot; removing the job undoes start at boot entirely.
set -eu
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
JOB=/etc/upstart/kindil.conf
SOURCE="$(dirname "$0")/kindil.conf"
# The root filesystem goes back to read-only however this ends.
trap 'mntroot ro' EXIT
case "${1:-}" in
install)
  mntroot rw
  cp "$SOURCE" "$JOB.part"
  mv "$JOB.part" "$JOB"
  ;;
remove)
  mntroot rw
  rm -f "$JOB" "$JOB.part"
  ;;
*)
  echo "usage: $0 install|remove" >&2
  trap - EXIT
  exit 2
  ;;
esac
sync
echo "done: $1"
