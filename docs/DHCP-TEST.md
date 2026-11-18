# kindil's own DHCP client: the device test

About 2 h 30 min, most of it dozing. Nothing changes until step 1 writes the
switch; step 8 removes it again. Run the steps over SSH with a lifeline
beside them, or through `cargo xtask run STEP.sh DEADLINE`; restart kindil
as in [WIFI-RUST-TEST.md](WIFI-RUST-TEST.md#restarting-kindil).

```sh
K="ssh -i ~/.ssh/sharno-desktop -o IdentitiesOnly=yes -o IdentityAgent=none root@192.168.1.130"
P='export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin;'
```

## What changes when it is on

- **The switch:** `/var/local/kindil/dhcp` holding `kindil`. Absent or
  anything else: udhcpc and the kindil-dhcp hook, exactly as before. Read
  at each start of kindil's Wi-Fi stack, with either Wi-Fi (wpa_supplicant,
  or kindil's own behind `/var/local/kindil/wifi`).
- **No udhcpc:** a thread of kindil's (src/dhcpc.rs) does DHCP. It sends
  from a UDP socket on port 68 bound to wlan0, and until the address is set
  it reads the replies off a packet socket, as udhcpc does. It sets the
  address, netmask and broadcast address, the default route and
  `/var/run/resolv.conf` itself, by ioctl rather than ifconfig and route.
  No `/var/run/kindil-udhcpc.pid`, no `/var/run/kindil-lease.*` markers.
- **Leases across naps:** a join on the network (SSID and access point) it
  holds a lease on keeps the lease without a packet until T1 (half the
  lease, usually); after T1 it confirms the address (INIT-REBOOT); on any
  other network it asks afresh. While awake it renews at T1 and rebinds at
  T2 as udhcpc did. Lease times count the time asleep (from the RTC).
- **RELEASE** when Wi-Fi is switched off in Settings, and when Settings
  joins another network (sent before leaving the old one).
- **If it can't start** (port 68 taken, say), udhcpc runs for that start:
  `kindil: DHCP: kindil's own client: ...; udhcpc instead`.
- **The launcher** is unchanged: with no udhcpc pid file it has one child
  fewer to end when kindil exits (the client dies with kindil, leaving the
  address), and it starts wifid as always.
- **The doze ledger** ends with `DHCP N exchanges (X an hour, M messages)`.

Log lines, in `/var/local/kindil/kindil.log`:

```
kindil: DHCP: kindil's own client (/var/local/kindil/dhcp)
kindil: DHCP: leased A.B.C.D from SERVER for 86400 s (renewing after 43200 s)
kindil: DHCP: confirmed A.B.C.D ...      (INIT-REBOOT after T1)
kindil: DHCP: renewed A.B.C.D ...        (at T1, awake)
kindil: DHCP: lease lost: expired | refused (NAK)
kindil: DHCP: lease given back
```

## Steps

1. **Switch on.** `echo kindil > /var/local/kindil/dhcp`, restart kindil.
   Expect within 15 s the two first log lines above, and:
   - `pidof udhcpc` empty, no `/var/run/kindil-udhcpc.pid`;
   - `ifconfig wlan0`: the leased address, its netmask and broadcast;
   - `route -n`: one default route, through wlan0, via the router;
   - `cat /etc/resolv.conf`: the domain (if the router gives one) and the
     name servers;
   - `ping -c 3 1.1.1.1`, `wget -q -O /dev/null http://example.com`, and
     the Settings Wi-Fi row with the address.
   Note the lease time in the `leased` line: it decides step 4.
2. **Both Wi-Fis.** Step 1 runs with whichever Wi-Fi is switched on; run
   it once more with the other (`echo kindil > /var/local/kindil/wifi`, or
   `rm` it), restarting kindil. With kindil's own Wi-Fi, `joined NAME`
   comes after the `leased` line: the join counts only once the lease is on
   wlan0.
3. **Off and on.** Wi-Fi off in Settings: `lease given back`, `ifconfig
   wlan0` without an address, the router's client list without the Kindle
   (if it shows one). On again: `leased` within about 10 s, most likely the
   same address (it asks for it).
4. **Naps and fetches keep the lease.** Sleep when idle: Doze; leave it
   about 70 minutes (two 30-minute fetches, a maintenance window at most).
   Then over SSH (in a maintenance window, or after a press):
   - `grep 'DHCP' /var/local/kindil/kindil.log | tail`: no `leased` or
     `confirmed` line at the fetches while T1 hadn't passed (a `confirmed`
     line after T1 is right: one exchange);
   - the doze ledger lines: `DHCP 0 exchanges` an hour with a lease of a
     day; with a lease of L hours, at most about 2 / L an hour;
   - the same address as in step 1, and the dashboard's fetches succeeding.
   kindil's own Wi-Fi rejoins at each fetch and proves it by the kept
   lease: `joined NAME` at each fetch, and no `no address within 15 s`.
5. **Renewal.** Sleep when idle: Never, plugged in, past T1 (with a router
   giving a day, skip this, or use a phone hotspot, whose leases are often
   an hour). Expect `renewed A.B.C.D` at T1, same address, no break in a
   `ping` running meanwhile.
6. **Another network.** Join a phone's hotspot in Settings: `lease given
   back` (sent on the old network), then `leased` with the hotspot's
   address, its default route and name servers. Forget it: back on the
   first network with a fresh `leased` (a new access point, so no
   INIT-REBOOT).
7. **Restart with the switch.** Restart kindil once more: the first
   DISCOVER asks for the address wlan0 still has, so the router usually
   gives it back (`leased` with the same address).
8. **Switch off.** `rm /var/local/kindil/dhcp`, restart kindil: udhcpc
   running, `/var/run/kindil-udhcpc.pid`, and its marker as before; no
   `kindil: DHCP: kindil's own client` line.

## If it goes wrong

- No address: the lifeline restarts kindil, which starts the client again
  (the switch stays), then hands Wi-Fi to wifid; Amazon's Wi-Fi ignores
  the switch. Over SSH (USBNetwork on the cable if Wi-Fi is down):
  `rm /var/local/kindil/dhcp` and restart kindil.
- Worth noting for a failure: `cat /proc/sys/net/ipv4/conf/*/rp_filter`,
  `ifconfig wlan0`, `route -n`, the `kindil: DHCP:` lines, and whether
  `kindil: DHCP: the packet socket: ...` appears (the packet socket and its
  BPF filter are the part least like what ran before; udhcpc uses the same
  kind of socket on this kernel).
