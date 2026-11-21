# Verification

What is proven about this crate, what is tested, and what is assumed.
CI (`.github/workflows/ci.yml`) runs all of it on every push.

## Proven: Kani

[Kani](https://github.com/model-checking/kani) is a bounded model checker
for Rust: a harness's inputs are symbolic, so a proof covers every value at
once (every f64 bit pattern, every enum variant), and Kani checks the
harness's assertions, and every panic, overflow and out-of-bounds access, on
all of them. Run them with:

```sh
cargo install --locked kani-verifier --version 0.68.0 && cargo kani setup
cargo kani -Z stubbing                       # all of them
cargo kani -Z stubbing --harness day_rules   # one
```

The harnesses are in `#[cfg(kani)] mod proofs` next to the code they prove.
Times are on a laptop (Apple M-series), Kani 0.68.0, CBMC 6.11; all 13 take
about 26 minutes one after another.

| Harness | Proves, for every input | Time |
| --- | --- | --- |
| `time::at_hours_is_total_and_near_the_date` | Any f64 hours (NaN, ±∞, any size) on any supported date: no time, or one within 72 h of that date's 00:00 UTC. No panic. | 9 s |
| `time::rounding_stays_within_a_minute` | Any moment within 5 days of the supported dates, any `Rounding`: `Nearest` moves it by −29..=30 s, `Up` by 0..=59 s, `None` not at all; the result is on a whole minute (but for `None`), rounding it again changes nothing, and it converts to a chrono `DateTime` (`Moment::utc` is total). | 390 s |
| `astronomy::unit::normalized_to_scale_is_in_range` | Any finite f64 normalizes into [0, max) for max 1, 24 and 360 (angles, fractions of a day). | 2 s |
| `astronomy::ops::days_since_solstice_is_a_day_of_the_year` | Any day 1..=366 of any year 1000..=3000, any latitude: a day of that year. | < 1 s |
| `astronomy::ops::julian_day_is_total` | Any year 1000..=3000, month, day: a finite Julian day, no overflow. | < 1 s |
| `models::coordinates::new_is_total` | Any two f64: `Ok` exactly when the latitude is in [−90, 90] and the longitude finite, then with the longitude in [−180, 180]; the matching error otherwise. | < 1 s |
| `models::parameters::angles_and_night_portions` | Any f64: a `DepressionAngle` exactly when in [0, 30]; any angle's night portion under any rule is in [0, 1/2]. | < 1 s |
| `schedule::day_rules_are_total_and_ordered` | `Day::from_events`, the rules that make a day's times from the sun's events, for any events (any moments within 5 days of the supported dates, each present or not), any supported date, any latitude and any `Parameters` (every method, madhab, high-latitude rule, shafaq, rounding, angle, Isha interval and adjustment): no panic; a day it makes has Fajr, Sunrise, Dhuhr, Asr, Maghrib, Isha in order at least 5 minutes apart, still strictly in order after rounding when there are no adjustments (rounding stubbed by any moment within its proven −29..=59 s); otherwise it is `NoAsr` or `TimesOutOfOrder` of that date. | 125 s |
| `schedule::night_times_are_in_order` | For any night (Maghrib before the next Fajr, any moments near the supported dates) and rounding: Maghrib ≤ the middle of the night ≤ the start of its last third ≤ the next Fajr, within the minute rounding moves them. | 498 s |
| `schedule::current_and_next_bracket_the_moment` | For any 8 times and any moment: `current`'s index is the last time at or before it, `next`'s the first after it, and each is `None` exactly when there is none. (`PrayerTimes::current`, `next` and `time_remaining` are this, on their times.) | 3 s |
| `schedule::latitudes_toward_the_equator_are_latitudes` | AqrabBalad's latitudes, from any latitude in any number of steps up to 180: valid latitudes, no farther from the equator, on the same side. | 1 s |
| `schedule::prayer_times_new_is_total` | `PrayerTimes::new`, the public calculation, for any date chrono has (supported or not), any coordinates, any parameters without a polar circle resolution, with the astronomy (`Events::new`) any events or either of its errors, and `Moment::utc` stubbed (proven total above): no panic, and `DateOutOfRange` outside the supported dates. | 545 s |
| `schedule::polar_circle_resolution_steps_are_total` | The polar circle resolutions' searches, for any supported date, coordinates and parameters, with each day they try any times or none (`Day::unresolved` stubbed): AqrabBalad tries at most 180 latitudes, and every try of either search (any of the 180 latitudes, any of the ±183 days) is panic-free; the searches around them are std's iterators over those ranges. | 5 s |

### What the proofs assume

- **The astronomy is taken as any value.** The floating-point astronomy
  (`SolarTime`, Meeus's series, the iteration to the sun's altitude) is
  replaced in the schedule harnesses by any events at all, and CBMC
  over-approximates `sin`, `cos`, `acos` and the like (any value in their
  range, or NaN). So the proofs show the code around the astronomy can't
  panic and keeps its invariants *whatever the astronomy returns*; that the
  astronomy is *right* is what the differential tests below are for. The
  astronomy itself has no panicking operation: it is f64 arithmetic, whose
  operations can't panic, and saturating casts.
- **Stubs.** `Events::new`, `Day::unresolved`, `Moment::utc` and
  `Moment::rounded` are stubbed (`#[kani::stub]`) where noted: the first two
  by any value of their result type; `Moment::utc` by a fixed time (its
  totality is `rounding_stays_within_a_minute`'s); `Moment::rounded` by any
  moment within the bounds that harness proves. Each stub is an
  over-approximation of what it replaces, so a proof with it holds without
  it; the stubbed functions are proven on their own. (They are stubbed
  because chrono's date arithmetic and 64-bit division are slow to verify.)
  Stubbing needs Kani's `-Z stubbing`.
- **Bounds.** Dates are 1000-01-01 to 3000-12-31 (`FIRST_DATE`,
  `LAST_DATE`; `PrayerTimes::new` returns `DateOutOfRange` outside them, and
  `prayer_times_new_is_total` covers every other date chrono has). Moments
  are within 5 days of those dates, which every moment the calculation makes
  is (solar events within 72 h of their date, shares of a night, i16 minutes
  of adjustment, AqrabYaum's 183 days moved by the same amount back).
- **Kani's models.** Kani's model of float `%` is imprecise (it reported a
  longitude wrap out of range that concrete execution doesn't produce), so
  the code wraps with `normalized_to_scale`, whose range is proven, instead.

## Tested

`cargo test --release --all-features`:

- **Unit and regression tests** (`src/`): one per fixed bug, which fails
  before its fix (see the git log), and Adhan's and salah's own examples.
- **Property tests** (`tests/properties.rs`, proptest, 40,000 cases each),
  with the real astronomy:
  - any f64 coordinates, any date chrono has, any parameters (angles, Isha
    intervals, adjustments, every method, rule, resolution, rounding), any
    moment and zone: no panic, in `new`, `current`, `next`,
    `time_remaining` and `on_local_date`;
  - without adjustments, the six times are in order, on whole minutes when
    rounded, Dhuhr within an hour of local mean noon (but for AqrabYaum),
    every time within a day of it (1900–2100, every latitude);
  - the next Fajr is the next day's own, and Maghrib, the middle of the
    night, its last third and the next Fajr are in order (strictly within
    60° unresolved; resolved polar days can borrow from different places);
  - every day within 60° has times for every method, and AqrabBalad finds
    times everywhere;
  - `current` and `next` bracket any moment, and `time_remaining` is the
    time to `next`;
  - `on_local_date`'s Dhuhr falls on the local date, for zones within 10 h
    of local mean time;
  - Qiblah bearings are in [0, 360).
- **Differential test against Adhan** (`tests/differential.rs`): adhan-js
  (the library salah ports; commit d2993d5, generated by
  `tests/fixtures/adhan/generate.ts`) for 29 places from 78°S to 78°N and
  both sides of the date line, every method, both madhabs, every
  high-latitude rule, shafaq and polar circle resolution, on 29 dates
  (2026, and 1990 to 2100): all six times and the middle and last third of
  the night **agree to the minute** (tolerance 0) on all 14,961 days both
  have times. The only other differences are by design and listed in the
  test: 15 days where salah has no times because the next day has none.
- **Differential test against NOAA** (`tests/noaa.rs`): an independent
  solar calculation, NOAA's (the equation of time rather than salah's right
  ascension and sidereal time), written in the test. Sunrise, transit and
  sunset from 65°S to 65°N, and 18° twilights from 45°S to 45°N, every 11th
  day of every 25th year from 1950 to 2150 on a 5° × 30° grid: 436,968
  times, **within 10 seconds (3 at worst)**, away from the edge of the sun's
  reaching an angle at all (where the time's dependence on the angle is
  unbounded).
- **no_std**: `cargo test --no-default-features --features libm`, a test
  that libm's functions agree with std's, and a build for
  `thumbv7em-none-eabihf`.

## Not covered

- The astronomy's accuracy far from 1950–2150: ΔT and the low-precision
  series' error grow with distance from 2000; the supported dates' first and
  last centuries aren't checked against a reference here.
- Whether a method's angles match an authority's published tables: methods
  are Adhan's, and agree with Adhan.
- Time zones and daylight saving: times are UTC; converting them is the
  caller's (`on_local_date` takes any chrono `TimeZone`).
