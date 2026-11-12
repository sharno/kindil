# Third-party components

Cargo.lock and flake.lock identify the exact dependency graph. This list is not
a replacement for the full transitive license inventory when distributing builds.

| Component | Source / license | Role |
|---|---|---|
| Slint 1.18.1 | https://github.com/slint-ui/slint — alternative GPL-3.0-only / Slint terms | UI, widgets, shaping, software rendering |
| Slint virtual-keyboard example | Slint `v1.18.1`, `examples/virtual_keyboard/ui` — MIT | Adapted touch keyboard; source, attribution and license in `ui/vendor/slint-keyboard` |
| slint-backend-kindle 0.7.1 | https://github.com/sverrejb/slint-kindle-backend — MIT OR Apache-2.0 | Kindle display/input backend, vendored in `vendor/` with a full-refresh patch |
| Miqat | pinned `crates/miqat` submodule — MIT | Unmodified dashboard layout, data and fonts |
| chrono-tz | https://github.com/chronotope/chrono-tz — MIT OR Apache-2.0 | IANA timezones for the clock |
| clap | https://github.com/clap-rs/clap — MIT OR Apache-2.0 | Command line |
| rbook | https://github.com/DevinSterling/rbook — Apache-2.0 | EPUB parsing/reading order |
| html2text | https://github.com/jugglerchris/rust-html2text — MIT | XHTML-to-text conversion |
| IBM Plex Sans Arabic | see `crates/miqat/assets/fonts/OFL.txt` — SIL OFL 1.1 | All typography: Miqat, the shell and the reader |

The small PW1 frontlight adapter follows Amazon's `include/linux/yoshime_fl.h`
and `drivers/misc/yoshime_fl.c` ioctl ABI. It calls the existing kernel driver;
it does not drive PWM, change voltage limits, or replace a kernel module.

kindil runs on Amazon's stock OS and uses its services (wifid, powerd, the
USBNetwork hack's Dropbear). Syncthing, if used, is a separately supplied build
under its own license.
