#!/usr/bin/env bash
# kindil's own DHCP client (src/dhcpc.rs) against a real server: dnsmasq,
# in throwaway network namespaces of an unprivileged user namespace, so no
# root is needed. Linux with unprivileged user namespaces, dnsmasq on PATH:
#
#   nix shell nixpkgs#dnsmasq -c tests/dhcp-netns.sh
#
# The client's side gets veth c0; the server's side s0, 192.0.2.1/24, in
# a namespace of its own (in one namespace, replies to the client's new
# address would go through lo rather than c0).
set -euo pipefail
cd "$(dirname "$0")/.."
command -v dnsmasq >/dev/null || {
	echo "dnsmasq isn't on PATH" >&2
	exit 1
}
bin=$(cargo test --lib --no-run 2>&1 | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p')
[ -x "$bin" ] || {
	echo "no test binary" >&2
	exit 1
}

exec unshare --user --map-root-user --net bash -s -- "$bin" <<'INNER'
set -euo pipefail
bin=$1
tmp=$(mktemp -d)
unshare --net sleep infinity &
server=$!
dnsmasq=
trap 'kill $server $dnsmasq 2>/dev/null || true; rm -rf "$tmp"' EXIT
sleep 0.2
on_server() { nsenter --target "$server" --net -- "$@"; }
ip link set lo up
ip link add c0 type veth peer name s0
ip link set s0 netns "$server"
ip link set c0 up
on_server ip link set lo up
on_server ip addr add 192.0.2.1/24 dev s0
on_server ip link set s0 up
# --no-daemon: it keeps its user and groups, which a user namespace
# without setgroups(2) wouldn't let it change.
# Not through on_server: $! has to be dnsmasq's own pid, for the trap.
nsenter --target "$server" --net -- dnsmasq --no-daemon --conf-file=/dev/null --port=0 \
	--interface=s0 --bind-interfaces \
	--dhcp-range=192.0.2.50,192.0.2.60,255.255.255.0,2m \
	--dhcp-option=option:router,192.0.2.1 \
	--dhcp-option=option:dns-server,192.0.2.1 --domain=example.test \
	--dhcp-leasefile="$tmp/leases" --pid-file="$tmp/pid" \
	--log-dhcp &
dnsmasq=$!
sleep 0.5
KINDIL_DHCP_TEST_INTERFACE=c0 KINDIL_DHCP_TEST_LEASES="$tmp/leases" \
	"$bin" --ignored --exact dhcpc::tests::against_a_real_server --nocapture
INNER
