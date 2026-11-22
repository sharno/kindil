# How kindil works

The details behind the [README](../README.md): what each part does, how
updates and start at boot work, and the files kindil keeps on the Kindle.

## Overview

kindil runs **as an app on Amazon's stock OS**, started from KUAL. While it
runs, Amazon's UI steps aside; when it exits, Amazon's UI comes back. A crash
restarts it; a version that keeps crashing makes way for the previous one, and
then for Amazon's UI. It never flashes or replaces init or the bootloader.
Unless you opt into [start at boot](#start-at-boot), it changes no system file
and a reboot always returns the Kindle to stock.

Sleeping, dozing and the battery, the launcher's session and update trial,
and start at boot are also written as formal models (TLA+, in `verify/`),
checked in every order of events, with the app's recorded runs checked
against them: see [Formal models](DEVELOP.md#formal-models).

## What it does

- **Home**: the [Miqat](https://github.com/sharno/miqat) prayer dashboard,
  pixel-exact under the status row, refreshed on each minute; or a large
  clock. Tap the dashboard to reveal the tab bar; it hides again after 20 s.
- **Status row and quick panel**: every page has a slim status row (clock,
  Wi-Fi, battery; Home leaves out the clock, having a big one of its own, so
  a minute redraws none of the row). Tapping it drops a panel over the page with the front light
  slider (dim sun to bright sun, with the level) and, in the reader, text size
  smaller/larger. A tap outside, or 20 s, puts it away; using a control
  restarts the 20 s.
- **Library and reader**: `.epub` and UTF-8 `.txt` files from `/mnt/us/books`.
  Tap the right two-thirds of a page to go forward, the left third to go back.
  kindil remembers the position in every
  book, the text size, the home screen and the front light across restarts
  and reboots. Rename and Trash
  (with Undo) from a book's **···** menu; nothing is ever overwritten or
  deleted outright.
- **Settings**: an index of sections, each row showing what it is set to now,
  and each section a page that fits on screen (a page swap is one e-ink
  refresh; scrolling redraws on every move):
  - **Display**: home screen, front light, a "clean screen" flash.
  - **Prayer times**: the calculation method and the Asr school, each choice
    with today's times for the configured location so it can be matched to
    a mosque's timetable (the one page that scrolls: twelve methods). The
    choice is written into `miqat.toml`'s `[prayer]` in place, comments and
    all, and the dashboard redraws with it.
  - **Location**: search for a city (Open-Meteo's geocoder), use the
    approximate location of the internet address (ipinfo.io, only when
    tapped), or type coordinates and a time zone. Each asks before saving,
    then writes `miqat.toml`'s `[location]` and `kindil.toml`'s `timezone`
    in place (creating `kindil.toml` if needed): the clock follows at once,
    and the dashboard fetches weather and radar for the new place.
  - **Wi-Fi**: on/off with network status, scan and join, and the saved
    networks, each with Forget.
  - **Remote access**: SSH server and SSH-at-boot switches, Syncthing.
  - **Power**: sleep when idle, start at boot, Exit to Kindle, Restart and
    Power off.
  - **Updates** and **About** (battery, memory, storage, uptime, and the
    battery draw measured between doze's online windows).
- **Updates over the air** from this repo's GitHub releases; see
  [Updates](#updates).
- **Device duties** that Amazon's UI normally performs:
  - **Power button**: sleeps (a sleep screen, front light off, suspend to
    RAM) and wakes, with the light restored. kindil hears the button through
    kernel uevents; Amazon's powerd doesn't run while kindil does.
  - **Sleep when idle**: Doze (the default), Never, or a full sleep after
    5, 15, 30 or 60 minutes without a touch. Doze is for an always-on wall
    dashboard: after 2 minutes without a touch the light goes off and the
    Kindle suspends to RAM, waking on the RTC alarm each minute and napping
    again as soon as the panel has the frame. Most minutes are a tick: only
    the dashboard's top 300 rows (the clock, the next prayer and the
    countdown) are rendered and sent to the panel as a partial update, with
    nothing else read. The whole dashboard (timetable, weather, radar) and
    the battery gauge redraw once in each 5 minutes of the clock (on its
    multiples of 5, when the weather on screen moves on), when the day or
    the next prayer changes, after a fetch, and whenever the device is
    woken. Every 30 minutes it also rejoins Wi-Fi to fetch the weather;
    between those fetches wpa_supplicant stays off the network. A dozing
    Kindle is reachable over SSH for 2 minutes every 2 hours, and for as
    long as an SSH session stays open: every 2 hours (counted from the
    doze's start, whose 2 idle minutes were the first) the weather fetch's
    Wi-Fi stays up and the device stays awake for 2 minutes once the
    fetched frame is on the panel, redrawing each minute without
    suspending. While a connection to either SSH server (port 22 or 2222)
    is established in `/proc/net/tcp` or `tcp6`, checked every 5 seconds,
    it doesn't nap either, and naps once the last one closes; each session
    holds it at most 30 minutes, so a stuck one can't drain the battery.
    About once an hour, on the first online wake after an hour (at the
    latest after 90 minutes), it logs a doze ledger: awake and asleep seconds, wakes
    (ticks and full redraws apart, with the seconds each), Wi-Fi-up
    seconds, seconds held awake (maintenance windows, and SSH), how
    redraws ended, the gauge, and the clock's corrections. The power button
    brings it back with the light restored, and Wi-Fi rejoins.
  - **Time keeping**: with Amazon's services stopped nothing else sets the
    clock (Amazon's `updatetime` did). Each resume from a nap moves the
    clock on by the whole seconds the SoC's RTC, `rtc0`, counted (2.6.31's
    `rtc_resume`); since the alarm wakes at the same point of `rtc0`'s
    second every minute, the rounding is one-sided: the clock fell 29 s
    behind in an hour of naps on 2026-11-17, and ran 20 to 40 s an hour
    fast that night. So after each nap or sleep kindil puts the clock back
    within `rtc0`'s second, forward or back, where that second begins being
    measured (to 10 ms) after each SNTP sync. Whenever it goes online (at
    start once Wi-Fi has an address, each doze fetch once the renewed lease
    has set the route and name servers, each wake) it asks pool.ntp.org,
    time.google.com and time.cloudflare.com (SNTP, at most 4 s in all) and
    sets the clock when two agree it is off by a quarter second or more;
    with UDP 123 blocked, the `Date` of Open-Meteo's HTTP answer does (at
    most 5 s), to within 2 s. `rtc0` is written only when it is off by more
    than its resolution. A step of over a year is refused unless the clock
    is before 2026. Each step is logged ("clock was 724 s slow; set from
    time.google.com"), and every log line carries the wall time.
  - **Suspending**: every nap and sleep goes through one step that arms
    the PMIC's wake alarm (`rtc1`) with an absolute time last, reads it back
    from the alarm registers (`RTC_ALM_READ`; the driver never reports it
    through sysfs), and doesn't suspend if that fails: a refused nap waits
    awake for its minute and tries again. Writing `0` to `wakealarm` arms
    an alarm 5 minutes ahead on this driver rather than disarming it, so
    kindil moves the alarm a year ahead instead. Each step (Wi-Fi parked,
    the alarm armed and read back, suspending, resumed) is noted before it
    is taken in `/var/local/kindil/naps.log` (96 KiB, and the file before
    it as `naps.log.1`): after a freeze its last line says where the device
    stopped.
  - **The kernel's health**: before every suspend kindil also asks
    whether the kernel can take one, and reboots instead when it can't
    (src/health.rs). After an oops (`TAINT_DIE` in
    `/proc/sys/kernel/tainted`): on 2026-11-20 the Wi-Fi driver oopsed
    out of memory and the next nap's suspend never returned. And before
    the kernel runs out of memory: Lab126's Wi-Fi driver cuts the chip's
    power at each suspend and loads its four firmware files again at each
    resume, and the PW1's 2.6.31 firmware loader never frees those pages
    (fixed upstream in 2.6.35): 17 pages (68 KiB) a resume, about 4 MiB
    an hour of doze, measured on the device. Neither the driver's own
    "WLAN off" (the SDIO core still cycles the chip and the driver then
    loses it) nor unloading it (which can hang) avoids that, so once the
    kernel holds 144 MiB outside its lists (`/proc/zoneinfo`; about 27
    after a boot) kindil reboots, after about a day of doze. At most one
    such reboot in 6 hours: in between it stays awake rather than reboot
    again, and says so in the log. A reboot through init that hasn't
    happened a minute later is forced. With start at boot, kindil is back
    by itself; without it, Amazon's UI is.
  - **udev's workers**: each of those firmware requests comes to udev
    with the kernel's 60-second timeout, and the PW1's udev 146 starts a
    new worker for every such event, letting idle ones go only after 3 s
    awake with nothing queued, which a doze wake never gives it: 8 more a
    wake, 240 by each half-hourly fetch, 35 MiB. So while kindil runs, the
    loader's timeout (`/sys/class/firmware/timeout`) is 0, and udev hands
    the requests to the workers it has; kindil and the launcher put 60
    back for Amazon's UI.
  - **Wi-Fi of its own**: kindil runs Wi-Fi itself with the networks joined
    in it, saved in `/var/local/kindil/networks.toml` (plain text, readable
    by root only; a network is saved once it has joined). It loads the
    driver, runs its own wpa_supplicant (upstream 2.12, built by the flake)
    and DHCP as its children, starting one again if it exits; Off stops
    them, and is remembered. Before a nap or a sleep it leaves the network;
    a wake that needs it (a press, or a doze fetch) joins again in about a
    second. None of Amazon's Wi-Fi services run while kindil does; on a
    first run it starts with no networks, ready to join one.
  - **SSH of its own**: kindil carries kindil-sshd (`sshd/`, Rust, on
    russh) and runs it as its child on port 2222 for now, beside
    USBNetwork's on 22: keys only (ed25519, RSA), commands and shells. Its trusted keys are its own list in `/var/local/kindil/ssh/`,
    which USBNetwork's `usbnet/etc/authorized_keys` seeds on the first start
    only: keys added on the USB storage later (anyone with the Kindle on a
    cable can) show in Settings → Remote access with their fingerprints, to
    Trust or Ignore. Its host key is its own; nothing of it is on `/mnt/us`.
    It ends with kindil, and its sessions with it; its log is
    `/var/local/kindil/sshd.log`, kept under 64 KiB.
  - **USB drive mode**: plugging into a computer shows a USB screen while the
    computer has `/mnt/us`; unplugging reloads the library and resumes
    Syncthing. A cable that keeps dropping doesn't flash the screen: kindil
    returns only once the storage has stayed back for 3 s.
    Amazon's volumd hands the storage over, unless `/var/local/kindil/usb-drive`
    holds `kindil`: then kindil does it itself (unmount, a check with
    `fsck.vfat` when it comes back, read-only if that fails) and the launcher
    stops volumd while kindil runs. Off by default, still to be tried with a
    cable: see [docs/USB-DRIVE-TEST.md](USB-DRIVE-TEST.md).
  - **Low battery**: at 5% and not charging, kindil sleeps with a screen that
    asks for a charge. Suspended, the last few percent last for days; any
    sleep wakes every 6 hours to check (on a charger too, which may be
    unplugged meanwhile), and at 1% on battery powers off, as Amazon's
    powerd would, unless a charger turns up within 10 s. A press is told
    from that check by the press itself, not the time. Only the power button or a charger ends this
    sleep: a charger's plug-in wakes the device seconds before its driver
    says it is connected, so a wake waits up to 10 s for either and
    otherwise sleeps on with the screen untouched; with a charger already
    in, it doesn't sleep at all.
  - **Charging**: the charger charges by itself; kindil decides when it
    stops, through the charger driver's `charging` switch (`0` stops it, `1`
    sets the charge current again). Its driver (Amazon's
    `aplite_charger.c`) stops at full (100% and under 55 mA of charging
    current) and is meant to start again at 95%, but runs that rule only
    while its work is scheduled, on the battery driver's cached readings:
    each suspend cancels the work and only a charger plug event schedules
    it again, so a dozing Kindle left on a charger stopped at 100% and ran
    down on it (90% and discharging, plugged in all night, on 0.17.0).
    kindil now keeps the rule itself, at every reading of the gauge chip
    (each minute awake, each full redraw of a doze, every 5 minutes, and
    the 6-hourly check of a sleep; no wake of its own), in one policy
    (src/power.rs): outside 32-113 °F, powerd's range, it pauses charging,
    and resumes 3 °F back inside; this wins over the rest. On a charger it
    stops at 100% under 55 mA and restarts at 95% or below. The charger
    itself is a state machine in the PMIC (MC13892), which stops by itself
    at the end of a charge and at its 120-minute charge timer, and which
    the switch's `1` (a charge current) doesn't start again: only a replug,
    the PMIC's self-clearing CHGRESTART bit, or the cell sagging under 98%
    of the charge voltage do. Up to 0.20 that left the cell discharging on
    its charger from 95% down to about 92% (2026-11-21). So every way back
    to charging also sets CHGRESTART through `/dev/pmic` (src/pmic.rs), as
    the driver does on a plug-in, and kindil judges whether it charges by
    the PMIC's CHGENS bit rather than the gauge's sign. A charger stopped
    by itself at 95% or below gets `0`, `1` and a restart, at most once
    each 30 minutes until it charges. Each change is logged with the
    chip's percentage, current and temperature and the PMIC's charger
    ("kindil: charging stopped at full (100%, +41 mA, 75 °F; PMIC charger
    charging, current setting 5)", "charging restarted at 95%", "PMIC
    charger restarted"). Without `/dev/pmic` it falls back to the gauge's
    sign and the switch alone, and says so once. kindil never leaves the switch
    off: an unplug while it holds it puts it back to `1` (the driver forgets
    "full" on an unplug, but not a `0` written to the switch, which keeps
    its own rule off), and so does leaving kindil; the launcher writes `1`
    too as Amazon's UI comes back. A kindil that crashed holding it is
    followed by one that finds the switch off on a charger, and holds it as
    full until 95%. A charger replugged between two readings switches
    charging on again, which kindil sees, and its rule goes on from there.
    To test on the device: [CHARGING-TEST.md](CHARGING-TEST.md).
  - **Battery gauge**: the PW1's fuel gauge is a TI bq27000-family chip
    (bq27200's registers) on I2C, run by Lab126's `yoshi_battery` driver.
    The driver's files (`battery_capacity`, `battery_mAH`,
    `battery_current`, `battery_temperature`) only show what it last read,
    and it reads every 20 s while awake only: each suspend cancels its
    poll and each resume puts it 20 s off, so a dozing Kindle, awake about
    1.5 s a minute, would see values hours old, refreshed in its 2-minute
    maintenance windows. The chip keeps measuring while the Kindle is
    suspended, so every reading kindil takes (the percentage on screen, low
    battery, the 6-hourly check asleep, the charging temperature, the doze
    ledger's mAh and current) comes from the chip itself, through the
    driver's register window: kindil writes a register's number to
    `battreg` and reads `battreg_value`, which reads that register from the
    chip there and then (src/gauge.rs has the registers and their scaling,
    the driver's own). It only reads; should that fail, it says so in the
    log once and uses the driver's files until it works again.
    `kindil gauge [--every SECS]` prints the two side by side.
    While dozing, the chip's charge runs low by about 11 mAh an hour
    that the cell never gave: 2 s after each resume, the driver
    subtracts from the chip's charge (NAC) what it supposes the suspend
    drew, 765 µA for the suspend's length rounded *up* to whole hours,
    and writes it back (`battery_handle_update_nac`, "Battery: Resume -
    PostUpdate" in the kernel log). A wake back asleep within those 2 s
    (a minute tick) cancels it; each full redraw and online wake (about
    16 an hour) lets it take 4 counts, 0.71 mAh, as if it followed an
    hour's sleep. Measured on 2026-11-19/20: 56-64 of the 87-92 counts
    the chip lost each hour were these, so doze really draws about 5 mAh
    an hour, not 16; a 60-minute suspend alone moved the chip's charge by
    nothing at all (its current stayed under the chip's dead band: NOACT
    in FLAGS). So the percentage falls about three times too fast while
    dozing (12% at 3.94 V), until a full charge sets it right again. The
    doze ledger and "doze drew" lines count the chip's charge as it is,
    these subtractions included. kindil neither writes the gauge nor
    redraws less; it keeps its wakes short instead. A full redraw whose
    body (timetable, weather, radar) is the same as the window's renders
    only the header, as a tick does: Miqat keys the body by its SVG and
    the radar raster under it, and equal keys draw equal pixels (tested
    through the day). Most of the 5-minute full redraws are such. And the
    half-hourly clean flash comes with an online wake's fetched frame,
    which is long anyway. Each doze wake's line in naps.log says what it
    rendered and when its frame was shown and sent ("header, body kept
    rendered in 0.31 s, shown at 0.40 s, sent at 0.62 s").
  - **Crash restart**: the launcher restarts kindil after a crash. After 3
    crashes within 5 minutes it runs the previous version (kept by the last
    update), and after 3 more it returns to Amazon's UI.
- **E-ink care**: while a finger drags a list, the list follows it in fast
  black-and-white frames (the A2 waveform), each sent only once the panel has
  finished the last, so motion doesn't smear. Every 8 frames of a long drag
  the moved area gets a fast black-and-white clean, and on release a hard
  grayscale clean (a flash of that area only). In the
  reader, swipe left/right or tap to turn pages. Page changes flash the panel
  once; reading flashes every 6th page turn, the dashboard every half hour
  (on the first frame after a fetch once 20 updates have gone by, which
  dozing is an online wake's; on any full redraw after 40 without one; a
  minute tick never flashes), to clear ghosting.

## Updates

**Settings → Updates → Check** asks GitHub for the latest release; **Install**
downloads it. Nothing installs without a tap.

- **Signed:** CI signs every release binary with a minisign key. kindil refuses
  a binary that this key didn't sign, or that doesn't report the release's
  version when run.
- **Trial run:** the new binary goes next to the current one, and kindil
  restarts into it (the Kindle doesn't reboot). It is kept once it has run for
  two minutes.
- **Rollback:** if it exits or crashes before then, or the Kindle reboots
  mid-trial, the launcher runs the previous binary again.
- **Launcher:** once kept, the new version also installs the KUAL launcher
  files it was built with, and writes out the launcher it carries
  (`/var/local/kindil/launcher`, from `launcher/`), which runs kindil's
  session and the boot guard; they take effect at the next start. A version
  that doesn't stay up for two minutes never replaces the launcher.
- **Scope:** only kindil's own files change; the bootloader, kernel and system
  partition are never touched.

## Start at boot

Optional, and off until you turn it on. It needs one boot job,
`/etc/upstart/kindil.conf`: the only system file kindil ever adds. Install it
once over SSH:

```sh
sh /mnt/us/extensions/kindil/boot/install.sh install
```

Then pick a mode in **Settings → Power → Start at boot**, or in KUAL →
**kindil → Start at boot: on / standalone / off**. The job runs once per boot,
as soon as powerd, wifid and volumd start, and hands over to `bin/boot.sh` on
the user storage, whose launcher (`launcher boot`) starts kindil while
`/mnt/us/kindil/BOOT` exists.
With **on**, it waits for Amazon's UI to come up first, and Amazon's UI is
there whenever kindil exits (Exit to Kindle, or crashes it can't recover
from); it doesn't start kindil again until the next boot. A job installed
before 0.6.2 runs once Amazon's UI is up instead: install it again to boot
faster.

- **Standalone:** `BOOT` holding `standalone` also sets Amazon's own
  `/mnt/us/DONT_START_FRAMEWORK` flag. The next boots then skip X, Amazon's
  UI and its Java framework, since kindil draws to the screen itself, and
  stop Amazon's boot spinner. While kindil runs (from a boot or from KUAL),
  its launcher stops powerd, whose work kindil does, and the services kindil
  has no use for (connections, apps, archive, updates, sync and the like,
  and cron, whose crontab only rotates and gathers Amazon's logs);
  kindil runs Wi-Fi itself, so wifid doesn't run either. Of Amazon's system
  only volumd stays, unless kindil runs USB drive mode itself (off by default). kindil comes up about 20 s after
  the kernel, with more memory free. Every way back to Amazon's UI removes the flag first and
  starts those services and X if they aren't running:
  leaving kindil (Exit to Kindle, a crash it gives up on, a kill), and any
  boot where kindil doesn't stay up for 2 minutes. After a boot kindil stays
  up in, the flag is set again.
- **Failed boots:** if kindil doesn't stay up for 2 minutes in two boots in a
  row, the next boots go to Amazon's UI. Choosing a mode again resets that.
- **Off:** KUAL → **Start at boot: off**, or delete `/mnt/us/kindil/BOOT` and
  `/mnt/us/DONT_START_FRAMEWORK` over USB. The job stays and does nothing.
- **Undo:** `sh /mnt/us/extensions/kindil/boot/install.sh remove` removes the
  job.

## Files on the device

| Path | What |
|---|---|
| `/mnt/us/extensions/kindil/` | KUAL launcher and the `kindil` binary; during an update also `kindil.next`, `kindil.trying`, and afterwards `kindil.previous` |
| `/var/local/kindil/` | The running copy, the launcher and the log; off `/mnt/us`, which a computer takes in USB drive mode |
| `/var/local/kindil/networks.toml` | The Wi-Fi networks joined in kindil, with their passwords (root only) |
| `/var/local/kindil/dhcp` | `kindil` inside: kindil's own DHCP client instead of udhcpc (off by default; [DHCP-TEST.md](DHCP-TEST.md)) |
| `/var/local/kindil/usb-drive` | `kindil` inside: kindil runs USB drive mode instead of volumd (off by default; [docs/USB-DRIVE-TEST.md](USB-DRIVE-TEST.md)) |
| `/var/local/kindil/usb-exported` | Present while a computer may have the storage: the next start checks it |
| `/var/local/kindil/kindil-sshd`, `/var/local/kindil/ssh/` | kindil's SSH server, its host key, trusted and ignored keys (root only) |
| `/var/local/kindil/sshd.log` | kindil's SSH server's log (and `sshd.log.1`), under 64 KiB each |
| `/mnt/us/kindil/kindil.toml` | Optional settings (every key has a default) |
| `/mnt/us/kindil/miqat.toml` | Miqat's own settings: location, prayer method |
| `/mnt/us/kindil/prefs.toml` | Choices made on the device; safe to delete |
| `/mnt/us/kindil/BOOT` | Present: start kindil at boot; `standalone` inside: without Amazon's framework (see [Start at boot](#start-at-boot)) |
| `/mnt/us/DONT_START_FRAMEWORK` | Amazon's flag to skip its framework; kindil sets and clears it for a standalone boot |
| `/mnt/us/kindil/OVERLAYS` | Present: runtime overlays at boot (experimental; see below) |
| `/var/local/kindil/overlay-tries` | A boot with overlays not yet proven; after one, they are skipped |
| `/etc/upstart/kindil.conf` | Only if you installed start at boot |
| `/mnt/us/books/` | Books |
| `/mnt/us/books/.kindil-trash/` | Trashed books, recoverable |

Each time Amazon's UI starts (at boot, and whenever kindil hands the screen
back), Amazon's `/etc/upstart/framework.conf` runs `chgrp -R javausers
/var/local; chmod -R g=u /var/local` and setgids every directory there. That
makes kindil's binaries, its keys and passwords group-writable by
`javausers`. kindil narrows its own files under `/var/local/kindil/` again at
every start (`src/perms.rs`), and the launcher does the same for itself and
the copy it runs, before running it. While Amazon's UI runs, the window stays
open: the `javausers` group (Amazon's framework runs as `framework`) can
change kindil's files, including the launcher root runs next and the trusted
SSH keys, and `/var/local` itself is `framework`'s.

**Runtime overlays** (experimental, off by default;
[docs/OVERLAY-TEST.md](OVERLAY-TEST.md)): with
`/mnt/us/kindil/OVERLAYS` present and the boot job installed, each boot's
`launcher boot` lays a copy of `framework.conf` whose loop leaves
`/var/local/kindil` out over Amazon's with a read-only bind mount, before
Amazon's UI starts. The system partition stays as it is: a boot without the
file is stock. It applies only over the exact `framework.conf` it was made
from, and a boot that doesn't run 2 minutes with it turns it off
until `/var/local/kindil/overlay-tries` is deleted. `/var/local` stays
`framework`'s, so Amazon's framework could still move `/var/local/kindil`
aside.

## Syncthing

Optional and off by default. kindil does not download it: copy an ARM
(`linux-arm`) Syncthing build to the Kindle and add a `[syncthing]` section to
`kindil.toml`. The Settings switch then starts it with `nice 10`,
`GOMAXPROCS=1` and `GOGC=50`, logs to `<home>/kindil-syncthing.log`, and stops
it on exit.

Caveats before relying on it:

- Go officially requires Linux 3.2 or later. The PW1 runs 2.6.31, so current
  Syncthing builds may fail to start; the switch then shows the error.
- It costs roughly 40–80 MB of the PW1's 256 MB RAM and noticeable CPU while
  scanning, and it keeps Wi-Fi busy, which drains the battery.
- For a lighter way to fill the library, push books from a computer:
  `rsync -av ~/Books/ root@KINDLE_IP:/mnt/us/books/`.
