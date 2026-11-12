# Release gates — PW1 / Celeste

Kindil is an experimental userland, not a finished, hardware-certified OS image.
The following boundaries must stay separate. This checklist does not authorize
writing a partition, replacing a bootloader, or stopping stock device services.

## Software gates

Run from a recursive checkout with access to the pinned private Miqat library:

```sh
nix develop
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets --features kindle -- -D warnings
scripts/dev.sh test-arm
scripts/dev.sh device
qemu-arm dist/kindil --preview dist/arm-preview --config examples/kindil.toml --books examples/books
bash scripts/test-initramfs.sh
```

The production Slint test verifies exact Miqat pixels, normal Arabic versus
deliberately ZWNJ-disconnected Arabic, reader reflow, touch dragging and page
reset, sliders, file operations, actual touch keyboard entry/cursor/backspace,
Arabic key entry, masked password entry and clearing, and touch-only rename.
Every printable ASCII key is checked against the keyboard model. Workflow
screenshots restore the normal header after the deliberately broken fixture.

For inspectable host test screenshots (fixture credentials only):

```sh
KINDIL_TEST_PREVIEW_DIR=dist/touch-tests cargo test --test slint_ui
```

These tests do not open a physical framebuffer or write to a physical frontlight.
The network form test intercepts its submission callback; socket protocol tests
use a fake local wpa_supplicant server. User-mode QEMU still uses the host kernel.

## Full-system emulator input gate — blocked on device files

Upstream [eink-emulator firmware documentation](https://github.com/katadelos/eink-emulator/blob/main/docs/firmware.md)
requires **PW1-specific `u-boot.bin`, `uImage` and a raw ext `rootfs.img`**.
Its docs explicitly say PW1 public updates omit U-Boot, so a firmware update
download alone is insufficient. Supply a legally obtained backup from the owned
PW1; do not borrow another Kindle model's bootloader, board data or waveforms.
Do not commit proprietary firmware or personal device images to either repo.

Preparation, once those files are available:

1. Use upstream's Nix development environment and its current `eink models`
   and `eink firmware` commands to confirm the PW1 model identifier and paths.
2. Put copies of the matching three files at the paths the tool reports.
3. Create a new emulator-only instance using the PW1/Celeste profile, not PW2.
   The emulator builds patched QEMU if needed; ordinary `qemu-arm` is not this
   machine emulator. Keep the source firmware backup unchanged.
4. First verify the supplied stock firmware boots in the emulator. Capture the
   serial kernel/version and framebuffer geometry; preserve the instance before
   experimenting. Use the emulator's own guest access and copied files only.
5. Test the ARM binary against the emulated Linux 2.6.31 kernel, starting with
   `--version` and `--preview` before trying the device backend. Record crashes,
   unsupported syscalls, peak RSS, rendering latency and serial logs.

Modern Rust/musl declares a newer Linux baseline. Passing the ARM instruction
and software-render tests does not remove this gate; a working old-kernel build
strategy must be demonstrated before a PW1 release.

## Matched standalone runtime gate

The staging script only checks archive/ELF/key prerequisites. An operational
runtime still needs validated PW1 kernel modules and EPDC waveforms, AR6003
firmware/calibration and interface setup, static ARM BusyBox/Dropbear/
wpa_supplicant, correct accounts and SSH keys, configuration, and an explicit
storage plan. No automatic eMMC mount or partition-writing scheme is supplied.
Helper binaries substituted in archive-contract tests are fixtures, not real
network/SSH programs or a usable runtime.

Confirm proc/sysfs/device nodes/devpts, DHCP addresses/routes/DNS, key-only SSH,
touch coordinates, framebuffer stride/updates, frontlight range, and basic book
and Trash workflows in the full-system environment. Archive success is not boot
success; emulator network success is not real AR6003 radio validation.

## Physical-device gate

Requires access to the owned PW1 and a verified recovery procedure, with 1.8 V
serial equipment if serial recovery is used. Agree on the reversible test plan
before issuing any device operation. No flash/eMMC/bootloader instructions are
included here. Validate the same interfaces on hardware, measure memory and
frame latency, and test disconnect/recovery paths.

Deep suspend, RTC wake and battery-life claims remain blocked on successful
hardware wake/recovery tests and measured current draw. Do not enable suspend
merely to make a power feature appear complete. Syncthing remains optional/off.
