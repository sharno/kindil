#!/bin/sh
# Exercises scripts/device/install.sh, which installs a release on the Kindle
# and rolls back unless kindil comes back up, with stubbed Kindle commands.
# Run from the repo root: tests/install.sh
set -eu
INSTALL="$(cd "$(dirname "$0")/.." && pwd)/scripts/device/install.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stub" "$T/run"
# Whether kindil is running, its Wi-Fi supplicant runs and Wi-Fi is joined:
# files in $T; the fake start.sh creates them when $T/comes-up says.
printf '#!/bin/sh\n[ -f %s/kindil-up ] && echo 1234\n' "$T" >"$T/stub/pidof"
printf '#!/bin/sh\n[ -f %s/wifi-up ] && echo "%s/run/wpa_supplicant -i wlan0"\nexit 0\n' "$T" "$T" >"$T/stub/ps"
printf '#!/bin/sh\n[ -f %s/online ] && printf "wpa_state=COMPLETED\\nip_address=10.0.0.2\\n"\nexit 0\n' "$T" >"$T/stub/wpa_cli"
printf '#!/bin/sh\necho "killall $*" >>%s/calls\nrm -f %s/kindil-up %s/wifi-up %s/online\n' "$T" "$T" "$T" "$T" >"$T/stub/killall"
printf '#!/bin/sh\nshasum -a 256 "$@"\n' >"$T/stub/sha256sum"
chmod +x "$T"/stub/*
sed -e "s#^export PATH=.*#export PATH=$T/stub:/usr/bin:/bin#" \
  -e "s#^B=.*#B=$T/deploy#" \
  -e "s#^E=.*#E=$T/ext#" \
  -e "s#^RUN=.*#RUN=$T/run#" \
  -e "s#^SETTLE=.*#SETTLE=0#" \
  -e "s#^ONLINE_WAIT=.*#ONLINE_WAIT=1#" \
  -e "s#^HOLD=.*#HOLD=0#" \
  -e "s#sleep 5#sleep 1#" \
  "$INSTALL" >"$T/install.sh"

# kindil NAME VERSION: a fake binary that reports VERSION.
kindil() {
  printf '#!/bin/sh\necho "kindil %s"\n' "$2" >"$1"
  chmod +x "$1"
}
# The installed extension, at 1.0.0.
installed() {
  rm -rf "$T/ext" && mkdir -p "$T/ext/bin"
  kindil "$T/ext/bin/kindil" 1.0.0
  printf '# the old launcher\n' >"$T/ext/menu.json"
  # The installed start.sh: what a rollback starts.
  printf '#!/bin/sh\necho "start.sh $(%s/ext/bin/kindil)" >>%s/calls\n' "$T" "$T" >"$T/ext/bin/start.sh"
  chmod +x "$T/ext/bin/start.sh"
}
# A bundle of 2.0.0. Its start.sh, like the launcher, copies the binary it
# runs to $T/run/kindil (the previous version, if $T/falls-back says), and
# brings kindil and its Wi-Fi up if $T/comes-up says.
bundle() {
  rm -rf "$T/deploy" "$T/b" && mkdir -p "$T/deploy" "$T/b/extensions/kindil/bin"
  kindil "$T/b/extensions/kindil/bin/kindil" 2.0.0
  printf '#!/bin/sh\necho "start.sh $(%s/ext/bin/kindil)" >>%s/calls\nif [ -f %s/falls-back ]; then cp %s/ext/bin/kindil.previous %s/run/kindil; else cp %s/ext/bin/kindil %s/run/kindil; fi\n[ -f %s/comes-up ] && touch %s/kindil-up %s/wifi-up %s/online\nexit 0\n' \
    "$T" "$T" "$T" "$T" "$T" "$T" "$T" "$T" "$T" "$T" "$T" >"$T/b/extensions/kindil/bin/start.sh"
  printf '# the launcher\n' >"$T/b/extensions/kindil/menu.json"
  tar -C "$T/b" -czf "$T/deploy/bundle.tgz" extensions
  shasum -a 256 "$T/deploy/bundle.tgz" | cut -d' ' -f1 >"$T/deploy/bundle.sha256"
}
reset() {
  rm -rf "$T/run" "$T/calls" "$T/comes-up" "$T/falls-back" "$T/kindil-up" "$T/wifi-up" "$T/online"
  mkdir -p "$T/run"
  installed
  bundle
}
run() { sh "$T/install.sh" test >/dev/null 2>&1 || true; }
log() { cat "$T/run/deploy.log"; }
version() { "$T/ext/bin/$1" --version; }
expect() {
  [ "$2" = "$3" ] || { echo "FAIL $1: got '$2', want '$3'"; exit 1; }
  echo "ok   $1"
}

reset
echo junk >"$T/deploy/bundle.sha256"
run
expect "a bundle that fails its checksum installs nothing" \
  "$(version kindil) $(log | grep -c 'failed: checksum mismatch')" "kindil 1.0.0 1"

reset
touch "$T/comes-up"
run
expect "an install kindil comes back up from is kept" "$(version kindil) $(log | grep -c 'install test: done: kindil 2.0.0')" "kindil 2.0.0 1"
expect "it keeps the version it replaced for the launcher's crash fallback" "$(version kindil.previous)" "kindil 1.0.0"
expect "it installs every file in the bundle" "$(cat "$T/ext/menu.json")" "# the launcher"

reset
run
expect "an install kindil doesn't come back up from is rolled back" \
  "$(version kindil) $(log | grep -c 'install test: rolled back')" "kindil 1.0.0 1"
expect "with the launcher files it had" "$(cat "$T/ext/menu.json")" "# the old launcher"
expect "and kindil started again" "$(tail -n 1 "$T/calls")" "start.sh kindil 1.0.0"

reset
kindil "$T/deploy/kindil.rollback" 0.9.0
run
expect "a rollback binary sent along is the one rolled back to" "$(version kindil)" "kindil 0.9.0"

reset
touch "$T/comes-up" "$T/falls-back"
run
expect "the launcher falling back to the previous version isn't taken for the new one" \
  "$(version kindil) $(log | grep -c 'rolled back')" "kindil 1.0.0 1"

reset
touch "$T/run/wifi-off"
printf '#!/bin/sh\necho "start.sh" >>%s/calls\ncp %s/ext/bin/kindil %s/run/kindil\ntouch %s/kindil-up\n' "$T" "$T" "$T" "$T" >"$T/b/extensions/kindil/bin/start.sh"
tar -C "$T/b" -czf "$T/deploy/bundle.tgz" extensions
shasum -a 256 "$T/deploy/bundle.tgz" | cut -d' ' -f1 >"$T/deploy/bundle.sha256"
run
expect "with Wi-Fi switched off, kindil running is enough" "$(version kindil)" "kindil 2.0.0"
