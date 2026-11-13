#!/bin/sh
# Exercises bin/boot.sh, the start-at-boot guard, with a fake boot id, stubbed
# sleep and a fake start.sh. Run from the repo root: tests/boot.sh
set -eu
BOOT="$(cd "$(dirname "$0")/.." && pwd)/kindle/extensions/kindil/bin/boot.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stub" "$T/run"
for c in sleep lipc-get-prop lipc-set-prop; do
  printf '#!/bin/sh\nexit 0\n' >"$T/stub/$c"
done
# Amazon's UI comes up while boot.sh waits for it.
printf '#!/bin/sh\necho lab126_gui >>%s/jobs\n' "$T" >"$T/stub/usleep"
# Upstart's stop and start, recorded in order, and the jobs running, one per
# line in $T/jobs.
printf '#!/bin/sh\necho "stop $1" >>%s/upstart\ngrep -vx "$1" %s/jobs >%s/jobs.new; mv %s/jobs.new %s/jobs\n' \
  "$T" "$T" "$T" "$T" "$T" >"$T/stub/stop"
printf '#!/bin/sh\necho "start $1" >>%s/upstart\necho "$1" >>%s/jobs\n' "$T" "$T" >"$T/stub/start"
printf '#!/bin/sh\nif grep -qx "$1" %s/jobs; then echo "$1 start/running"; else echo "$1 stop/waiting"; fi\n' \
  "$T" >"$T/stub/status"
chmod +x "$T"/stub/*
# The fake start.sh, like the real one, does nothing while its pid file names
# a live process. It counts its runs and leaves a pid file naming a process
# that is alive (up=1: this test's shell) or gone.
printf '#!/bin/sh\nkill -0 "$(cat %s/pid 2>/dev/null)" 2>/dev/null && exit 0\necho start >>%s/starts\nif [ "$(cat %s/up)" = 1 ]; then echo %s >%s/pid; else echo 999999 >%s/pid; fi\n' \
  "$T" "$T" "$T" "$$" "$T" "$T" >"$T/run/start.sh"
chmod +x "$T/run/start.sh"
sed -e "s#^FLAG=.*#FLAG=$T/BOOT#" \
  -e "s#^STATE=.*#STATE=$T/state#" \
  -e "s#^BOOT_ID=.*#BOOT_ID=$T/boot_id#" \
  -e "s#^PIDFILE=.*#PIDFILE=$T/pid#" \
  -e "s#^SKIP=.*#SKIP=$T/SKIP#" \
  -e "s#^export PATH=.*#export PATH=$T/stub:/usr/bin:/bin#" \
  "$BOOT" >"$T/run/boot.sh"
chmod +x "$T/run/boot.sh"

boot() { echo "$1" >"$T/boot_id"; "$T/run/boot.sh" >/dev/null 2>&1; }
starts() { if [ -f "$T/starts" ]; then wc -l <"$T/starts" | tr -d ' '; else echo 0; fi; }
tries() { cat "$T/state/boot-tries" 2>/dev/null || echo 0; }
# A boot as the job finds it: X starting, Amazon's UI still to come.
reset() {
  rm -rf "$T/state" "$T/starts" "$T/pid" "$T/BOOT" "$T/SKIP" "$T/upstart"
  echo 1 >"$T/up"
  echo x >"$T/jobs"
}
skip() { if [ -f "$T/SKIP" ]; then echo skip; else echo framework; fi; }
upstart() { cat "$T/upstart" 2>/dev/null | tr '\n' ','; }
expect() {
  [ "$2" = "$3" ] || { echo "FAIL $1: got '$2', want '$3'"; exit 1; }
  echo "ok   $1"
}

reset
boot a
expect "without the flag nothing starts" "$(starts)" "0"

reset
touch "$T/BOOT"
boot a
expect "with the flag kindil starts and settles" "$(starts) $(tries)" "1 0"
boot a
expect "Amazon's UI coming back in the same boot starts nothing" "$(starts)" "1"
boot b
expect "the next boot starts it again" "$(starts)" "2"

reset
touch "$T/BOOT"
echo 0 >"$T/up"
boot a
boot b
expect "boots where kindil didn't stay up are counted" "$(starts) $(tries)" "2 2"
echo 1 >"$T/up"
boot c
expect "after two of them the Kindle boots to Amazon's UI" "$(starts)" "2"

reset
touch "$T/BOOT"
# A pid from the last boot that some other process has now.
echo $$ >"$T/pid"
boot a
expect "a stale pid file from the last boot doesn't block the start" "$(starts)" "1"

reset
echo standalone >"$T/BOOT"
boot a
expect "standalone: a boot kindil stays up in skips the framework next time" "$(skip)" "skip"
reset
: >"$T/BOOT"
boot a
expect "after Amazon's UI: the framework keeps starting" "$(skip)" "framework"

reset
echo standalone >"$T/BOOT"
touch "$T/SKIP"
boot a
expect "standalone: X and Amazon's UI never start, nor does the spinner stay" "$(starts) $(upstart)" "1 stop x,stop early_progress,"

reset
: >"$T/BOOT"
boot a
expect "after Amazon's UI: kindil waits for it" "$(starts) $(upstart) $(grep -c lab126_gui "$T/jobs")" "1  1"

reset
echo standalone >"$T/BOOT"
touch "$T/SKIP"
echo 0 >"$T/up"
boot a
expect "a standalone boot kindil doesn't stay up in gets Amazon's full UI" "$(skip) $(upstart)" "framework stop x,stop early_progress,start x,"
boot a
expect "which starts nothing more in that boot" "$(starts) $(upstart)" "1 stop x,stop early_progress,start x,"

reset
echo standalone >"$T/BOOT"
echo 0 >"$T/up"
boot a
boot b
touch "$T/SKIP"
rm -f "$T/upstart"
echo x >"$T/jobs"
boot c
expect "past the failed-boot limit Amazon's UI comes with its framework" "$(starts) $(skip) $(upstart)" "2 framework "
printf 'x\nlab126_gui\n' >"$T/jobs"
touch "$T/SKIP"
boot d
expect "a job from before 0.6.2 restarts Amazon's UI, which is already up" "$(skip) $(upstart)" "framework stop lab126_gui,start lab126_gui,"
