# Validation on the PW1

kindil runs as an app on the stock OS, so testing on the device is reversible:
quitting kindil, killing it, or rebooting returns the Kindle to Amazon's UI.
Nothing below writes a system partition.

## Software gates

```sh
nix develop
scripts/dev.sh check
scripts/dev.sh test-arm
scripts/dev.sh device
qemu-arm dist/kindil --version
```

`tests/slint_ui.rs` drives the production window with real pointer events on a
simulated device: the pixel-exact Miqat frame, tab navigation, page-turn tap
zones, saved reading positions, rename through the touch keyboard, Trash and
Undo, settings switches, joining a network with a typed password, Arabic
keyboard entry and contextual shaping, and the full printable-ASCII key set.

Screenshots of a test run:

```sh
KINDIL_TEST_PREVIEW_DIR=dist/touch-tests cargo test --test slint_ui
```

## On the device, in order

Each step needs the one before it. Keep an SSH session open throughout.

1. `/mnt/us/extensions/kindil/bin/kindil --version` over SSH. Proves the static
   binary runs on Linux 2.6.31. If it fails, nothing else is worth trying.
2. `kindil preview /tmp/p` over SSH. Exercises Slint, fonts and Miqat on the
   device without touching the screen; time it and note peak RSS.
3. Start from KUAL. Check: the dashboard appears; a tap reveals the tabs; the
   tabs respond where tapped (touch orientation); Settings → Exit to Kindle
   restores Amazon's UI.
4. Reading: page turns, the Aa panel, front light steps, a full flash on
   every 6th turn.
5. Settings: Wi-Fi status and IP, scan, SSH switch (sshd stops and starts),
   SSH at boot (`/mnt/us/usbnet/auto` appears and disappears), battery.
6. Power button: a press shows the sleep screen, switches the light off and
   suspends; the next press wakes with the light back and Wi-Fi reconnects
   (`kindil uevents` prints the button as `online@.../yoshibutton`). USB:
   plugging in shows the USB screen and the computer sees the Kindle drive;
   unplugging returns to kindil with a clean screen.
7. Leave the dashboard running for an hour plugged in. Record memory, minute
   updates, and the 30-minute ghosting flash.
8. Optional: Syncthing, watching memory and the log.

Record kernel messages (`dmesg`) and `/var/local/kindil/kindil.log` for any
failure.
