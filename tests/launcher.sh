#!/bin/sh
# Exercises bin/session.sh's update trial and rollback with fake binaries and
# stubbed Kindle commands. Run from the repo root: tests/launcher.sh
set -eu
SESSION="$(cd "$(dirname "$0")/.." && pwd)/kindle/extensions/kindil/bin/session.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/stub" "$T/ext" "$T/run"
for c in lipc-set-prop stop start eips sleep; do
  printf '#!/bin/sh\nexit 0\n' >"$T/stub/$c"
  chmod +x "$T/stub/$c"
done
sed -e "s#^EXT=.*#EXT=$T/ext#" \
  -e "s#^export PATH=.*#export PATH=$T/stub:/usr/bin:/bin#" \
  "$SESSION" >"$T/run/session.sh"
chmod +x "$T/run/session.sh"

# fake NAME VERSION BODY: a binary that reports VERSION, then runs BODY.
fake() {
  printf '#!/bin/sh\n[ "$1" = --version ] && { echo "kindil %s"; exit 0; }\n%s\n' "$2" "$3" >"$1"
  chmod +x "$1"
}
# What kindil does once a trial has run for two minutes (src/update.rs).
COMMIT="mv $T/ext/kindil $T/ext/kindil.previous; mv $T/ext/kindil.trying $T/ext/kindil; rm $T/run/trying"
reset() { rm -rf "$T/ext" "$T/run/kindil" "$T/run/trying" "$T/staged" && mkdir "$T/ext"; }
run() { "$T/run/session.sh" >/dev/null 2>&1; }
files() { (cd "$T/ext" && ls | tr '\n' ' '); }
expect() {
  [ "$2" = "$3" ] || { echo "FAIL $1: got '$2', want '$3'"; exit 1; }
  echo "ok   $1"
}

reset
fake "$T/ext/kindil" 1.0.0 "exit 0"
fake "$T/ext/kindil.next" 2.0.0 "exit 139"
run
expect "a crashing update rolls back" "$(files)$("$T/run/kindil" --version)" "kindil kindil 1.0.0"

reset
fake "$T/ext/kindil" 1.0.0 "exit 0"
fake "$T/ext/kindil.next" 2.0.0 "$COMMIT; exit 0"
run
expect "a settled update is kept" "$(files)$("$T/ext/kindil" --version)" "kindil kindil.previous kindil 2.0.0"

reset
fake "$T/new" 2.0.0 "$COMMIT; exit 0"
fake "$T/ext/kindil" 1.0.0 "[ -f $T/staged ] && exit 0; touch $T/staged; cp $T/new $T/ext/kindil.next; exit 75"
run
expect "exit 75 restarts into the staged update" "$("$T/ext/kindil" --version)" "kindil 2.0.0"

reset
fake "$T/ext/kindil" 1.0.0 "exit 0"
fake "$T/ext/kindil.trying" 2.0.0 "exit 0"
touch "$T/run/trying"
run
expect "a trial cut short by a reboot is dropped" "$(files)$("$T/run/kindil" --version)" "kindil kindil 1.0.0"

reset
fake "$T/ext/kindil" 1.0.0 "echo ran >>$T/runs; exit 1"
rm -f "$T/runs"
run
expect "a crash without a trial returns to Amazon's UI" "$(wc -l <"$T/runs" | tr -d ' ')" "1"
