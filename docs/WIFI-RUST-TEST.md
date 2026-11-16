# Wi-Fi without wpa_supplicant: the device test

About 1 h 30 min, most of it waiting. Run each step through
`cargo xtask run STEP.sh DEADLINE` (a lifeline beside it), or over SSH with
a lifeline started by hand. Nothing changes until step 1 writes the switch;
step 9 removes it again.

SSH (read-only checks may run at any time):

```sh
K="ssh -i ~/.ssh/sharno-desktop -o IdentitiesOnly=yes -o IdentityAgent=none root@192.168.1.130"
P='export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin;'
```

Never run `iwlist scanning` or `iwconfig` while kindil's own Wi-Fi is on:
they go through the same driver, and a scan or a SET there changes the
driver's profile under the keeper.

## What changes when it is on

- **The switch:** `/var/local/kindil/wifi` holding `kindil`. Absent or
  anything else: wpa_supplicant, exactly as before. Read at each start of
  kindil's Wi-Fi stack (kindil starting, Wi-Fi switched on in Settings).
- **kindil's own Wi-Fi** (src/station.rs, src/wext.rs): no wpa_supplicant
  process. A keeper thread scans (SIOCSIWSCAN/SIOCGIWSCAN), picks the most
  preferred saved network in range, and joins with "essid off", the WPA2
  profile (SIOCSIWAUTH), the PMK (SIOCSIWENCODEEXT, alg 4) and the ESSID. The
  firmware does the 4-way handshake. udhcpc and the kindil-dhcp hook are the
  same programs as before, but a join counts only once a lease comes after
  it, within 15 s of associating, since a wrong password can associate for
  a few seconds but gets no DHCP through. For a new network (and a first
  join) kindil restarts udhcpc; the same network's lease is renewed by the
  udhcpc that holds it (SIGUSR1). The hook leaves a marker per udhcpc,
  `/var/run/kindil-lease.PID` (`bound|renew IP NANOS`), and only the
  running udhcpc's marker counts, with that IP on wlan0, while still on
  the same access point. Settings' scans are done by the keeper between
  joins, never during one. It checks the access point (SIOCGIWAP) every 3 s; on a loss
  it gives the firmware 10 s to rejoin by itself, then joins again (PMK
  again), with pauses doubling from 2 s to a minute between failed tries.
  After a suspend it joins the same network again straight away.
- **It refuses**, with the reason in the log: WPA1/TKIP-only, WPA2 with
  TKIP-only pairwise, WEP, WPA3-only and 802.1X networks. A saved network
  like that in range makes it fall back (next point); joining one in
  Settings fails with the reason.
- **The fallback:** no join with a lease within 30 s on a saved network
  in range (associating without an address counts as failing), a refused saved network in range, or 3 driver errors in
  a row: it logs `kindil's own Wi-Fi gives up (...)`, disassociates, turns
  the ESSID off and starts wpa_supplicant with the saved networks for the
  rest of this run (until kindil restarts). If its start fails, it is
  tried again with growing pauses until it comes up or Wi-Fi is switched
  off or on. Saved networks out of sight (out of range, or hidden) are
  tried unseen in turn, each first with a CCMP group and then TKIP (or as
  it was last seen); once every saved network has failed unseen twice
  (about 2 minutes away from all of them) it falls back too. Leaving it
  resets the driver's profile (ESSID off, WPA off) even if the
  disassociation fails. Forgetting the network it is on leaves it.
- **The launcher** is unchanged: udhcpc's pid file still tells it kindil
  ran Wi-Fi. When kindil exits, wifid starts Amazon's wpa_supplicant, which
  resets the driver's profile itself.

Log lines to look for, in `/var/local/kindil/kindil.log`:

```
kindil: Wi-Fi: kindil's own, without wpa_supplicant (/var/local/kindil/wifi)
kindil: Wi-Fi: joined NAME
kindil: Wi-Fi: lost the access point
kindil: Wi-Fi: NAME not joined; trying again in 2s
kindil: kindil's own Wi-Fi gives up (...); wpa_supplicant for the rest of this run
```

## Restarting kindil

kindil by pid (not `killall`: that also finds `kindil-dhcp` and a
lifeline), then KUAL's start script; TERM is "stopped on purpose", so the
launcher brings Amazon's UI back in between:

```sh
for p in $(pidof kindil); do
  [ "$(awk 'NR==1{print $2}' /proc/$p/status)" = kindil ] && kill -TERM $p
done
sleep 25
/mnt/us/extensions/kindil/bin/start.sh
```

## Steps

1. **Switch on.** `echo kindil > /var/local/kindil/wifi`, restart kindil.
   Expect within 30 s: the "kindil's own" log line, `joined NAME`, and
   - `pidof wpa_supplicant` empty (Amazon's included);
   - `cat /var/run/kindil-udhcpc.pid` alive;
   - `cat /var/run/kindil-lease.$(cat /var/run/kindil-udhcpc.pid)`: `bound`
     and the address `ifconfig wlan0` shows;
   - `cat /proc/net/wireless` status 0001 or so, level above 0;
   - `ifconfig wlan0` with the lease's address; `route -n` default via the
     router; `cat /etc/resolv.conf` the name servers;
   - `ping -c 3 1.1.1.1` and `wget -q -O /dev/null http://example.com`;
   - the Settings Wi-Fi row: the network's name, address and bars.
   Fail: no join within 30 s, then the "gives up" line and wpa_supplicant
   running. Note the reason in the log and go to step 9.
2. **Scan list.** Settings → Wi-Fi → the network list. Expect the networks
   around (as with wpa_supplicant), strongest first, the lock on protected
   ones, no hidden (empty) names. The link stays up during the scan (watch
   `/proc/net/wireless` and a ping in another shell). Scanning right after
   switching Wi-Fi on waits for the join to finish (up to about 30 s).
3. **Off and on.** Switch Wi-Fi off in Settings: `ifconfig wlan0` down, no
   udhcpc, the log has no errors; `/var/local/kindil/wifi-off` exists. On
   again: `joined NAME` within about 10 s, DHCP again, internet.
4. **Joining in Settings.** With a second network available (a phone's
   hotspot, WPA2): join it from the list with its password. Expect it to
   join within 30 s, with an address, and be saved. Then with a wrong
   password: expect "couldn't join ...: check the password" (with "joined,
   but got no address" if it associated) within about 45 s, nothing saved
   in `/var/local/kindil/networks.toml`, and the keeper back on the first
   network with a lease. Forget the hotspot
   afterwards; if it was the joined one, the keeper moves to the other
   network within about 5 s.
5. **Suspend and resume.** Set Sleep when idle to Doze, leave it untouched
   2 minutes, let it nap for 5 minutes (it wakes every 1 or 5 minutes to
   redraw; the 30-minute online wake is the one that matters, so better 35
   minutes; kindil's own Wi-Fi isn't parked between them yet). Each wake
   that reconnects logs a join; then press the power button. Expect `joined
   NAME` within a few seconds of the wake, the lease renewed (same address),
   internet. The lifeline can't help while it dozes: stay nearby.
6. **The access point dropping it.** Without the owner, options from
   least to most invasive:
   - out of range: carry the Kindle away until the bars go, then back
     within a minute. Expect `lost the access point`, then `joined NAME`
     soon after coming back. Away for longer (every saved network failing
     unseen twice, about 2 minutes), it falls back to wpa_supplicant for
     the run: expected;
   - the router's client list: block or kick the Kindle's MAC for a minute;
   - a phone hotspot as the only saved network, switched off and on.
   Expect no "gives up" line for a short absence.
7. **One hour hold.** Leave it on, Sleep when idle Never, plugged in, for
   an hour, pinging the router every minute from the Kindle
   (`while sleep 60; do ping -c1 -W5 ROUTER >/dev/null && echo ok || echo LOST; done`)
   and logging `/proc/net/wireless` (the crypt discard count should stay
   flat). Group keys are typically rekeyed every hour or so; the firmware
   should answer the group-key handshake itself. Expect no LOST and no
   `lost the access point`. If it drops at a rekey, the keeper rejoins (a
   few seconds without a link); note the time.
8. **The fallback.** With a saved network in range whose password is wrong
   (save the hotspot, then change its password on the phone): restart kindil
   with only that network in range. Expect `associated with ... but no
   address within 15 s` if it associates, then the "gives up (no join with
   an address on ... within 30 s)" line within about a minute of the start,
   then wpa_supplicant running (`pidof wpa_supplicant`) and the usual
   wpa_supplicant behaviour for the rest of the run.
9. **Switch off.** `rm /var/local/kindil/wifi`, restart kindil. Expect no
   "kindil's own" line and wpa_supplicant running, joined, as before the
   test.

## If it goes wrong

- No Wi-Fi at all: the lifeline restarts kindil and, still offline, hands
  Wi-Fi to Amazon's wifid. Then over SSH `rm /var/local/kindil/wifi` and
  restart kindil.
- Without SSH: any start where kindil's own Wi-Fi can't hold a saved
  network in range falls back to wpa_supplicant within about 40 s, so SSH
  over Wi-Fi should come back by itself. If not, hold the power button 20 s
  to reboot: Amazon's UI and its Wi-Fi ignore the switch (with start at
  boot on, kindil starts again with it; turn start at boot off first if
  you can).
