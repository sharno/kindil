# Third-party components

Cargo.lock and flake.lock identify the exact dependency graph. This list is not
a replacement for the full transitive license inventory when distributing builds.

| Component | Source / license | Role |
|---|---|---|
| Slint 1.18.1 | https://github.com/slint-ui/slint — alternative GPL-3.0-only / Slint terms | UI, widgets, shaping, software rendering |
| slint-backend-kindle 0.7.1 | https://github.com/sverrejb/slint-kindle-backend — MIT OR Apache-2.0 | Experimental Kindle display/input backend |
| Miqat | pinned `crates/miqat` submodule — MIT | Unmodified dashboard layout, data and fonts |
| tz-rs | https://github.com/x-hgg-x/tz-rs — MIT OR Apache-2.0 | Timezone/TZif parsing |
| rbook | https://github.com/DevinSterling/rbook — Apache-2.0 | EPUB parsing/reading order |
| html2text | https://github.com/jugglerchris/rust-html2text — MIT | XHTML-to-text conversion |
| DejaVu Sans 2.37 | https://dejavu-fonts.github.io — see `assets/DejaVu-LICENSE.txt` | Shell/reader font |
| IBM Plex Sans Arabic | see `crates/miqat/assets/fonts/OFL.txt` — SIL OFL 1.1 | Miqat's exact typography |

The small PW1 frontlight adapter follows Amazon's `include/linux/yoshime_fl.h`
and `drivers/misc/yoshime_fl.c` ioctl ABI. It calls the existing kernel driver;
it does not drive PWM, change voltage limits, or replace a kernel module.

Runtime helpers are separately supplied, not bundled: Dropbear, wpa_supplicant,
BusyBox and optionally Syncthing retain their upstream licenses. Firmware and
waveforms must come from the matching owned device/firmware, not an arbitrary
other Kindle model.
