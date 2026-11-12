# 2026-11 architecture audit

The previous implementation's host builds and test counts overstated what had
been established. This refactor keeps the useful project intent while replacing
the riskiest duplication.

| Finding | Resolution |
|---|---|
| Handwritten libc layouts confused C `long`, modern musl `time_t`, and old-kernel evdev records | Remove the application's raw display/input/libc stack; use std/libc and pinned community backend |
| “Waveform defaults are zero” inferred without reading the probe initializer | Remove identity-table installation; preserve matched kernel/backend behavior |
| Initial tests copied the renderer instead of exercising it | Drive the real Slint component/software renderer |
| Timezone offsets passed as seconds at runtime but hours in prayer tests | Remove the parallel prayer implementation; reuse Miqat's existing `salah` configuration and renderer |
| Bitmap-font-only rendering displayed disconnected Arabic | Runtime vector fonts + contextual shaping; regression compares normal Arabic with ZWNJ-separated Arabic |
| Recreating Miqat would drift from the actual dashboard | Export Miqat's existing Rust engine as a library and pin its repository as a submodule |
| Full-screen clock repaint after every raw input record | Slint's event/dirty-region pipeline; Miqat image refresh on one background worker |
| A 74-minute source cross-toolchain build blocked iteration | Prebuilt Rust/ARM std and cached Zig; Cargo artifact reuse |
| GNU/glibc link driver was insufficient once `-ldl` appeared | Musl-aware Zig link driver; disable Rust's self-contained CRT when Zig supplies it, avoiding duplicate `_start` |
| An unbound Wi-Fi client could not receive replies reliably | Bind a unique local Unix datagram socket; verify the production command path with a fake server |
| Background/one-shot DHCP would fight supervision | Foreground udhcpc plus an explicit Rust DHCP hook; no shell interpolation of DHCP values |
| Services might start during ordinary host/stock-OS UI use | Start the supervisor only in explicit standalone PID-1 operation |
| SSH default policy lacked a complete runtime/account/PTY setup | Keys-only policy, devpts/device setup, explicit required runtime overlay; do not claim reachability without system tests |
| Initramfs scripts recursively removed caller-selected output directories | Fresh staging directory, refuse existing output, atomic publication, private archive permissions |
| Exiting PID 1 would panic the kernel | Keep PID 1 alive after UI exit or fatal initialization error |
| Large/invalid book files could exhaust a small device | Bounded UTF-8 reads, EPUB ZIP expansion preflight and library parsing; no archive extraction to disk |
| File cleanup could lose user data | Explicit rename and confirmed recoverable Trash; atomic no-clobber destination creation; undo |

## Established evidence

- Host and ARM `qemu-arm` regression tests exercise the real UI and native
  Miqat engine, including a pixel-for-pixel dashboard assertion.
- Arabic joining, reading, font/frontlight controls, EPUB parsing/expansion
  limits, Wi-Fi socket replies and basic file operations have regression tests.
- The device artifact is checked for ARM32 and absence of an ELF interpreter.
- Dedicated CI access to private Miqat is a repository-scoped read-only deploy
  key, not a reused personal token.

## Not established

There has been no physical PW1 boot, panel/touch/frontlight test, WLAN firmware
bring-up, SSH reachability test, RTC wake test or measured battery/runtime/RSS
benchmark. QEMU user-mode uses the host kernel and cannot establish compatibility
with lab126 2.6.31. Native Miqat rendering is deliberately shared, but performance
of its weather/radar/TLS stack on the actual 800 MHz/256 MiB device is not yet
measured. These are release gates, not details to paper over with a successful
cross compile. See the README validation ladder before booting any image.
