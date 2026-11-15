# USB drive mode run by kindil: the hands-on test

About 20 minutes with the Kindle, a USB cable, a computer (macOS assumed
below; any OS works) and a wall charger. Nothing here changes the Kindle
until step 3 turns the switch on. Step 7 turns it off again.

## What changes when it is on

kindil does USB drive mode itself (src/drive.rs, src/usb.rs) instead of
Amazon's volumd:

- **The switch:** the file `/var/local/kindil/usb-drive` holding `kindil`.
  Absent, or holding anything else (`amazon`), volumd keeps doing USB drive
  mode as before, and kindil only follows its events. The file is on the
  system storage (not `/mnt/us`), so a computer that has the user storage
  can't take the switch with it, and over SSH it can always be turned off.
  It is a flag file, not a `kindil.toml` key, because older kindil versions
  (a rollback) reject unknown keys in `kindil.toml` and wouldn't start.
- **The launcher**, when a session starts with the switch on, stops volumd
  after kindil is running (volumd still starts at boot, which kindil's boot
  job needs). That stops USBNetwork's `usbnet` and `usbnet-autostart` jobs
  too. Their post-stop runs `usbnetwork usbms`, which **stops USBNetwork's
  dropbear**: SSH must be kindil's own server. When kindil exits, the
  launcher starts volumd again, and the USBNetwork jobs that were running.
- **kindil** waits for volumd to go (up to 2 minutes; if it doesn't go,
  volumd keeps USB drive mode), then:
  - loads `g_file_storage removable=y` with an empty LUN if no gadget is
    loaded, so a computer is noticed;
  - on a computer configuring the gadget (`online` on `fsl-usb2-udc`) for
    3 s: refuses if any program has files open on `/mnt/us` or
    `/mnt/base-us`; stops Syncthing; syncs; unmounts `/mnt/us` (fsp), then
    `/mnt/base-us`; checks `/proc/mounts`; swaps `g_ether` for
    `g_file_storage`; writes `/var/local/kindil/usb-exported`; and only then
    writes `/dev/mmcblk0p4` to the LUN;
  - on the cable going (charger uevent, or `connected` reading 0) or the
    LUN emptying (an eject from the computer): empties the LUN (unloads the
    gadget if it won't empty), runs `fsck.vfat -a -v /dev/loop/0`, mounts
    `/mnt/base-us` (read-only if fsck exits 2 or more) and fsp, removes the
    flag, restores `g_ether` if it was loaded, and reloads the library;
  - at start, with the flag still there (a crash or reboot while the
    computer had the storage), unmounts, checks and mounts the storage again.
- **A wall charger** never configures the gadget, so nothing happens.

The kindil log is `/var/local/kindil/kindil.log`; every line of this
feature contains `USB drive`.

## 0. Preconditions

Over SSH (Wi-Fi) as root. Every command assumes:

```sh
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
K=/var/local/kindil
LUN=/sys/devices/platform/fsl-usb2-udc/gadget/gadget-lun0/file
```

1. SSH is **kindil's own server**, not USBNetwork's dropbear (which lives on
   `/mnt/us` and is stopped by step 3). Check:
   `ps | grep -e dropbear -e sshd | grep -v grep` shows no USBNetwork process
   serving your session.
2. USBNetwork is **off**, so `g_ether` isn't loaded at boot:
   ```sh
   mv /mnt/us/usbnet/auto /mnt/us/usbnet/DISABLED_auto   # if auto exists
   reboot
   ```
   After the reboot: `lsmod | grep g_` shows no `g_ether`.
3. This build is deployed (`cargo xtask deploy`), and its launcher is in
   place: start kindil, wait 2 minutes, then
   ```sh
   /mnt/us/extensions/kindil/bin/kindil launcher   # prints "launcher already there"
   ```
4. Nothing in `/mnt/us` you can't lose. The test writes one file,
   `kindil-usb-test.txt`; back up `/mnt/us/books` if it matters.
5. The switch is off: `cat $K/usb-drive` fails or prints `amazon`.

## 1. Capture events with volumd in charge (switch off)

In one SSH window, record everything the kernel says about USB:

```sh
$K/kindil uevents --usb | tee $K/usb-capture-amazon.log
```

Each event prints with every field it carries, then a `signal=` line (how
kindil reads it) and a `state:` line (cable, LUN, LUN online, gadget,
mounts, volumd). Do each step slowly, about 10 s apart, and say which step
it is in a second window: `echo "STEP computer plug" >> $K/usb-capture-amazon.log`.

1. **Computer plug:** cable into the computer. Wait until the Kindle shows
   kindil's USB screen and the computer shows the drive.
2. **Finder eject:** eject the Kindle drive in Finder. Wait 10 s. Note
   whether the Kindle leaves the USB screen.
3. **Unplug** the cable.
4. **Wall charger:** plug in, wait 10 s, unplug.
5. **Yank:** plug into the computer, wait for the drive, then pull the cable
   without ejecting.

Ctrl-C, then keep `$K/usb-capture-amazon.log`.

## 2. What to read in the capture

- Which `ACTION`s `aplite_charger.0` and `fsl-usb2-udc` send on plug and
  unplug, for a computer and for the wall charger. kindil re-reads
  `connected` on any event from these two, so only the timing matters.
- That `fsl-usb2-udc` goes `online` for the computer and **never** for the
  wall charger.
- Whether a Finder eject shows: a `g_file_storage` event, `lun=""` in the
  state line, or `lun-online` changing. kindil reads an empty LUN as an
  eject (and checks it every second); if the LUN stays set after an eject,
  the Kindle returns only when the cable goes.

## 3. Switch on

```sh
echo kindil > $K/usb-drive
```

Settings → Power → **Exit to Kindle**, then start a new session (or KUAL →
Start kindil):

```sh
$K/launcher start
sleep 30
status volumd                                  # stop/waiting
status usbnet                                  # stop/waiting
grep "USB drive" $K/kindil.log | tail -5       # "started; storage Mounted"
lsmod | grep g_                                # g_file_storage
cat $LUN                                       # empty
grep /mnt /proc/mounts                         # /mnt/base-us and /mnt/us
```

Leave a capture running for the rest, in its own window:

```sh
$K/kindil uevents --usb | tee $K/usb-capture-kindil.log
```

and follow the log in another: `tail -f $K/kindil.log | grep "USB drive"`.

## 4. Export and return, with a disposable file

1. Plug into the computer. Within about 3 s the Kindle shows the USB
   screen and the computer mounts the drive.
   ```sh
   cat $LUN                                     # /dev/mmcblk0p4
   grep /mnt /proc/mounts                       # neither /mnt/us nor /mnt/base-us
   ls $K/usb-exported                           # present
   ```
2. On the computer:
   ```sh
   echo "kindil $(date)" > /Volumes/Kindle/kindil-usb-test.txt
   ```
3. Eject in Finder. If the Kindle returns now, the eject was seen. If not,
   unplug.
4. Back on the Kindle:
   ```sh
   cat /mnt/us/kindil-usb-test.txt              # the line written
   ls $K/usb-exported                           # gone
   grep -e "USB drive" -e fsck.vfat $K/kindil.log | tail -15   # "storage check: Clean"
   grep /mnt/base-us /proc/mounts               # rw
   ```
5. The library shows the same books; Syncthing, if it was on, is on again.

## 5. Refusals and the wall charger

1. **Busy:** hold a file open, then plug into the computer:
   ```sh
   sleep 300 < /mnt/us/kindil-usb-test.txt &
   ```
   Expect a notice "USB drive: not shared, sleep (PID) has files open on
   it"; the computer gets no drive (or an empty one). Unplug,
   `kill %1`, plug again: it exports. Unplug.
2. **Wall charger:** plug in. Nothing changes but charging;
   `grep "USB drive" $K/kindil.log | tail -3` shows no export. Unplug.

## 6. Yanks

1. **Yank:** plug into the computer, wait for the drive, start copying a
   large file to it, and pull the cable mid-copy. The Kindle returns in a
   few seconds, after fsck: a notice if it repaired anything ("storage
   errors repaired") or found errors it couldn't fix ("read-only"). Check:
   ```sh
   grep -e "USB drive" -e fsck.vfat $K/kindil.log | tail -20
   grep /mnt/base-us /proc/mounts               # rw, unless fsck exited 2+
   dmesg | tail -20
   ```
2. **Cable flapping:** wiggle or replug the cable quickly a few times. The
   Kindle exports only once the computer has held the gadget for 3 s, and
   comes back whenever the cable goes.
3. **Yank, then reboot** (the flag survives): plug in, wait for the drive,
   then over SSH `reboot`. After the boot, start kindil (or let start at
   boot do it). Within a minute:
   ```sh
   grep "USB drive" $K/kindil.log | tail -5    # "storage Checked(Clean)" or Repaired
   ls $K/usb-exported                           # gone
   ```
   and the notice "Storage checked after USB: clean".

Clean up: `rm /mnt/us/kindil-usb-test.txt`.

## 7. Rollback

Never while the computer has the storage: unplug first.

```sh
rm $K/usb-drive            # or: echo amazon > $K/usb-drive
```

Then Settings → Power → **Exit to Kindle**: the launcher starts volumd and
USBNetwork's jobs again. Without a session to end (kindil not running), do it
by hand:

```sh
status volumd | grep -q start/ || start volumd
```

If the storage is left unmounted or still in the LUN (only after a failure;
check `cat $LUN` and `grep /mnt /proc/mounts`):

```sh
echo > $LUN                                         # empty the LUN; if refused:
modprobe -r g_file_storage
losetup /dev/loop/0 || losetup -o 8192 /dev/loop/0 /dev/mmcblk0p4
fsck.vfat -a -v /dev/loop/0
grep -q /mnt/base-us /proc/mounts || mount /mnt/base-us
grep -q " /mnt/us " /proc/mounts || fsp /mnt/base-us /mnt/us -o rw,nosuid,nodev,noatime,allow_other,umask=0,uid=0,gid=0,max_write=65536,max_readahead=65536,nonempty
rm -f $K/usb-exported
start volumd
```

A reboot with the switch off is always the stock behaviour. Re-enable
USBNetwork if you use it: `mv /mnt/us/usbnet/DISABLED_auto /mnt/us/usbnet/auto`.

## What this session settles

Write the answers in this file, with the capture logs:

1. The plug and unplug `ACTION`s for a computer and for the wall charger;
   whether the charger ever brings `fsl-usb2-udc` online.
2. Whether a Finder eject empties the LUN (or sends any event). Without
   that, the Kindle returns only on unplug.
3. Which gadget is loaded once the launcher has stopped volumd (volumd may
   swap gadgets during usbnet's post-stop).
4. Whether emptying the LUN after a yank is refused (the computer's medium
   lock); the log then says "the LUN won't empty; unloading the gadget".
5. That `fsp`, mounted again by kindil, survives a kindil restart, and that
   volumd started by the launcher afterwards is happy with the storage
   kindil mounted.
6. Whether USBNetwork comes back as it was after Exit to Kindle, if it was
   on (the launcher unloads a leftover `g_ether` before starting volumd so
   USBNetwork's start goes the way it does at boot).
7. Whether the boot's own mount after a yank already runs fsck (volumd has
   `mntus --fsck mount`); kindil checks again regardless.
