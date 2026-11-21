# Salah (salah-rs fork)

[![CI](https://github.com/sharno/salah-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/sharno/salah-rs/actions/workflows/ci.yml)

Islamic prayer times for Rust: a maintained fork of
[insha/salah](https://github.com/insha/salah) (MIT, by Farhan Ahmed), itself a
port of [Adhan](https://github.com/batoulapps/Adhan).

## Why this fork

salah 0.7 panics on ordinary inputs: where the sun never reaches a twilight
angle (most of Europe around the June solstice), where it never rises or sets
(past the polar circles), on any time that rounds up to 24:00 UTC, on
`current()` for any day but today. This fork keeps salah's calculations and
attribution, and changes:

- **A total API.** No input panics: the calculation returns `Result` with a
  typed `Error`, coordinates and parameters are checked types, and nothing
  reads the clock.
- **Every known bug fixed**, each in its own commit with a regression test
  (listed below).
- **Verified.** Kani proofs that the API can't panic and that its results
  keep their order, property tests over wide input ranges, and differential
  tests against Adhan and NOAA's solar calculator; see
  [VERIFICATION.md](VERIFICATION.md).
- **Adhan's current algorithms**: the converged sunrise and twilight
  iteration, the Asr checks, the order guard and the polar circle
  resolutions of adhan-js, whose times it matches to the minute.
- **`no_std`** (without std or an allocator), with the `libm` feature.

It is not published to crates.io (`salah` there is the original). Use it from
git:

```toml
[dependencies]
salah = { git = "https://github.com/sharno/salah-rs" }
```

This fork is maintained with the help of an AI coding assistant; every change
carries a test, and the proofs and checks in CI gate each commit.

### What changed from salah 0.7

Fixed (upstream issue or PR where there is one):

- No times where the sun doesn't rise or set: `Error::SunNeverRises` /
  `SunNeverSets` instead of a panic (#20), or times from the nearest latitude
  or day with `PolarCircleResolution::AqrabBalad` / `AqrabYaum` (PR #21).
- A twilight the sun never reaches gives way to the high-latitude rule (#18,
  fixed upstream too).
- A time that rounds up to 24:00 UTC is the next day's 00:00 (PR #26).
- `current`, `next` and `time_remaining` take the time and return `Option`:
  no panic before Fajr (PR #23) or on another day (#17); `Prayer::name` no
  longer reads the clock to call Dhuhr Jumua.
- Rounding happens once, at the end: `Rounding::Up` is a ceiling (Singapore's
  times were often a minute late) and `Rounding::None` keeps the seconds.
- Noon near the date line is on the date (Suva's times were a day late in
  October and November); `PrayerTimes::on_local_date` gives a local day's
  times where the zone is far from its meridian (Kiritimati).
- Sunrise, sunset and twilights converge (one Newton step was up to 34
  minutes off near the polar circles).
- Asr uses the noon declination, and is an error where the sun can't reach it
  (it was Dhuhr's time); times crowding near the polar circles are an error.
- Tehran's 4.5° Maghrib angle is used (it was ignored).
- The Moonsighting Committee's seasons in the southern hemisphere (an overflow,
  and up to half an hour off).
- The recommended high-latitude rule applies south of 48°S.
- The next Fajr is the next day's own (it was unrounded); the middle of the
  night is public.
- Qiblah bearings stay in [0, 360).
- Dates outside 1000–3000 are an error, not a panic; `Rounding`, `Shafaq` and
  `Qiblah` are exported; `no_std` (#16).

Changed API: `PrayerTimes::new(date, coordinates, parameters) -> Result`
replaces the `PrayerSchedule` builder; `Coordinates::new` and
`DepressionAngle::new` return `Result`; `Parameters` has `isha: Isha` (an
angle or an interval), `maghrib_angle: Option<_>`, `polar_circle_resolution`,
and `i16` adjustments.

## Usage

```rust
use salah::prelude::*;

let makkah = Coordinates::new(21.4225, 39.8262).expect("valid coordinates");
let date = NaiveDate::from_ymd_opt(2026, 3, 20).expect("a date");
let params = Configuration::with(Method::UmmAlQura, Madhab::Shafi);

match PrayerTimes::new(date, makkah, params) {
    Ok(times) => {
        for prayer in [Prayer::Fajr, Prayer::Sunrise, Prayer::Dhuhr, Prayer::Asr, Prayer::Maghrib, Prayer::Isha] {
            println!("{}: {}", prayer.name(), times.time(prayer).format("%H:%M UTC"));
        }
        // The prayer at a moment, and the next one.
        let now = times.time(Prayer::Asr);
        assert_eq!(times.current(now), Some(Prayer::Asr));
        assert_eq!(times.next(now), Some(Prayer::Maghrib));
        println!("Maghrib in {} minutes", times.time_remaining(now).unwrap().num_minutes());
    }
    Err(error) => println!("No prayer times: {error}"),
}
```

Times are `DateTime<Utc>`; show them in the place's time zone (with
`chrono-tz`, say). `PrayerTimes::new` takes the solar day of a UTC date, whose
noon is near 12:00 local mean time; where a zone keeps far from its meridian,
`PrayerTimes::on_local_date(date, &zone, ..)` takes the day whose Dhuhr falls
on the local date.

### Coordinates

Latitude in [-90°, 90°] and a finite longitude (wrapped into [-180°, 180°]),
north and east positive. Anything else is a `CoordinatesError`.

### Configuration

`Configuration::with(method, madhab)` gives a method's `Parameters`, whose
fields can then be changed; `Configuration::new(fajr_angle, isha)` builds custom
ones.

| Field                     | Meaning                                                                                         |
| ------------------------- | ----------------------------------------------------------------------------------------------- |
| `method`                  | The preset these came from (`Other` for custom ones).                                           |
| `fajr_angle`              | The sun's depression for Fajr: a `DepressionAngle`, 0° to 30°.                                  |
| `maghrib_angle`           | `Some(angle)` for Maghrib at that depression rather than at sunset (Tehran).                    |
| `isha`                    | `Isha::Angle(angle)`, or `Isha::Interval(minutes)` after sunset.                                |
| `madhab`                  | `Shafi` (shadow 1×) or `Hanafi` (2×), for Asr.                                                  |
| `high_latitude_rule`      | The bounds on Fajr and Isha where twilight lasts long (below).                                  |
| `polar_circle_resolution` | What to do on a day without times near the poles (below).                                       |
| `adjustments`             | `TimeAdjustment`: minutes added to each time.                                                   |
| `rounding`                | `Nearest` minute (default), `Up` to the next minute, or `None` (to the second).                 |
| `shafaq`                  | The Moonsighting Committee's twilight for Isha: `General`, `Ahmer` (red) or `Abyad` (white).    |

**Methods**

| Value                   | Fajr  | Isha            | Notes                                                                    |
| ----------------------- | ----- | --------------- | ------------------------------------------------------------------------ |
| `MuslimWorldLeague`     | 18°   | 17°             |                                                                          |
| `Egyptian`              | 19.5° | 17.5°           |                                                                          |
| `Karachi`               | 18°   | 18°             |                                                                          |
| `UmmAlQura`             | 18.5° | 90 min          | Add 30 minutes to Isha in Ramadan.                                       |
| `Dubai`                 | 18.2° | 18.2°           | Sunrise −3, Dhuhr, Asr, Maghrib +3 minutes.                              |
| `MoonsightingCommittee` | 18°   | 18°             | Seasonal twilights; a seventh of the night above 55°N.                   |
| `NorthAmerica`          | 15°   | 15°             | ISNA.                                                                    |
| `Kuwait`                | 18°   | 17.5°           |                                                                          |
| `Qatar`                 | 18°   | 90 min          |                                                                          |
| `Singapore`             | 20°   | 18°             | Rounds up.                                                               |
| `Tehran`                | 17.7° | 14°             | Maghrib at 4.5°.                                                         |
| `Turkey`                | 18°   | 17°             | An approximation of Diyanet's.                                           |
| `Other`                 | 0°    | 0°              | Placeholders, for custom parameters.                                     |

**High-latitude rules**: `MiddleOfTheNight` (default): Fajr no earlier and Isha
no later than the middle of the night; `SeventhOfTheNight`: no earlier than
the last seventh, no later than the first; `TwilightAngle`: angle / 60 of the
night. `HighLatitudeRule::recommended(coordinates)` is the seventh beyond 48°
north or south.

**Polar circle resolutions**: `Unresolved` (default): the `Error` that says why
there are no times; `AqrabBalad`: the times of the nearest latitude toward
the equator that has them; `AqrabYaum`: those of the nearest day that has
them.

### Qibla direction

```rust
use salah::prelude::*;

let paris = Coordinates::new(48.8566, 2.3522).expect("valid coordinates");
let qiblah = Qiblah::new(paris);
assert!((qiblah.value() - 119.163).abs() < 0.001); // degrees from true north
```

### Features

- `std` (default). Without it the crate is `no_std` and needs `libm`:
  `default-features = false, features = ["libm"]`.
- `serde`: serialization; deserializing checks coordinates and angles.

## Contributing

Please see the `CONTRIBUTING.md` file for more information.

## Code of Conduct

Our contributor code of conduct can be found in the `code-of-conduct.md` file.

## Acknowledgement

This library is based on the [Adhan](https://github.com/batoulapps/Adhan) library by Batoul Apps. All astronomical calculations are high precision equations directly from the book [Astronomical Algorithms](http://www.willbell.com/math/mc1.htm) by Jean Meeus.

## License

Salah is available under the MIT License. The full license text can be found in the `LICENSE` file.
