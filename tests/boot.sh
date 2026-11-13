#!/bin/sh
# Exercises bin/boot.sh, the start-at-boot guard, with a fake boot id, stubbed
# sleep and a fake start.sh. Run from the repo root: tests/boot.sh
set -eu
BOOT="$(cd "$(dirname "$0")/.." && pwd)/kindle/extensions/kindil/bin/boot.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stub" "$T/run"
printf '#!/bin/sh\nexit 0\n' >"$T/stub/sleep"
# Upstart's stop and start, recorded in order.
for c in stop start; do
  printf '#!/bin/sh\necho "%s $*" >>%s/upstart\n' "$c" "$T" >"$T/stub/$c"
done
chmod +x "$T/stub/sleep" "$T/stub/stop" "$T/stub/start"
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
reset() { rm -rf "$T/state" "$T/starts" "$T/pid" "$T/BOOT" "$T/SKIP" "$T/upstart"; echo 1 >"$T/up"; }
skip() { if [ -f "$T/SKIP" ]; then echo skip; else echo framework; fi; }
upstart() { tr '\n' ',' <"$T/upstart" 2>/dev/null || true; }
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
echo 0 >"$T/up"
boot a
expect "a standalone boot kindil doesn't stay up in gets Amazon's full UI" "$(skip) $(upstart)" "framework stop lab126_gui,start lab126_gui,"
boot a
expect "which starts nothing more in that boot" "$(starts) $(upstart)" "1 stop lab126_gui,start lab126_gui,"

reset
echo standalone >"$T/BOOT"
echo 0 >"$T/up"
boot a
boot b
touch "$T/SKIP"
rm -f "$T/upstart"
boot c
expect "past the failed-boot limit the framework comes back" "$(starts) $(skip) $(upstart)" "2 framework stop lab126_gui,start lab126_gui,"
