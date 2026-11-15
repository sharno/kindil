#!/usr/bin/env bash
#
# Waits for the Kindle to answer over SSH, then runs a command on it.
#
# Usage: scripts/device/wait.sh [--hours N] [COMMAND...]
#
# Asleep, dozing or offline, the Kindle doesn't answer; this asks every 20 s
# for up to N hours (default 12). With no COMMAND it prints kindil's version,
# its recent log and its Wi-Fi state.
# See scripts/device/lib.sh for KINDIL_DEVICE and KINDIL_SSH_KEY.

set -euo pipefail
# shellcheck source=scripts/device/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

hours=12
if [[ "${1:-}" == --hours ]]; then
	hours="${2:?--hours needs a number}"
	shift 2
fi
wait_reachable "$hours"
echo "the Kindle answered at $(date)"
if [[ $# -gt 0 ]]; then
	k "$*"
else
	k '/var/local/kindil/kindil --version
	   grep -v "weather:\|radar:\|start/running\|stop/waiting\|blanket" /var/local/kindil/kindil.log | tail -n 20
	   wpa_cli -i wlan0 status 2>/dev/null | grep "^ssid\|wpa_state\|ip_address"'
fi
