#!/bin/sh
# udhcpc's hook while kindil runs Wi-Fi itself (src/net.rs writes it to
# /var/local/kindil): the address, the default route and the name servers,
# which Amazon's wifid and connection manager set otherwise.
# udhcpc sets interface, ip, broadcast, subnet, router, dns and domain.
# shellcheck disable=SC2154
RESOLV=/var/run/resolv.conf
case "$1" in
deconfig)
  ifconfig "$interface" 0.0.0.0
  ;;
bound | renew)
  ifconfig "$interface" "$ip" ${broadcast:+broadcast "$broadcast"} ${subnet:+netmask "$subnet"}
  while route del default dev "$interface" 2>/dev/null; do :; done
  for gw in $router; do route add default gw "$gw" dev "$interface" && break; done
  {
    [ -n "$domain" ] && echo "domain $domain"
    for ns in $dns; do echo "nameserver $ns"; done
  } >"$RESOLV.new" && mv "$RESOLV.new" "$RESOLV"
  ;;
esac
exit 0
