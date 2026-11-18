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
