#!/bin/sh
# KUAL: start kindil at boot without Amazon's Java framework from now on, with
# a fresh count of failed boots. bin/boot.sh skips the framework from the boot
# after the next one, once kindil has stayed up.
mkdir -p /mnt/us/kindil
echo standalone >/mnt/us/kindil/BOOT
rm -f /var/local/kindil/boot-tries
