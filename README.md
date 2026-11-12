# kindil · قنديل

A Rust/Slint userland for the first-generation Kindle Paperwhite: a Miqat wall
dashboard, an EPUB/text reader, and basic user-file management.

**Experimental, not a device-validated replacement OS. No flash/install command
is provided.** Host software rendering and QEMU user-mode tests do not establish
that an image boots on the PW1's Linux 2.6.31 kernel. Nothing in the development
commands opens the framebuffer, changes brightness, stops Amazon services, or
modifies a bootloader. `--kindle` explicitly selects the hardware backend.

## Design

- **Slint 1.18.1** owns widgets, layout, touch dispatch and software rendering.
  Full runtime font shaping is retained: bitmap-font-only rendering breaks
  Arabic contextual joining. Bundled DejaVu Sans is used for the shell.
- **Miqat is reused, not recreated.** `crates/miqat` is a pinned submodule of
  [sharno/miqat](https://github.com/sharno/miqat). Its existing Rust renderer,
  IBM Plex Sans Arabic fonts, prayer settings, Hijri calendar, weather, radar
  and contrast correction produce the same 758×1024 dashboard. Slint displays
  the frame 1:1 without a surrounding toolbar. A regression test compares every
  displayed pixel with Miqat's output. Tap the **top-right corner** for controls.
- Miqat and a simple clock are selectable dashboard views. Miqat refreshes on a
  single background worker using its existing weather/radar cache. UI input
  does not wait for network requests. Supply your real Miqat TOML unchanged.
- **slint-backend-kindle 0.7.1** is the pinned experimental hardware adapter,
  reported tested on PW1 upstream. Amazon's Linux drivers still own the EPDC,
  touch controller, Wi-Fi firmware and electrical limits. We do not replace
  those drivers or install our own waveform tables.
- **rbook**, **html2text** and **textwrap** provide the basic EPUB/text reading
  path. This is text reflow, not a full CSS/image EPUB engine. DRM, PDF, MOBI
  and AZW are not supported. EPUBs are checked against compressed/expanded-size
  limits before parsing to fit a 256 MiB device.
- SSH is **Dropbear**, Wi-Fi authentication is **wpa_supplicant**, and DHCP is
  **BusyBox udhcpc**, supervised only in standalone PID-1 mode. We do not
  reimplement cryptography or Wi-Fi drivers. SSH is key-only, on by policy;
  the runtime image must actually supply the server, keys and account files.

## Fast development

Both repositories are private; use an account/SSH key with access:

```sh
git clone --recurse-submodules git@github.com:sharno/kindil.git
cd kindil
nix develop

scripts/dev.sh sim       # actual Slint screens → dist/sim/*.ppm (+ PNG with ffmpeg)
scripts/dev.sh desktop   # interactive software-rendered desktop window
scripts/dev.sh check     # host tests, formatting, clippy
scripts/dev.sh test-arm  # run actual ARM test binaries under qemu-arm
scripts/dev.sh device    # experimental static ARM binary → dist/kindil
```

The flake uses official **prebuilt Rust + ARM musl std** and a cached **Zig**
compiler/linker. It does not build cross GCC/rustc from source. Cargo caches the
first Slint/library compilation; subsequent UI edits rebuild the application.
`nix build .#kindil` builds the host package; `nix build .#kindil-device` builds
the pinned device binary. Do not use the old `packages.armv7l-linux.kindile`
attribute or stale manually selected GCC paths.

For plain Cargo, Rust ≥1.92 and the ARM musl target are required. The Nix shell
supplies the pinned toolchain, link driver, QEMU and archive tools.

## Use

```sh
cargo run -- --preview dist/preview --config examples/kindil.toml --books examples/books
cargo run --features desktop -- --desktop --config examples/kindil.toml --books examples/books
# Only after emulator/hardware compatibility checks and a recovery plan:
# ./kindil --kindle --config /mnt/us/kindil.toml
```

`examples/miqat.toml` is Miqat's example, **not your personal configuration**.
Use `miqat_config` in Kindil's TOML (or `--miqat-config FILE`) to select the
current Miqat configuration. Previews use fixture weather and make no requests.
Desktop/device Miqat mode fetches weather/radar as the existing Miqat engine does.
Missing configuration is shown as an error, never silently replaced by another
city's prayer times. The clock dashboard has its own explicit timezone setting.

### Reader, files and controls

- The library lists up to 256 regular `.txt`/`.epub` files in the configured
  `books` directory, without following symlinks or scanning system directories.
- Open a book, turn pages, and use **Aa / ☀** for font-size and brightness
  sliders. Font changes repaginate; the reader can scroll a page as needed.
  These preferences are currently session-local.
- Preview brightness is explicitly simulated. Hardware control queries the
  PW1 driver's maximum first, clamps percentages to that range and debounces
  writes. If discovery/read/write fails, the slider is disabled. The tiny
  `/dev/yoshime_fl` adapter is the only project-owned device ioctl surface.
- **⋮** opens rename/Trash actions. Trash requires confirmation, retains bytes
  in `.kindil-trash`, and supports undo of the last move. Existing files are
  never overwritten. No recursive deletion or filesystem formatting exists.
- Text fields open an on-screen keyboard adapted from Slint's MIT example.
  Use the globe key to cycle **Latin → symbols → Arabic**, Shift for capitals
  or alternate symbols, arrows to move the cursor, and Backspace to delete.
  Esc, Return (where appropriate), or the down-arrow dismisses it; tapping the
  field opens it again. The form stays above the keyboard. All printable ASCII
  characters are available for WPA2 credentials; passwords remain masked and
  are cleared on submission. Slint owns editing and Arabic contextual shaping.
- Wi-Fi search/connect uses wpa_supplicant's control socket on a worker thread.
  WPA2 passphrases are not logged; interactive credentials are session-local.
  Previews do not launch daemons. Repeated network actions are disabled while a
  request is in flight. Touch typing is covered by actual pointer-event tests,
  not just callback/property tests; physical PW1 touch remains a release gate.

## Standalone runtime (not boot-validated)

`scripts/make-initramfs.sh RUNTIME_OVERLAY NEW_OUTPUT.cpio.gz` only stages an
experimental **RAM image**. It refuses missing helpers, wrong-architecture or
dynamically linked binaries, absent SSH authorized keys, and existing outputs.
It neither downloads random helper executables nor writes a device partition.

The overlay must provide matching static ARM BusyBox (`/bin/sh`, `ip`, udhcpc),
Dropbear, wpa_supplicant, account files, SSH host keys/authorized_keys, your
`/etc/kindil.toml`, Miqat config and any books. Exact kernel modules, panel
waveforms, firmware, Wi-Fi board calibration and storage mounts are **not**
guessed or provisioned by this project. A valid archive alone does not prove
that those prerequisites exist or that the OS boots.

PID 1 mounts proc/sysfs/devtmpfs (static-node fallback), devpts and volatile
runtime directories, supervises services and reaps orphans. It does not mount
or rewrite eMMC partitions. Fatal startup errors leave PID 1 alive for serial
diagnosis rather than deliberately panicking the kernel.

SSH is reachable only once networking, accounts, PTYs, keys and compatible
helper binaries are present. No default password is supplied. Syncthing is
optional/off by default; no claim of PW1 compatibility or memory usage is made.

## Hardware validation boundary

1. Host Slint rendering/UI tests: safe, no hardware involved.
2. ARM tests under `qemu-arm`: validate instruction/ABI behavior on the host's
   kernel, **not** EPDC, old-kernel syscalls, boot, Wi-Fi or suspend behavior.
3. Full-system testing: use [eink-emulator](https://github.com/katadelos/eink-emulator)
   with the **PW1/Celeste** model and an owned firmware image. It needs a patched
   QEMU; its upstream currently does not publish a ready-made release binary.
4. Only then consider a reversible, RAM-only boot on real hardware, with tested
   recovery. Do not flash eMMC/bootloader or copy another model's waveforms.

See [the release-gate checklist](docs/VALIDATION.md) for exact missing inputs,
safe emulator preparation and the evidence still needed. No hardware milestone
is considered complete merely because a preview, build or archive succeeds.

Modern Rust/musl libraries can assume a newer Linux than the PW1's 2.6.31. The
linker targets that kernel version, but this does **not** remove every library's
minimum-kernel requirement. This remains a release blocker until a matching
full-system boot is tested. No guarantee against bricking is possible.

Deep suspend/RTC wake is deliberately **not enabled**. Sleeping in an event loop
is not suspend-to-RAM, and continuous Wi-Fi/SSH/sync prevents e-reader-like
battery-life claims. Battery-mode scheduling must be validated against recovery,
RTC wake and actual power measurements; plugged-in dashboard use is the initial
target.

## Licensing

Project-owned code is MIT. Dependencies retain their own licenses: in particular
Slint has GPL/commercial/other alternative terms which must be reviewed before
distributing an embedded product. See `THIRD_PARTY.md`, the pinned crate licenses,
and the font license files. The old public-domain bitmap font was removed.
