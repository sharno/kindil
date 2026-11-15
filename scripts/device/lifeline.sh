#!/bin/sh
# Runs on the Kindle beside an experiment (`cargo xtask run` starts both):
#   lifeline.sh DEADLINE_SECONDS TEST_PID
# When the test exits, or at the deadline (wall clock), it kills the test if
# still running, clears any wake alarm and resumes a paused kindil. If the
# Kindle is then offline: with kindil running, kindil is restarted (its Wi-Fi
# afresh), and if still offline, stopped, which hands Wi-Fi to Amazon's wifid;
# with no kindil running, wifid is started afresh. It never starts wifid
# behind a running kindil's back.
#
# It can only act while the Kindle is awake: no help for tests that keep it
# asleep or dozing (see the README's device section).
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
exec >>/var/local/kindil/lifeline.log 2>&1
DEADLINE=$(($(date +%s) + ${1:-3600}))
TEST=$2
E=/mnt/us/extensions/kindil/bin
log() { echo "$(date '+%F %T') lifeline: $*"; }
online() {
  n=0
  while [ "$n" -lt "$1" ]; do
    wget -q -T 3 -O /dev/null http://example.com 2>/dev/null && return 0
    n=$((n + 1))
    sleep 3
  done
  return 1
}

log "test $TEST, deadline in ${1:-3600} s"
while [ "$(date +%s)" -lt "$DEADLINE" ] && kill -0 "$TEST" 2>/dev/null; do
  sleep 10
done
if kill -0 "$TEST" 2>/dev/null; then
  log "deadline: killing test $TEST"
  kill "$TEST"
  sleep 2
  kill -9 "$TEST" 2>/dev/null
fi
for alarm in /sys/class/rtc/rtc*/wakealarm; do echo 0 >"$alarm"; done 2>/dev/null
for k in $(pidof kindil); do kill -CONT "$k"; done
online 20 && { log "online, nothing to do"; exit 0; }
if pidof kindil >/dev/null; then
  log "offline: restarting kindil"
  killall kindil
  sleep 25
  "$E/start.sh"
  online 40 && { log "online after restarting kindil"; exit 0; }
  log "STILL OFFLINE: stopping kindil; Amazon's UI and wifid take over"
  killall kindil
  sleep 25
else
  log "offline, no kindil: starting wifid afresh"
  killall wpa_supplicant udhcpc 2>/dev/null
  stop wifid
  stop wifis
  start wifid
fi
if online 40; then log "online with Amazon's wifid"; else log "offline even with wifid"; fi
