#!/usr/bin/env bash
#
# Runs an experiment script on the Kindle with a lifeline beside it.
#
# Usage: scripts/device/run.sh SCRIPT DEADLINE_SECONDS [ARGS...]
#
# Copies SCRIPT and lifeline.sh to /var/local/kindil, starts SCRIPT detached
# with ARGS, and a lifeline that restores kindil and Wi-Fi when SCRIPT exits
# or at DEADLINE_SECONDS, whichever is first (see lifeline.sh). The script
# logs where it likes; this returns once both are started.
# See scripts/device/lib.sh for KINDIL_DEVICE and KINDIL_SSH_KEY.

set -euo pipefail
# shellcheck source=scripts/device/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

script="${1:-}" deadline="${2:-}"
[[ -f "$script" && "$deadline" =~ ^[0-9]+$ ]] ||
	die "usage: scripts/device/run.sh SCRIPT DEADLINE_SECONDS [ARGS...]"
shift 2
name="$(basename "$script")"
put "$script" "$DEVICE_DIR/lifeline.sh" /var/local/kindil
args="$(printf '%q ' "$@")"
k "cd /var/local/kindil && sh $name $args </dev/null >/dev/null 2>&1 & pid=\$!; setsid sh lifeline.sh $deadline \$pid </dev/null >/dev/null 2>&1 & echo \"started $name as \$pid, lifeline at $deadline s\""
