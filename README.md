# kindil · قنديل

A prayer-time wall dashboard and e-reader for the first-generation Kindle
Paperwhite (PW1), in Rust and Slint.

kindil runs **as an app on Amazon's stock OS**, started from KUAL. While it
runs, Amazon's UI steps aside; when it exits or crashes, Amazon's UI comes
back. It never flashes, never replaces init or the bootloader, and changes no
system file. A reboot always returns the Kindle to stock.

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
- **Settings**: home screen, front light, a "clean screen" flash, Wi-Fi on/off
  with network status, scan and join, SSH server and SSH-at-boot switches,
  Syncthing, device facts (battery, memory, storage, uptime), Restart and
  Power off.
- **Device duties** that Amazon's UI normally performs:
  - **Power button**: sleeps (a sleep screen, front light off, suspend to
    RAM) and wakes, with the light restored. powerd ignores the button while
    kindil runs, so kindil hears it through kernel uevents.
  - **Sleep when idle**: Never (the default, for an always-updating wall
    dashboard) or after 5, 15, 30 or 60 minutes without a touch.
  - **USB drive mode**: plugging into a computer shows a USB screen while the
    computer has `/mnt/us`; unplugging reloads the library and resumes
    Syncthing. A cable that keeps dropping doesn't flash the screen: kindil
    returns only once the storage has stayed back for 3 s.
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
Start it from KUAL → **kindil → Start kindil**. **Settings → Exit to Kindle**
returns to Amazon's UI. The log is `/var/local/kindil/kindil.log` (over SSH).
Restart and Power off also come back to Amazon's UI on the next boot.

If kindil ever stops responding: SSH in and `killall kindil` (the launcher then
restarts Amazon's UI), or hold the power button for about 20 s to reboot.

## Files on the device

| Path | What |
|---|---|
| `/mnt/us/extensions/kindil/` | KUAL launcher and the `kindil` binary |
| `/var/local/kindil/` | The running copy and the log; off `/mnt/us`, which a computer takes in USB drive mode |
| `/mnt/us/kindil/kindil.toml` | Optional settings (every key has a default) |
| `/mnt/us/kindil/miqat.toml` | Miqat's own settings: location, prayer method |
| `/mnt/us/kindil/prefs.toml` | Choices made on the device; safe to delete |
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
