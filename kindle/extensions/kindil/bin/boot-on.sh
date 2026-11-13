#!/bin/sh
# KUAL: start kindil at boot from now on, with a fresh count of failed boots.
mkdir -p /mnt/us/kindil
touch /mnt/us/kindil/BOOT
rm -f /var/local/kindil/boot-tries
