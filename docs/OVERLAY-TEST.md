# Runtime overlays: the device test

About 30 min plus three reboots. Nothing here writes the system partition:
the overlay's copy lives on tmpfs (`/var/kindil-overlays`, root only), the
bind mount ends with the boot or with `umount`, and the switch and counter
are files on the user and kindil storage.

SSH (read-only checks may run at any time):

```sh
K="ssh -i ~/.ssh/sharno-desktop -o IdentitiesOnly=yes -o IdentityAgent=none root@192.168.1.130"
P='export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin;'
```

## What it does

- **Switch:** `/mnt/us/kindil/OVERLAYS` (or `/mnt/base-us/kindil/OVERLAYS`,
  the same storage under `/mnt/us`'s FUSE layer, should `/mnt/us` not be up
  yet). Read by `bin/boot.sh` and `launcher boot` on every boot, whatever
  start at boot says. The boot job (`/etc/upstart/kindil.conf`) must be
  installed: it is what runs them.
- **First thing in `launcher boot`**, before the start-at-boot guard: for
  each overlay whose target is byte for byte the file it was made from
  (`launcher/overlays/etc/upstart/framework.conf.orig`, md5
  `cf6613a4e8eea764c2d7f4c4153ca319`), it counts the boot in
  `/var/local/kindil/overlay-tries`, writes the replacement to
  `/var/kindil-overlays/etc_upstart_framework.conf` (the original's modes),
  binds it over `/etc/upstart/framework.conf` (mount(2), MS_BIND), remounts
  the bind read-only (MS_REMOUNT|MS_BIND|MS_RDONLY) and runs
  `initctl reload-configuration`. A target that differs (other firmware) or
  is missing is skipped and logged; a bind that fails undoes the ones
  before it.
- **Proof:** 2 minutes after applying, if upstart still knows the
  `framework` job, the counter file is deleted (`overlays proven` in the
  log). If not, the overlays are unbound, upstart reloads, and the boot
  stays counted. After one unproven boot the overlays are skipped (`the last boot with overlays
  wasn't proven; skipping them`) until the counter file is deleted.
- **The replacement** differs from Amazon's only in the permissions loop
  in `script`: `/var/local/kindil` is left out of `chgrp -R javausers`,
  `chmod -R g=u` and the setgid on directories; everything else gets
  what it got before (top-level symlinks were never chmodded by
  `chmod -R` either).

Log lines are in `/var/local/kindil/kindil.log`, prefixed `boot: overlays:`.

## Why the reload

Upstart here is 0.6.6 (`initctl version`: `init.exe (upstart 0.6.6)`). It
watches `/etc/upstart` with inotify (`nih_watch_new` in `/sbin/init.exe`),
and a bind mount makes no inotify event. `initctl reload-configuration`
walks the directory again and re-reads every file (upstart 0.6.6
`conf_source_reload_dir`, `conf_reload_path`), so it reads the bound file.
A job with a running instance keeps its old definition until that instance
stops (`job_class_remove` refuses; the old class is marked deleted); the
next start uses the new one. A file that no longer parses drops the job:
hence the `status framework` check in the proof. Unbinding needs a reload
too.

At boot the bind should come well before `framework` starts: kindil's job
runs at `started volumd`, `framework` at `contentpack_font_ready`, after X
and `lab126_gui`. Step 3's probe confirms it.

## 1. By hand, without a reboot

With Amazon's UI up and kindil not running. Baseline:

```sh
$K "$P md5sum /etc/upstart/framework.conf; grep framework /proc/mounts; ls -ld /var/kindil-overlays"
```

Expect `cf6613a4…`, no mount, no directory. Then the probes: a root-only
file inside kindil's directory and one beside it.

```sh
$K "$P mkdir -m 700 /var/local/kindil/probe-dir; for f in /var/local/kindil/probe /var/local/kindil/probe-dir/f /var/local/overlay-probe; do echo x >\$f; chgrp root \$f; chmod 600 \$f; done; ls -l /var/local/kindil/probe /var/local/overlay-probe; ls -ld /var/local/kindil/probe-dir"
```

Lay the overlay (the copy on tmpfs, a plain bind: the device's `mount` is
util-linux 2.12r, so the read-only remount is left to the launcher in step
3):

```sh
$K "$P mkdir -m 700 /var/kindil-overlays && cat >/var/kindil-overlays/etc_upstart_framework.conf && chmod 644 /var/kindil-overlays/etc_upstart_framework.conf" <launcher/overlays/etc/upstart/framework.conf
$K "$P mount -n --bind /var/kindil-overlays/etc_upstart_framework.conf /etc/upstart/framework.conf && initctl reload-configuration && md5sum /etc/upstart/framework.conf && grep -n KEEP /etc/upstart/framework.conf && grep framework /proc/mounts && status framework"
```

Expect the overlay's md5 (`md5 launcher/overlays/etc/upstart/framework.conf`
on the computer), the `KEEP` lines, a `tmpfs /etc/upstart/framework.conf`
mount and `framework start/running` (or `stop/waiting`), not `Unknown job`.

Upstart can't print a job's script, so the proof is what the next framework
start does. Restart Amazon's UI as kindil's session does:

```sh
$K "$P stop lab126_gui; start lab126_gui"
```

Wait until the home screen is back (about a minute), then:

```sh
$K "$P ls -l /var/local/kindil/probe /var/local/kindil/probe-dir/f /var/local/overlay-probe; ls -ld /var/local/kindil/probe-dir /var/local/kindil"
```

Pass: both kindil probes still `-rw------- root root` and `probe-dir`
`drwx------ root`; the control `/var/local/overlay-probe` became
`-rw-rw---- … javauser` (Amazon's loop ran). If the control didn't change,
the framework didn't start again: wait longer or check `status framework`.

Undo:

```sh
$K "$P umount /etc/upstart/framework.conf && initctl reload-configuration && rm -rf /var/kindil-overlays && md5sum /etc/upstart/framework.conf && status framework"
```

Expect `cf6613a4…` and the job still known. Reset the probes (they are used
again in step 3):

```sh
$K "$P for f in /var/local/kindil/probe /var/local/kindil/probe-dir/f /var/local/overlay-probe; do chgrp root \$f; chmod 600 \$f; done; chmod 700 /var/local/kindil/probe-dir"
```

## 2. Install the build

`cargo xtask deploy` copies kindil and the extension (with the new
`bin/boot.sh`). The launcher at `/var/local/kindil/launcher` is replaced
only once kindil has run for 2 minutes: start kindil from KUAL, wait, then
check that it is this build's:

```sh
$K "$P strings /var/local/kindil/launcher | grep -c 'overlays proven'; grep -c OVERLAYS /mnt/us/extensions/kindil/bin/boot.sh"
```

Both must print at least 1. Exit to Kindle afterwards.

## 3. The reboot test

Start at boot **on** or **off**, not standalone: a standalone boot never
starts the framework, so the probes would pass anyway. With the probes
reset as at the end of step 1:

```sh
$K "$P touch /mnt/us/kindil/OVERLAYS; rm -f /var/local/kindil/overlay-tries"
```

Reboot. Once the home screen is up (and again after 2 minutes):

```sh
$K "$P grep 'overlays:' /var/local/kindil/kindil.log | tail -5; grep framework /proc/mounts; cat /var/local/kindil/overlay-tries; ls -ld /var/kindil-overlays; md5sum /etc/upstart/framework.conf; status framework; ls -l /var/local/kindil/probe /var/local/kindil/probe-dir/f /var/local/overlay-probe"
```

Pass:

- `boot: overlays: /etc/upstart/framework.conf: overlaid` early in the
  boot, then `boot: overlays: overlays proven` 2 minutes later, and no
  `overlay-tries` after that (`1` before).
- The `/proc/mounts` line ends in `ro,…`: the read-only remount worked on
  this kernel (2.6.31).
- `/var/kindil-overlays` is `drwx------ root`.
- The kindil probes stayed `root`/`600`, the control went to `javauser`/
  `660`: the bind came before the framework started. If the kindil probes
  widened too, the framework started before the bind (compare the
  `overlaid` line's time with when the home screen appeared), and the
  order needs a different approach.
- Exiting kindil to Amazon's UI later (with start at boot on) leaves the
  probes alone too.

## 4. The counter

```sh
$K "$P echo 2 >/var/local/kindil/overlay-tries"
```

Reboot. Expect `boot: overlays: 2 boots with overlays weren't proven;
skipping them …` in the log, no framework mount, md5 `cf6613a4…`, and the
counter still `2`. Then `rm /var/local/kindil/overlay-tries`.

Optional, the counting itself: reboot within 2 minutes of the home screen
coming up, twice in a row; the counter reads `1`, then `2`, and the third
boot skips the overlays.

## 5. Undo over USB

Connect over USB, delete `kindil/OVERLAYS` on the drive, eject, reboot.
Expect no framework mount, md5 `cf6613a4…`, and no `overlays:` lines for
that boot. Without a reboot, as root: the undo command of step 1.

Clean up the probes: `rm -rf /var/local/kindil/probe /var/local/kindil/probe-dir /var/local/overlay-probe`.

## Still to confirm on the device

- `reload-configuration` picks up the bound file (read from upstart's
  source; step 1 shows it in behaviour).
- MS_REMOUNT|MS_BIND|MS_RDONLY on this kernel (step 3's `ro`). If it
  fails the launcher logs `can't overlay (read-only remount: …)` and
  applies nothing.
- The bind lands before the framework's first start on every boot (step 3).
- `/mnt/us` is up when the boot job runs: `boot.sh` has always read
  `BOOT` there, and `/mnt/base-us` is the fallback.
- A broken overlay that made Amazon's UI fail would meet Amazon's own
  `lab126_gui` monitor, which reboots after 3 failed starts and, at 2 such
  reboots, shows the customer-service screen and shuts down (clearing its
  counters for the next power-on). Our counter reaches 2 in the same two
  boots, so the third is stock either way.
