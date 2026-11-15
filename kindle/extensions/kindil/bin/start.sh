#!/bin/sh
# KUAL's "Start kindil": hands over to kindil's launcher (launcher/ in the
# repo), which starts kindil's session detached and returns, so KUAL can
# close its menu before Amazon's UI steps aside. The launcher sits on the
# system storage, written by a kindil that has run for two minutes; if none
# runs there yet, the installed kindil writes it. If it still can't run,
# nothing changes: Amazon's UI stays.
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
L=/var/local/kindil/launcher
mkdir -p /var/local/kindil
"$L" --version >/dev/null 2>&1 || "$(dirname "$0")/kindil" launcher >/dev/null 2>&1
exec "$L" start
