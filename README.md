# kindil

A single-process Rust shell for the **first-generation Amazon Kindle
Paperwhite** (2012, model `EY21`, board "Celeste"/"Yoshime").

kindil owns the e-ink panel and the touch controller directly. There is no
init, no window manager and no compositor: one process renders the screen,
pushes it to the panel, and reacts to taps. It is meant to run either as a
[KUAL](https://www.mobileread.com/forums/showthread.php?t=203326) extension
alongside Amazon's own UI, or as PID 1 in a custom initramfs that replaces the
Kindle OS entirely.

## The device

| | |
|---|---|
| SoC | Freescale i.MX50 (i.MX508), single Cortex-A8 @ 800 MHz |
| RAM | 256 MiB |
| Panel | 6" E Ink, 212 DPI, 4-LED frontlight |
| Framebuffer | `/dev/fb0`, `mxc_epdc_fb`, **758x1024 portrait**, 8 bpp, stride 768 |
| Touch | Cypress `cyttsp` on `/dev/input/event0`, axes X 0..758 / Y 0..1024 |
| Wi-Fi | Atheros AR6003 (SDIO) |
| Kernel | Amazon lab126 `2.6.31-rt11-lab126` |
| Bootloader | U-Boot 2009.08-lab126 (no secure boot; serial console) |
| Frontlight | misc device `/dev/yoshime_fl` (minor 162) |

The stock userland is uClibc, so **kindil is built as a static ARMv7 musl
binary**: no dynamic loader, no shared libraries, nothing to install.

The panel geometry is read from the kernel at runtime rather than hard-coded:
the Paperwhite 1's framebuffer is portrait, while the later Paperwhites are
landscape, and one code path serves both.

## Build

```bash
# The device binary (static ARMv7 musl)
nix build .#packages.armv7l-linux.kindil

# A native build, for running the tests and developing the UI
nix build .#kindil

# Run the unit tests
nix build .#checks.x86_64-linux.kindil-tests

# A dev shell with cargo, rustc, rustfmt and clippy
nix develop
```

## Design

The kernel keeps the parts that are genuinely hard - E Ink waveform and
ghosting management, the touch controller, the PMIC, frontlight PWM, suspend
and resume, and the Wi-Fi stack. Everything above that is Rust.

| Module | Responsibility |
|---|---|
| `epdc` | The Freescale EPDC ioctl ABI: waveform modes, update regions, the panel refresh handshake. |
| `fb` | Mapping `/dev/fb0`, reading the panel geometry and pixel format. |
| `input` | Touch events via evdev. |
| `render` | Text and shapes into the framebuffer. |
| `time` | The wall clock, without a date/time crate. |
| `shell` | Screens, transitions and update scheduling. |

### Why the ABI is spelled out by hand

The EPDC ioctl numbers encode the *size* of their argument struct, so a
mismatch between our layout and the kernel's is not a compile error - it is an
`ENOTTY` at runtime. The structs are therefore `#[repr(C)]` mirrors of the
kernel's `include/linux/mxcfb.h`, and their sizes and offsets are asserted in
tests against values measured from the real header.

Two details that are easy to get wrong, and that the tests pin down:

* `struct mxcfb_update_data` in the 2.6.31 kernel is **72 bytes** and still
  carries the Lab126 `hist_bw_waveform_mode` / `hist_gray_waveform_mode`
  fields. Later kernels dropped them and use different struct sizes, hence
  different ioctl numbers.
* `struct fb_fix_screeninfo` contains two `unsigned long` fields, so it is
  **68 bytes on the 32-bit ARM target** and 80 on a 64-bit host. We spell them
  as `u32` so the layout matches the target.

### Two device quirks that matter

* **The panel is inverted grayscale.** At 8bpp, `0` is black and `0xFF` is
  white. The driver flips the PxP LUT internally, so we write the panel's
  native encoding directly.
* **The waveform table starts out zeroed.** The driver maps the waveform we
  request through a table that we must install with
  `MXCFB_SET_WAVEFORM_MODES`. Without it, asking for `GC16` resolves to mode 0
  (`INIT`), which flashes the whole panel to white on every update.

### Touch coordinates need no correction

The `cyttsp` driver reports raw controller values with no rescaling, and on
the Paperwhite 1 they already land in the framebuffer's coordinate space:
the panel is 758x1024 portrait and the touch axes are X 0..758, Y 0..1024.
No axis swap or inversion is needed - which is the opposite of the Kindle
Touch, whose 600x800 panel *does* need one.

### The frontlight is a character device

Not a sysfs `brightness` node. On the Paperwhite 1 it is a misc device,
`/dev/yoshime_fl` (minor 162), driven by the ioctls in
`include/linux/yoshime_fl.h`. The same driver exposes an `fl_intensity`
attribute under the `fl_tps6116x` sysfs class, which we fall back to.

## Deploying

kindil is a single static binary. Two ways to run it:

### As a KUAL extension, alongside Amazon's UI

```bash
nix build .#packages.armv7l-linux.kindil
./scripts/make-kual-extension.sh
```

That writes a KUAL extension (a `config.xml` plus the binary) to
`result/kual-extension/`. Copy it onto the Kindle's user partition:

```bash
cp -r result/kual-extension/kindil /mnt/us/extensions/
chmod 755 /mnt/us/extensions/kindil/kindil
```

It then appears in the KUAL menu.

### As PID 1, replacing the Kindle OS

```bash
nix build .#packages.armv7l-linux.kindil
./scripts/make-initramfs.sh
```

That writes a minimal initramfs whose `/sbin/init` is kindil. Boot it with
`kexec`:

```sh
kexec -l /boot/uImage \
  --initrd=/mnt/us/initramfs.cpio.gz \
  --append='root=/dev/ram0 rootfstype=ramfs rdinit=/sbin/init'
```

This is the shape Quill OS uses on the same hardware family.

## Licence

MIT - see [LICENSE](LICENSE). The bundled 8x8 font (`src/font.rs`) is in the
public domain, from <https://github.com/dhepper/font8x8>.
