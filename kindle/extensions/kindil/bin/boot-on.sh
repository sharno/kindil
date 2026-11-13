#!/bin/sh
# KUAL: start kindil at boot after Amazon's UI from now on, with a fresh count
# of failed boots.
mkdir -p /mnt/us/kindil
: >/mnt/us/kindil/BOOT
rm -f /mnt/us/DONT_START_FRAMEWORK /var/local/kindil/boot-tries
