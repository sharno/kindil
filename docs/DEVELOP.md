# Developing kindil

Building, testing and installing kindil from source. For what it does, see
the [README](../README.md) and [How kindil works](HOW-IT-WORKS.md).

## Build and test

```sh
git clone git@github.com:sharno/kindil.git
cd kindil
nix develop

cargo xtask sim       # every screen → dist/sim/*.png, simulated device
cargo xtask desktop   # interactive window, simulated device
cargo xtask check     # fmt, clippy, every test suite
cargo xtask verify    # the formal models, and the tests' traces against them
cargo xtask test-arm  # tests on ARM under qemu-arm
```

The dashboard engine, Miqat, is its own project
([sharno/miqat](https://github.com/sharno/miqat)), carried in
`crates/miqat` as a git subtree: a plain clone has it, and changes made here
go back with `git subtree push`.

```sh
git subtree pull --prefix=crates/miqat https://github.com/sharno/miqat.git main   # take Miqat's changes
git subtree push --prefix=crates/miqat https://github.com/sharno/miqat.git BRANCH # send kindil's back
```

`preview` and `desktop` use a simulated device: no file under `/mnt/us`, no
framebuffer, no front light and no network is touched. Previews use fixture
weather and make no requests.

### On the Kindle

`cargo xtask` (the `xtask/` crate) reaches the Kindle over SSH
(`KINDIL_DEVICE`, optionally `KINDIL_SSH_KEY` and `KINDIL_SSH_PORT`), and
copies files with plain `ssh` (no scp or sftp needed on the Kindle):

```sh
cargo xtask release 0.9.1 [--rollback 0.9.0]   # install a release
cargo xtask run experiment.sh 600 [ARGS]       # run a script, with a lifeline
cargo xtask wait [--hours N] [COMMAND]         # wait for it to answer
```

`release` checks the release's signature, bundles it with the launcher
files from its tag and waits for the Kindle; there the release installs
itself (`kindil install`, src/install.rs) and rolls back, launcher files
and all, unless it is the version running, with its Wi-Fi joined, and stays
so for a minute. `run` starts a script with `kindil-lifeline` beside it,
which brings kindil and Wi-Fi back when the script ends or at the deadline.
It runs the installed binary on the user storage, never the copy on
/var/local (which it would keep from being replaced), so USB drive mode
can't take the storage while a lifeline runs. A lifeline needs the Kindle awake: tests that keep it asleep or dozing need
someone nearby to press the power button, or must end on their own first.

### Layout

| Module | Role |
|---|---|
| `ui/app.slint` | The whole UI: theme, components, pages, the `Kindil` global |
| `ui.rs` | Wiring: callbacks, worker threads, timers, e-ink refresh policy |
| `view.rs` | Pure domain → display text (units, wording) |
| `host.rs` | The `Host` trait the UI uses, and the `Simulated` device |
| `kindle.rs` | `Host` on the device: battery, charger and front light drivers, power button, volumd's events or kindil's USB drive mode, USBNetwork |
| `power.rs` | powerd's rules kept by kindil: charging temperatures, critical battery |
| `wifi.rs` | wpa_supplicant control socket, typed SSID/passphrase |
| `net.rs` | Wi-Fi run by kindil: driver, supervised wpa_supplicant and udhcpc (or kindil's own DHCP client) |
| `dhcp.rs` | udhcpc's hook, `kindil-dhcp`: the lease onto the interface, and its marker for kindil's own Wi-Fi |
| `dhcpc.rs`, `dhcpc/` | kindil's own DHCP client, a thread in place of udhcpc, behind the `/var/local/kindil/dhcp` switch (see [DHCP-TEST.md](DHCP-TEST.md)): RFC 2131's states without I/O (`machine.rs`, messages by dhcproto), a packet socket's replies before there is an address (`wire.rs`, with its BPF filter), the lease onto the interface by ioctl (`apply.rs`), and lease times that count the time asleep |
| `station.rs`, `wext.rs`, `pmk.rs` | kindil's own Wi-Fi without wpa_supplicant, behind the `/var/local/kindil/wifi` switch (see [docs/WIFI-RUST-TEST.md](WIFI-RUST-TEST.md)): the AR6003 firmware's WPA2-PSK offload through wireless-extension ioctls |
| `networks.rs` | The saved Wi-Fi networks |
| `ssh.rs` | SSH run by kindil: its kindil-sshd, firewall port, supervised |
| `ssh_keys.rs` | The keys it trusts: seeded once from USBNetwork's, then trusted in Settings by fingerprint |
| `sync.rs` | Syncthing as a typed state machine |
| `update.rs` | Over-the-air updates: GitHub releases, signature check, trial and commit |
| `system.rs` | Sleep, wake and USB drive mode as a state machine |
| `trace.rs` | The state machine's steps as JSON lines, for the formal model (see [Formal models](#formal-models)) |
| `drive.rs`, `usb.rs` | USB drive mode run by kindil: the state machine and sequences, and the device under them |
| `reader.rs`, `files.rs`, `prefs.rs` | Books, Trash, persisted choices |
| `location.rs` | Settings → Location: city search, IP lookup, typed coordinates, written into `miqat.toml` and `kindil.toml` |
| `clock.rs` | Time keeping: SNTP and HTTP `Date`, the RTC after each resume |
| `maintenance.rs` | Doze's maintenance window every 2 hours, and staying awake while an SSH session is open (`/proc/net/tcp`) |
| `prayer.rs` | Settings → Prayer times: the methods and Asr schools with today's times, edited into `miqat.toml` |
| `ghosting.rs` | When to flash the panel |
| `launcher.rs` | Writes out the launcher kindil carries, once it has run two minutes |
| `vendor/slint-backend-kindle` | Display/touch backend, patched for full refresh |
| `sshd/` | kindil-sshd, its own small crate: keys-only SSH server (exec, shells on a pty) |
| `launcher/` | The launcher, its own small crate: the session around kindil (Amazon's UI and services aside, crash restart, update trial, restore) and the boot guard |
| `installer/` | The [installer](../README.md#install), for the computer the Kindle is plugged into (never built for the Kindle): the `kindil-installer` library and `kindil-install` CLI; `installer/gui/` is the Slint window, `kindil-installer-gui` |

The build pins Slint 1.18.1 with its software renderer and full font shaping
(Arabic joins correctly). IBM Plex Sans Arabic, from Miqat, is the only font.
The device binary is static ARMv7 musl, linked by Zig for the 2.6.31 kernel.

## Formal models

`verify/` holds TLA+ models of the designs whose bugs hide in the order of
events: sleeping, dozing and the battery, the launcher's session and update
trial, and start at boot. [everystate](https://github.com/mattzcarey/everystate)
(a TLA+ model checker in Rust, TLC's language and output; in the dev shell)
checks every reachable state of each, and checks recorded runs of the code
against the model. `cargo xtask verify` runs it all, in under two minutes,
and CI runs it after `cargo xtask check`; without everystate it says so and
skips.

| Model | What it holds | Configurations |
|---|---|---|
| `Sleep.tla` | `Mode::step` (src/system.rs) arm for arm, the effects `Ctx::apply` runs (src/ui.rs: timers, workers, a doze wake ended by its frame on the panel or its ceiling), the host's nap and sleep loop with its 6-hourly check (src/kindle.rs, src/power.rs), and the world: the button, the charger and its `connected` file appearing late, the gauge around 5%, the RTC alarm, volumd | `Sleep`: sleep, the battery, the charger; `SleepDoze`: dozing in detail; `SleepUsb`: USB drive mode; `SleepOpen`: the reducer alone, any event at any time; `SleepTrace`: for the UI tests' traces |
| `Launcher.tla` | The session (launcher/src/session.rs): every way out restores what it stopped; crash limits; the update trial and `Slots::commit` (src/update.rs) | `Launcher` |
| `Boot.tla` | `launcher boot` (launcher/src/boot.rs, overlay.rs) and bin/boot.sh, boot after boot: the guard's tries and limit, the standalone boot's way back, overlays skipped after an unproven boot | `Boot` |

Actions are named after the code's events and functions, with a comment
saying which function each stands for. Each property is an `INVARIANT` or a
`PROPERTY` (liveness, under the fairness written in `Spec`) in the module;
the timing the models abstract away is written there as named assumptions
(the idle timer comes after the short timers; the charger's `connected`
appears within seconds; Sleep's `Races`).

Some configurations fail on purpose: each reproduces a finding on the code
as it is (the header of the .cfg says which), and `xtask/src/verify.rs`
lists it as `Expect::Fails`. Once the code is fixed it holds, `verify` says
so and fails, and the entry becomes `Expect::Holds`: a fixed bug stays
checked. A configuration that should hold and doesn't fails the task with
everystate's counterexample: read it as a story in the code's words (the
actions are the events, in order) and decide against the code which side is
wrong.

To check one by hand, from `verify/`:

```sh
everystate -workers auto -config SleepDoze.cfg Sleep.tla   # safety on every core
everystate -config Sleep.cfg Sleep.tla                     # with PROPERTIES: one thread
everystate -json -config SleepCharger.cfg Sleep.tla        # the counterexample as JSON
```

**Traces.** src/trace.rs writes every event `Ctx::device` handles as a JSON
line named as the model's actions, with the mode after it, its effects, why
it was ignored and a count of the events (so the checker's unlogged steps
can't be events). `verify` runs the reducer's seeded random runs
(`trace::tests::random_events`) and the UI tests (tests/slint_ui.rs) with
`KINDIL_TRACE=target/verify/traces`, a file per test, then `everystate
conform` checks each against `Sleep.tla`: the random runs against
`SleepOpen` (any event at any time), the UI tests against `SleepTrace` (the
app's own timers and workers and the host's naps as modelled, up to 3
unlogged steps of theirs before each event). A change to `Mode::step` or to
what an effect starts that the model doesn't share fails here. On the
Kindle, `touch /var/local/kindil/trace` records the next start's events to
`/var/local/kindil/trace.jsonl` (up to 1 MiB), to check against the model
the same way.

A pass covers the models at their bounds, under their assumptions; the
models were last confirmed against the code at 0.15.0. Change `system.rs`,
the effects in `ui.rs`, `kindle.rs`'s nap or suspend, or the launcher, and
`verify` says whether the model still describes it.

## Install over SSH

Needs a jailbroken PW1 with KUAL. SSH needs the USBNetwork hack; for Wi-Fi
SSH set `USE_WIFI="true"` and `USE_WIFI_SSHD_ONLY="true"` in
`/mnt/us/usbnet/etc/config` and add your key to `usbnet/etc/authorized_keys`.

```sh
export KINDIL_DEVICE=root@KINDLE_IP     # and KINDIL_SSH_KEY=~/.ssh/key, if needed
cargo xtask release 0.9.1               # a signed release, with rollback
```

Or build and copy your own (the build needs Linux, for the Zig and ARM
toolchains in the flake):

```sh
nix develop
cargo xtask device                     # dist/kindil, static ARMv7
cargo xtask deploy                     # build, then binary + KUAL launcher over SSH
```

Or copy over USB: `kindle/extensions/kindil` to `/mnt/us/extensions/kindil`,
and `dist/kindil` to `/mnt/us/extensions/kindil/bin/kindil`.

Then put your Miqat settings at `/mnt/us/kindil/miqat.toml` and, optionally,
a `/mnt/us/kindil/kindil.toml` (see [examples/kindil.toml](../examples/kindil.toml)).
Start it from KUAL → **kindil → Start kindil**. **Settings → Power → Exit to Kindle**
returns to Amazon's UI. The log is `/var/local/kindil/kindil.log` (over SSH).
Restart and Power off also come back to Amazon's UI on the next boot.

If kindil ever stops responding: SSH in and `killall kindil` (the launcher then
restarts Amazon's UI), or hold the power button for about 20 s to reboot.

## Emulator

`cargo xtask emulate` runs kindil on an emulated PW1:
[katadelos/eink-emulator](https://github.com/katadelos/eink-emulator), a
QEMU with the PW1's board (`imx50-celeste`), booting the PW1's own kernel
(2.6.31) and system partition. It is the device's software on emulated
hardware: upstart and Amazon's boot, the jobs kindil's launcher stops and
restores, KUAL's way in (`start.sh`), the boot job and a standalone boot,
the EPDC display driver and its update ioctls, the cyttsp touchscreen, the
AR6003 Wi-Fi driver with kindil's own wpa_supplicant and DHCP, kindil's SSH
server, and the battery, charger and front-light sysfs files.

Nothing of it lives in this repo, and no firmware does: it comes from your
own PW1. You need

- the emulator's checkout with its QEMU built (`./eink build`; it needs
  Python 3.10+, Ninja, Meson, GLib, pixman, dtc and e2fsprogs: with Nix,
  a shell with `python3 ninja meson pkg-config glib pixman dtc bison flex`
  and `e2fsprogs.bin` builds it on macOS; e2fsprogs's development headers
  break QEMU's Cocoa window there);
- a firmware directory, outside the repo, with the PW1's kernel `uImage`
  (from the eMMC at 0x41000: its 64-byte header, then the size it gives),
  `rootfs.img` (mmcblk0p1, raw ext3), and a bootloader. No PW1 update
  carries U-Boot: put the device's own there as `u-boot.bin` if you have it,
  or build upstream barebox, which supports the PW1 as EY21. It needs Linux
  (macOS's case-insensitive disk can't even check it out; a container
  does):

  ```sh
  git clone --depth 1 https://github.com/barebox/barebox && cd barebox
  make ARCH=arm CROSS_COMPILE=arm-linux-gnueabihf- kindle-mx50_defconfig
  make ARCH=arm CROSS_COMPILE=arm-linux-gnueabihf-
  cp images/barebox-kindle-ey21.img "$KINDIL_EMULATOR_FIRMWARE"/
  ```

  The kernel doesn't boot without one: it needs Lab126's ATAGs (board ID,
  serial) and the DDR type U-Boot leaves in the DDR controller, or its idle
  loop calls a function it never set up. barebox passes the ATAGs from the
  emulated IDME; it can't read the emulated eMMC (its DMA reads come back
  empty), so QEMU puts the kernel in RAM at every reset and xtask tells
  barebox, over the serial console, to set the DDR type and boot it.
  Never copy the device's `/var/local` (mmcblk0p3) into the emulator: it
  holds your Wi-Fi passwords and keys. The emulator gives the guest a fresh
  one, and a synthetic serial number and MAC.

```sh
export KINDIL_EINK_EMULATOR=~/src/eink-emulator
export KINDIL_EMULATOR_FIRMWARE=~/kindle-firmware/pw1   # uImage, rootfs.img, bootloader
cargo xtask emulate setup        # the instance, from the firmware directory
cargo xtask emulate run          # boot it (headless; --window to see and click it)
cargo xtask emulate install      # build kindil and install it, with example settings
ssh -i "$KINDIL_EINK_EMULATOR/build/ssh/id_ed25519" -p 4122 root@127.0.0.1 \
  /mnt/us/extensions/kindil/bin/start.sh
cargo xtask emulate tap 379 500                # a finger at panel pixel (x, y), 758 × 1024
cargo xtask emulate screenshot dist/emulator/now.png
cargo xtask emulate stop
cargo xtask emulate test         # all of it from a clean instance, then a standalone boot
```

Without the two variables every task says so and does nothing, so CI and
other computers are unaffected. `install` adds what a first start needs
where it isn't there yet: `examples/miqat.toml` and `examples/kindil.toml`,
the emulator's open access point `Kindle-QEMU` in kindil's saved networks,
the emulator's key in USBNetwork's `authorized_keys` (which seeds kindil's
SSH server's), and "sleep when idle: never" (see below). Ports: 4122 is
SSH over USB, 4123 the emulator's SSH over Wi-Fi, 4124 kindil's SSH server
over Wi-Fi (`KINDIL_EMULATOR_SSH_PORT` moves all three). After the guest
reboots itself (Settings → Power → Restart), `cargo xtask emulate boot`
takes barebox through the boot again. `test` leaves its screenshots in
`dist/emulator/`.

What it doesn't cover, of what kindil touches:

| kindil uses | In the emulator |
|---|---|
| The panel's look: waveforms, ghosting, partial-update artefacts | Synthetic waveforms; the screenshot is the framebuffer, not the ink |
| Power button (`yoshibutton` uevents) | Never arrives: the press reaches the emulated PMIC, whose interrupt for it the PW1's kernel leaves masked |
| Battery gauge (`yoshi_battery`) | Fixed: 80 %, 77 °F, 3.9 V, 0 mA; low battery, the 1 % power-off and the charging temperature limits can't be reached |
| Charger (`aplite_charger.0/connected`, `charging`) | Always connected and charging (the USB link holds VBUS): no unplug, no plug-in wake |
| Front light (`fl_tps6116x`) | The file takes the level; there is no light |
| Suspend to RAM (`/sys/power/state`) and the `rtc1` alarm | Doze's naps suspend (the CPU waits in the kernel's iRAM suspend code) and the PMIC's alarm is raised and taken, but USB networking didn't come back: the kernel disabled the USB controller's interrupt (`irq 18: nobody cared`), SSH stayed unreachable, and QEMU later exited on its own. Doze and sleep are therefore off in `install`'s settings |
| USB drive mode (the gadget's `lun0`, volumd) | The emulator swaps the storage gadget for USB Ethernet: no drive mode |
| Wi-Fi security, kindil's own WPA2 offload (`/var/local/kindil/wifi`) | Only the open `Kindle-QEMU` network: no WPA, no signal changes |
| Real time and battery life | QEMU's clock follows the computer's; nothing measures power |

## Releasing

To release: bump `version` in `Cargo.toml`, commit, then publish a GitHub
release tagged `vX.Y.Z`. The Release workflow builds, signs and attaches
`kindil` and `kindil.minisig`. The signing key is the `MINISIGN_SECRET_KEY`
repository secret.

## Validation

Rendering, touch, Arabic shaping and every UI flow are tested on the host
with the real Slint component and renderer. On the Kindle (PW1, firmware
5.6.1.1, kernel 2.6.31) kindil has run as the daily dashboard since
November 2026; device tests for the parts still behind switches are in
[USB-DRIVE-TEST.md](USB-DRIVE-TEST.md), [WIFI-RUST-TEST.md](WIFI-RUST-TEST.md)
and [OVERLAY-TEST.md](OVERLAY-TEST.md). See also [VALIDATION.md](VALIDATION.md).
