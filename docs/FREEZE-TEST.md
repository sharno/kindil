# The doze freeze of 2026-11-17: what changed, and the device test

On 0.14.0, dozing on battery, the screen stopped at 10:18 PM and the Kindle
answered neither ping nor SSH until a hard restart the next morning. The
night's last line was an online wake's clock sync ("clock was 9.5 s fast;
set from the Date of api.open-meteo.com"); the steps after it (the fetch,
the settle, parking Wi-Fi, arming the alarm, suspending) log nothing when
they succeed, so the log can't say which of them, or the kernel, stopped.
This branch makes each of them leave a line, and fixes what the
investigation found wrong on the way.

```sh
K="ssh -i ~/.ssh/sharno-desktop -o IdentitiesOnly=yes -o IdentityAgent=none root@192.168.1.130"
P='export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin;'
```

## 1. Each nap is recorded, and its alarm read back

After deploying, let it doze ten minutes, then in a maintenance window:

```sh
$K "$P tail -12 /var/local/kindil/naps.log"
```

Expect, per nap: `wake N: Draw Tick for … , Done after 1.2 s, panel
frame F`, `Wi-Fi parked in 0.0x s`, `nap: suspending, rtc1 alarm at
N (rtc M +K s, read back N)`, `nap: resumed after K s by the clock, rtc1 at
N...`. A `not read back` means `/dev/rtc1` couldn't be opened (it is
exclusive): note who holds it (`fuser /dev/rtc1`). A `not suspending` line
is a refused nap: kindil.log says why, and the dashboard keeps ticking
awake.

## 2. The clock stays within a second

Over a night, kindil.log's network syncs should say at most `clock was
0.x s fast/slow`, and the hourly ledger's `clock corrected N times by the
RTC` shows both signs. Check against the network directly:

```sh
$K "$P date -u +%s.%N; cat /sys/class/rtc/rtc0/since_epoch"; date -u +%s.%N
```

## 3. Online wakes find the network

Each online wake logs `online: Wi-Fi back in X s, clock Y s more` and
`online wake ended after Z s`. `Network unreachable` and DNS `Try again`
from the clock should be gone; a `renewed lease wasn't applied within 8s`
line says udhcpc's renewal was slow.

## 4. If it freezes again

Before anything else, ask whether the power button woke it: a suspend
whose alarm didn't fire still wakes on the button; a kernel hung in
suspend or resume doesn't. Then, after the restart:

```sh
$K "$P tail -20 /var/local/kindil/naps.log; tail -40 /var/local/kindil/kindil.log"
$K "$P dmesg | grep -e savemsg -e 'def:rst' -e 'def:bcut' -e '^#'"
```

- Last line `nap: suspending, ... read back N`: the kernel took the suspend
  with a verified alarm and never came back from it; with the button dead
  too, a hang in a driver's suspend or resume.
- Last line `Wi-Fi parked` (or an online wake's line in kindil.log with
  nothing in naps.log after it): kindil stopped before suspending.
- Last line `resumed`: the device came back and kindil stopped after.
- `wake N: Draw Tick for 22:47:01…, Done` lines going on while the photo
  of the screen shows an earlier minute: the panel said it drew frames
  that never reached the glass.
- Amazon's syslog keeps the kernel's suspend and resume lines, but only
  until `tinyrot` rotates `/var/log/messages` (a tmpfs, lost on reboot);
  the online wakes show in it as `channel hint set to …`.
- The dmesg lines: Amazon's kernel prints the previous boot's last kernel
  messages (`# ...` between `Kernel Reboot Message Start/End`) after a
  watchdog or hard reset when its saved buffer survived, and the reset's
  kind (`reset=hard`, `reset=watchdog`).
