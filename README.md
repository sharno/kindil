<p align="center">
  <img src="assets/logo.svg" alt="kindil's lantern logo" width="160">
</p>

<h1 align="center">kindil · قنديل</h1>

<p align="center">
  Turn a first-generation Kindle Paperwhite into a prayer-time wall
  dashboard and e-reader.
</p>

## Why

The 2012 Kindle Paperwhite has a lovely e-ink screen, a front light and Wi-Fi,
and Amazon no longer supports it. kindil gives it a second life as something
you hang on a wall: the next prayer and the time left until it, today's
prayer times, the weather and a rain radar, readable across a room, lit or
unlit, for days on one charge. It's also a simple reader for your own books.

kindil is one small program that takes over the Kindle while it runs. It
replaces Amazon's software for the screen, Wi-Fi, power and sleep, so
nothing of Amazon's store, sync or ads runs, and it never changes the
Kindle's system: exit kindil, or remove it, and the Kindle is stock again.

## Screenshots

<table>
<tr>
<td align="center"><img src="docs/screenshots/dashboard-miqat.png" alt="The prayer dashboard" width="240"><br><sub>Prayer dashboard</sub></td>
<td align="center"><img src="docs/screenshots/dashboard-miqat-rain.png" alt="The dashboard with the rain radar" width="240"><br><sub>Rain radar when rain is coming</sub></td>
<td align="center"><img src="docs/screenshots/dashboard-clock.png" alt="The clock home screen" width="240"><br><sub>Or just a big clock</sub></td>
</tr>
<tr>
<td align="center"><img src="docs/screenshots/library.png" alt="The library" width="240"><br><sub>Library</sub></td>
<td align="center"><img src="docs/screenshots/reader.png" alt="The reader" width="240"><br><sub>Reader</sub></td>
<td align="center"><img src="docs/screenshots/reader-panel.png" alt="The reader's quick panel" width="240"><br><sub>Light and text size</sub></td>
</tr>
<tr>
<td align="center"><img src="docs/screenshots/settings.png" alt="Settings" width="240"><br><sub>Settings</sub></td>
<td align="center"><img src="docs/screenshots/settings-prayer.png" alt="Prayer calculation methods" width="240"><br><sub>Prayer methods, with today's times</sub></td>
<td align="center"><img src="docs/screenshots/settings-location.png" alt="Location settings" width="240"><br><sub>Location</sub></td>
</tr>
<tr>
<td align="center"><img src="docs/screenshots/settings-wifi.png" alt="Wi-Fi settings" width="240"><br><sub>Wi-Fi</sub></td>
<td align="center"><img src="docs/screenshots/settings-about.png" alt="About" width="240"><br><sub>About</sub></td>
<td align="center"><img src="docs/screenshots/asleep.png" alt="The sleep screen" width="240"><br><sub>Asleep</sub></td>
</tr>
</table>

## Features

**Prayer dashboard**
- The next prayer and the time left until it, large enough to read across a
  room.
- Today's six times in Arabic and English, with the current one highlighted,
  and the Gregorian and Hijri dates.
- Twelve calculation methods (Muslim World League, ISNA, Egyptian, Umm
  al-Qura, Karachi, Dubai, Moonsighting Committee, Kuwait, Qatar, Singapore,
  Tehran, Turkey) and both Asr schools. Each choice shows today's times, so
  you can match your mosque's timetable.
- Weather now, the next six hours and tomorrow, from Open-Meteo, and a rain
  radar map that appears when rain is expected.
- Or a plain big clock as the home screen.

**Always on, for days**
- After two minutes untouched the light goes off and the Kindle sleeps
  between updates: it wakes each minute to move the clock and the countdown,
  redrawing only that strip, and every 30 minutes to fetch the weather.
- Keeps its own time: corrected after every sleep and checked against
  internet time servers when online.
- Charging is paused when the battery is too hot or cold; at 1% it powers
  off instead of draining flat.

**Reader**
- EPUB and plain text books from the Kindle's `books` folder.
- Tap or swipe to turn pages; your place in every book, the text size and
  the light are remembered.
- Rename books, or move them to a Trash you can undo.

**Set up on the Kindle itself**
- Location: search for a city, use your approximate location, or type
  coordinates. The clock's time zone follows.
- Wi-Fi: scan, join, and forget networks, with an on-screen keyboard.
- Front light, home screen, sleep when idle, start at boot.

**Looks after itself**
- Updates over Wi-Fi, signed, tried first and kept only if they run well.
- A crash restarts it; a version that keeps crashing makes way for the
  previous one, then for Amazon's own interface.
- Its own Wi-Fi, SSH server and power management, so Amazon's store, sync
  and background services don't run while it does.
- Never changes the Kindle's system: exit or remove it and the Kindle is
  stock.

## Install

You need:

1. A **first-generation Kindle Paperwhite** (2012). Its serial number
   (Settings → Device Info) starts with B024, B01B, B020, B01C, B01D or B01F.
   No other Kindle is supported.
2. A **jailbreak** and **KUAL**, done once on the Kindle. Amazon's software
   only runs programs Amazon signed; the jailbreak lets the Kindle run others.
   Follow the MobileRead community's guides:
   - Firmware up to 5.4.4.2: the
     [Touch/PW1/PW2 jailbreak](https://www.mobileread.com/forums/showthread.php?t=186645).
   - Firmware 5.6.1.1: [downgrade to 5.3.3](https://www.mobileread.com/forums/showthread.php?t=264432)
     first, then the jailbreak above. Read the threads through first: a
     firmware change can brick a Kindle.
   - Then [KUAL](https://www.mobileread.com/forums/showthread.php?t=203326),
     usually with [MRPI](https://www.mobileread.com/forums/showthread.php?t=251143).
3. A computer (macOS, Linux or Windows) with [Rust](https://rustup.rs), to run
   the installer. There are no prebuilt installer downloads yet.

Then:

```sh
git clone https://github.com/sharno/kindil && cd kindil
cargo run --release -p kindil-installer-gui
```

Plug the Kindle in with a USB cable and unlock it. The installer finds it,
checks it's a Paperwhite 1 with a jailbreak and KUAL (and says what's missing
if not), installs the latest signed release, and ejects it. Your settings are
kept, so the same button upgrades an existing install. (Prefer a terminal?
`cargo run --release -p kindil-installer -- install`.)

<p align="center">
  <img src="docs/installer.png" alt="The installer: a Kindle found, checked and ready to install" width="420">
</p>

On the Kindle, open **KUAL → kindil → Start kindil**, then set your place in
**Settings → Location**.

**Start at boot** (optional): to have the Kindle come up straight into kindil,
install the boot job once over SSH with
`sh /mnt/us/extensions/kindil/boot/install.sh install`, then choose a mode in
**Settings → Power → Start at boot**. It's the only system file kindil ever
adds, and if kindil doesn't stay up after a boot, the next boot returns to
Amazon's interface.

## Getting back to the stock Kindle

- **Settings → Power → Exit to Kindle** brings back Amazon's interface.
- Without start at boot, restarting the Kindle always returns it to stock.
- To remove kindil, delete the `extensions/kindil` and `kindil` folders from
  the Kindle over USB (and, if you installed it,
  `sh /mnt/us/extensions/kindil/boot/install.sh remove` first).

## More

- [How kindil works](docs/HOW-IT-WORKS.md): every feature in detail, updates,
  start at boot, and the files it keeps on the Kindle.
- [Developing kindil](docs/DEVELOP.md): building, testing, installing over
  SSH and releasing.
- The dashboard is [Miqat](https://github.com/sharno/miqat), carried here as a
  library.

## License

MIT. Slint is GPL-3.0 / Slint licensed; see [THIRD_PARTY.md](THIRD_PARTY.md).
