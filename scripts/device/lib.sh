# shellcheck shell=bash
# Sourced by the scripts in scripts/device: reaching the Kindle over SSH.
#
#   KINDIL_DEVICE   the Kindle's SSH target, e.g. root@192.168.1.10 (required)
#   KINDIL_SSH_KEY  a private key to use, alone (optional)
#
# The PW1's busybox has no rsync and its dropbear wants scp's old protocol
# (-O); it greets every session with a banner, which these helpers drop.

: "${KINDIL_DEVICE:?set KINDIL_DEVICE to the Kindle\'s SSH target, e.g. root@192.168.1.10}"

# shellcheck disable=SC2034 # for the scripts that source this
DEVICE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=6)
if [[ -n "${KINDIL_SSH_KEY:-}" ]]; then
	SSH_OPTS+=(-i "$KINDIL_SSH_KEY" -o IdentitiesOnly=yes -o IdentityAgent=none)
fi

die() {
	echo "error: $*" >&2
	exit 1
}

# Runs a command on the Kindle, with the usual PATH (SSH sessions lack sbin).
k() {
	# shellcheck disable=SC2029 # the command is built here on purpose
	ssh "${SSH_OPTS[@]}" "$KINDIL_DEVICE" "export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin; $*" 2>/dev/null |
		sed -e '/^[[:space:]]*$/d' -e '/Welcome to Kindle/d'
}

# put FILE... REMOTE_DIR: copies files into a directory on the Kindle.
put() {
	local dir="${*: -1}"
	scp -q -O "${SSH_OPTS[@]}" "${@:1:$#-1}" "$KINDIL_DEVICE:$dir/"
}

reachable() {
	ssh "${SSH_OPTS[@]}" "$KINDIL_DEVICE" true >/dev/null 2>&1
}

# Waits up to HOURS (default 12) for the Kindle to answer: asleep, dozing
# or offline it doesn't, and it is not this script's place to wake it.
wait_reachable() {
	local end=$(($(date +%s) + ${1:-12} * 3600))
	until reachable; do
		[[ $(date +%s) -lt $end ]] || die "the Kindle didn't answer in ${1:-12} h"
		sleep 20
	done
}
