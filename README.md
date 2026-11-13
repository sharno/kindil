<img src="assets/logo.svg" alt="" width="96" align="right">

# kindil · قنديل

A prayer-time wall dashboard and e-reader for the first-generation Kindle
Paperwhite (PW1), in Rust and Slint.

kindil runs **as an app on Amazon's stock OS**, started from KUAL. While it
runs, Amazon's UI steps aside; when it exits, Amazon's UI comes back. A crash
restarts it; a version that keeps crashing makes way for the previous one, and
then for Amazon's UI. It never flashes or replaces init or the bootloader.
Unless you opt into [start at boot](#start-at-boot), it changes no system file
and a reboot always returns the Kindle to stock.

## What it does

- **Home**: the [Miqat](https://github.com/sharno/miqat) prayer dashboard,
  full-bleed and pixel-exact, refreshed on each minute; or a large clock. A
  small battery gauge always shows at the top. Tap the dashboard to reveal the
  status row, the quick panel and the tab bar; they hide again after 20 s.
- **Status row and quick panel**: every page has a slim status row (clock,
  Wi-Fi, battery). Tapping it drops a panel over the page with the front light
  slider (dim sun to bright sun, with the level) and, in the reader, text size
  smaller/larger. A tap outside, or 20 s, puts it away; using a control
  restarts the 20 s.
- **Library and reader**: `.epub` and UTF-8 `.txt` files from `/mnt/us/books`.
  Tap the right two-thirds of a page to go forward, the left third to go back.
  kindil remembers the position in every
  book, the text size and the home screen across restarts. Rename and Trash
  (with Undo) from a book's **···** menu; nothing is ever overwritten or
  deleted outright.
- **Settings**: an index of sections, each row showing what it is set to now,
  and each section a page that fits on screen (a page swap is one e-ink
  refresh; scrolling redraws on every move):
  - **Display**: home screen, front light, a "clean screen" flash.
  - **Wi-Fi**: on/off with network status, scan and join.
  - **Remote access**: SSH server and SSH-at-boot switches, Syncthing.
  - **Power**: sleep when idle, start at boot, Exit to Kindle, Restart and
    Power off.
  - **Updates** and **About** (battery, memory, storage, uptime, and the
    battery draw measured between doze's online windows).
- **Updates over the air** from this repo's GitHub releases; see
  [Updates](#updates).
- **Device duties** that Amazon's UI normally performs:
  - **Power button**: sleeps (a sleep screen, front light off, suspend to
    RAM) and wakes, with the light restored. powerd ignores the button while
    kindil runs, so kindil hears it through kernel uevents.
  - **Sleep when idle**: Doze (the default), Never, or a full sleep after
    5, 15, 30 or 60 minutes without a touch. Doze is for an always-on wall
    dashboard: after 2 minutes without a touch the light goes off and the
    Kindle suspends to RAM, waking on the RTC alarm each minute to redraw the
    clock and countdown, and every 30 minutes to fetch over Wi-Fi. The power
    button brings it back with the light restored. wifid never hears of a
    suspend kindil starts, so kindil switches Wi-Fi off and on after one.
  - **USB drive mode**: plugging into a computer shows a USB screen while the
    computer has `/mnt/us`; unplugging reloads the library and resumes
    Syncthing. A cable that keeps dropping doesn't flash the screen: kindil
    returns only once the storage has stayed back for 3 s.
  - **Low battery**: at 5% and not charging, kindil sleeps with a screen that
    asks for a charge. Suspended, the last few percent last for days.
  - **Crash restart**: the launcher restarts kindil after a crash. After 3
    crashes within 5 minutes it runs the previous version (kept by the last
    update), and after 3 more it returns to Amazon's UI.
- **E-ink care**: while a finger drags a list, the list follows it in fast
  black-and-white frames (the A2 waveform), each sent only once the panel has
  finished the last, so motion doesn't smear. Every 8 frames of a long drag
  the moved area gets a fast black-and-white clean, and on release a hard
  grayscale clean (a flash of that area only). In the
  reader, swipe left/right or tap to turn pages. Page changes flash the panel
  once; reading flashes every 6th page turn, the dashboard every 30 minutes, to
  clear ghosting.

## Install on the Kindle

Needs a jailbroken PW1 with KUAL. SSH needs the USBNetwork hack; for Wi-Fi
SSH set `USE_WIFI="true"` and `USE_WIFI_SSHD_ONLY="true"` in
`/mnt/us/usbnet/etc/config` and add your key to `usbnet/etc/authorized_keys`.

```sh
nix develop
scripts/dev.sh device                  # dist/kindil, static ARMv7
scripts/dev.sh deploy root@KINDLE_IP   # binary + KUAL launcher over SSH
```

Or copy over USB: `kindle/extensions/kindil` to `/mnt/us/extensions/kindil`,
and `dist/kindil` to `/mnt/us/extensions/kindil/bin/kindil`.

Then put your Miqat settings at `/mnt/us/kindil/miqat.toml` and, optionally,
a `/mnt/us/kindil/kindil.toml` (see [examples/kindil.toml](examples/kindil.toml)).
Start it from KUAL → **kindil → Start kindil**. **Settings → Power → Exit to Kindle**
returns to Amazon's UI. The log is `/var/local/kindil/kindil.log` (over SSH).
Restart and Power off also come back to Amazon's UI on the next boot.

If kindil ever stops responding: SSH in and `killall kindil` (the launcher then
restarts Amazon's UI), or hold the power button for about 20 s to reboot.

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
  files it was built with; they take effect at the next start.
- **Scope:** only kindil's own files change; the bootloader, kernel and system
  partition are never touched.

To release: bump `version` in `Cargo.toml`, commit, then publish a GitHub
release tagged `vX.Y.Z`. The Release workflow builds, signs and attaches
`kindil` and `kindil.minisig`. The signing key is the `MINISIGN_SECRET_KEY`
repository secret.

## Start at boot

Optional, and off until you turn it on. It needs one boot job,
`/etc/upstart/kindil.conf`: the only system file kindil ever adds. Install it
once over SSH:

```sh
sh /mnt/us/extensions/kindil/boot/install.sh install
```

Then KUAL → **kindil → Start at boot: on**. The job runs whenever Amazon's UI
starts and hands over to `bin/boot.sh` on the user storage, which starts
kindil once per boot while `/mnt/us/kindil/BOOT` exists. Amazon's UI still
starts first and is there whenever kindil exits (Exit to Kindle, or crashes
it can't recover from); it doesn't start kindil again until the next boot.

- **Failed boots:** if kindil doesn't stay up for 2 minutes in two boots in a
  row, the next boots go to Amazon's UI. **Start at boot: on** resets that.
- **Off:** KUAL → **Start at boot: off**, or delete `/mnt/us/kindil/BOOT` over
  USB. The job stays and does nothing.
- **Undo:** `sh /mnt/us/extensions/kindil/boot/install.sh remove` removes the
  job.

## Files on the device

| Path | What |
|---|---|
| `/mnt/us/extensions/kindil/` | KUAL launcher and the `kindil` binary; during an update also `kindil.next`, `kindil.trying`, and afterwards `kindil.previous` |
| `/var/local/kindil/` | The running copy and the log; off `/mnt/us`, which a computer takes in USB drive mode |
| `/mnt/us/kindil/kindil.toml` | Optional settings (every key has a default) |
| `/mnt/us/kindil/miqat.toml` | Miqat's own settings: location, prayer method |
| `/mnt/us/kindil/prefs.toml` | Choices made on the device; safe to delete |
| `/mnt/us/kindil/BOOT` | Present: start kindil at boot (see [Start at boot](#start-at-boot)) |
| `/etc/upstart/kindil.conf` | Only if you installed start at boot |
| `/mnt/us/books/` | Books |
| `/mnt/us/books/.kindil-trash/` | Trashed books, recoverable |

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

## Develop

```sh
git clone --recurse-submodules git@github.com:sharno/kindil.git
cd kindil
nix develop

scripts/dev.sh sim       # every screen → dist/sim/*.png, simulated device
scripts/dev.sh desktop   # interactive window, simulated device
scripts/dev.sh check     # fmt, clippy, tests
scripts/dev.sh test-arm  # tests on ARM under qemu-arm
```

`preview` and `desktop` use a simulated device: no file under `/mnt/us`, no
framebuffer, no front light and no network is touched. Previews use fixture
weather and make no requests.

### Layout

| Module | Role |
|---|---|
| `ui/app.slint` | The whole UI: theme, components, pages, the `Kindil` global |
| `ui.rs` | Wiring: callbacks, worker threads, timers, e-ink refresh policy |
| `view.rs` | Pure domain → display text (units, wording) |
| `host.rs` | The `Host` trait the UI uses, and the `Simulated` device |
| `kindle.rs` | `Host` on the stock OS: lipc (wifid, powerd front light and battery), USBNetwork |
| `wifi.rs` | wpa_supplicant control socket, typed SSID/passphrase |
| `sync.rs` | Syncthing as a typed state machine |
| `update.rs` | Over-the-air updates: GitHub releases, signature check, trial and commit |
| `system.rs` | Sleep, wake and USB drive mode as a state machine |
| `reader.rs`, `files.rs`, `prefs.rs` | Books, Trash, persisted choices |
| `ghosting.rs` | When to flash the panel |
| `vendor/slint-backend-kindle` | Display/touch backend, patched for full refresh |

The build pins Slint 1.18.1 with its software renderer and full font shaping
(Arabic joins correctly). IBM Plex Sans Arabic, from Miqat, is the only font.
The device binary is static ARMv7 musl, linked by Zig for the 2.6.31 kernel.

## Validation status

Rendering, touch, Arabic shaping and every UI flow are tested on the host with
the real Slint component and renderer.

On a PW1 (firmware 5.6.1.1, kernel 2.6.31):

- **Confirmed:** the static binary runs, and `kindil preview` renders every
  screen identically to the host in 6 s, with about 13 MB peak memory. The
  lipc properties kindil reads exist (`wifid enable`; powerd `battLevel`,
  `isCharging`, `flIntensity`, `flMaxIntensity`), as do the wpa_supplicant
  socket and the USBNetwork toggle.
- **Still to confirm:** the interactive session (panel refresh and touch
  through the backend), and every switch exercised on the device.

See [docs/VALIDATION.md](docs/VALIDATION.md).

## Licensing

Project code is MIT. Slint is GPL-3.0 / Slint licensed; review before
distributing. See [THIRD_PARTY.md](THIRD_PARTY.md).
