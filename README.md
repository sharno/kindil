# tawqit

[![CI](https://github.com/sharno/tawqit/actions/workflows/ci.yml/badge.svg)](https://github.com/sharno/tawqit/actions/workflows/ci.yml)

Islamic prayer times for Rust, verified. *Tawqīt* is the science of
timekeeping for the prayers.

- **A total API.** Every input is checked once, by its type's constructor;
  after that the calculation can't fail or panic. Nothing reads the clock.
- **Times say what they are.** Each is the sun's (`Time::Computed`), a
  rule's in its place (`Time::Fallback`, naming the rule), or missing with
  the reason (`Unavailable`); a day is complete (all six, in order) or
  partial.
- **Verified.** Everything after the astronomy is an integer core proven
  with [Verus](https://github.com/verus-lang/verus) for every input; the
  astronomy's conversions into it are proven with
  [Kani](https://github.com/model-checking/kani); the astronomy agrees with
  Adhan to the minute and with NOAA's solar calculator to seconds. See
  [VERIFICATION.md](VERIFICATION.md).
- **[jiff](https://github.com/BurntSushi/jiff)** types (`civil::Date`,
  `Timestamp`, `tz::TimeZone`, `SignedDuration`), and `no_std` (without an
  allocator) with the `libm` feature.

```toml
[dependencies]
tawqit = { git = "https://github.com/sharno/tawqit" }
```

## Usage

```rust
use jiff::civil::date;
use tawqit::{Coordinates, Day, Method, Outcome, Prayer, PrayerDate, Time};

let makkah = Coordinates::from_degrees(21.4225, 39.8262).expect("a place");
let today = PrayerDate::new(date(2026, 3, 20)).expect("a date in 1000-3000");
let parameters = Method::UmmAlQura.parameters();

match Day::calculate(today, makkah, &parameters) {
    Day::Complete(schedule) => {
        for prayer in Prayer::ALL {
            println!("{}: {}", prayer.name(), schedule.at(prayer));
        }
        // The mark at a moment, and the next one.
        let now = schedule.at(Prayer::Asr);
        assert_eq!(schedule.current(now), Some(tawqit::Mark::Prayer(Prayer::Asr)));
        assert_eq!(schedule.next(now), Some(tawqit::Mark::Prayer(Prayer::Maghrib)));
        println!("Maghrib in {}", schedule.until_next(now).unwrap());
    }
    Day::Partial(partial) => {
        // Near the poles: each prayer is there, or says why not.
        for prayer in Prayer::ALL {
            match partial.outcome(prayer) {
                Outcome::Time(Time::Computed(at)) => println!("{prayer:?} {at}"),
                Outcome::Time(Time::Fallback { at, rule }) => println!("{prayer:?} {at} ({rule:?})"),
                Outcome::Unavailable(why) => println!("{prayer:?}: {why:?}"),
            }
        }
    }
}
```

Times are UTC instants; show them in the place's zone. `Day::calculate`
takes the solar day of a UTC date (its noon near 12:00 local mean time);
where a zone keeps far from its meridian, `Day::on_local_date(date, &zone,
…)` takes the day whose noon falls on the local date.

### Settings

`Method::X.parameters()` is a preset (each is a `const` in `presets`);
change it with the `with_` methods, or build your own with
`Parameters::new(fajr_angle, isha)`. Every value is a checked type, so
every `Parameters` is valid.

```rust
use tawqit::*;

let minutes = |m| MinuteOffset::new(m).expect("within 12 hours");
let parameters = Method::MuslimWorldLeague
    .parameters()
    .with_madhab(Madhab::Hanafi)
    .with_high_latitude_rule(HighLatitudeRule::SeventhOfTheNight)
    .with_polar_circle_resolution(PolarCircleResolution::AqrabBalad)
    .with_offsets(Offsets { isha: minutes(2), ..Offsets::ZERO })
    .with_rounding(Rounding::Up);

let custom = Parameters::new(
    DepressionAngle::new(16.5).expect("0 to 30 degrees"),
    Isha::Interval(IshaInterval::new(75).expect("5 to 240 minutes")),
);
assert_ne!(parameters, custom);
```

| Setting | Values |
| --- | --- |
| Fajr | `DepressionAngle`: (0°, 30°], to a hundredth |
| `Isha` | `Angle(DepressionAngle)` or `Interval(IshaInterval)` (5–240 minutes after sunset) |
| `Maghrib` | `Sunset`, or `Angle(DepressionAngle)` (Tehran), which gives way to sunset if it would crowd Isha |
| `Madhab` | `Shafi` (Asr shadow 1×), `Hanafi` (2×) |
| `NightBound` | `HighLatitude(rule)`: Fajr no earlier and Isha no later than `MiddleOfTheNight`, `SeventhOfTheNight` or `TwilightAngle` (angle / 60) of the night; or `MoonsightingSeasons(Shafaq)` |
| `PolarCircleResolution` | `Unresolved` (a `PartialDay`), `AqrabBalad` (the nearest latitude with times), `AqrabYaum` (the nearest day) |
| `Offsets` | `MinuteOffset`s (±12 hours) per prayer: the method's and yours |
| `Rounding` | `Nearest`, `Up`, `Off` (to the second) |

| Method | Fajr | Isha | Notes |
| --- | --- | --- | --- |
| `MuslimWorldLeague` | 18° | 17° | |
| `Egyptian` | 19.5° | 17.5° | |
| `Karachi` | 18° | 18° | |
| `UmmAlQura` | 18.5° | 90 min | Add 30 minutes to Isha in Ramadan. |
| `Dubai` | 18.2° | 18.2° | Sunrise −3, Dhuhr, Asr, Maghrib +3 minutes. |
| `MoonsightingCommittee` | 18° | 18° | Seasonal twilights; a seventh of the night above 55°N. |
| `NorthAmerica` | 15° | 15° | ISNA. |
| `Kuwait` | 18° | 17.5° | |
| `Qatar` | 18° | 90 min | |
| `Singapore` | 20° | 18° | Rounds up. |
| `Tehran` | 17.7° | 14° | Maghrib at 4.5°. |
| `Turkey` | 18° | 17° | An approximation of Diyanet's. |

### Qibla

```rust
use tawqit::{Coordinates, Qiblah};

let paris = Coordinates::from_degrees(48.8566, 2.3522).expect("a place");
assert!((Qiblah::new(paris).degrees() - 119.163).abs() < 0.001);
```

### Features

- `std` (default). Without it the crate is `no_std` and needs `libm`:
  `default-features = false, features = ["libm"]`.
- `serde`: serialization; deserializing checks every value.

## Origins

tawqit began from [salah](https://github.com/insha/salah) by Farhan Ahmed
(MIT), itself a port of [Adhan](https://github.com/batoulapps/Adhan) by
Batoul Apps. It keeps their astronomy, from Jean Meeus's *Astronomical
Algorithms*, follows Adhan's current algorithms, and is otherwise its own:
a new API, every known bug of salah fixed (the git history has each fix
with its test), and the verification above.

It is maintained with the help of an AI coding assistant; every change
carries a test, and the proofs and checks in CI gate each commit.

## License

MIT; see [LICENSE](LICENSE).
