# Verification

What is proven about tawqit, what is only tested, and how the parts
connect. CI (`.github/workflows/ci.yml`) runs all of it on every push.

## The layers

```text
 inputs ──▶ checked types ──▶ float astronomy ──▶ conversions ──▶ integer core ──▶ jiff types
 (f64, jiff  Latitude, …,      Meeus, Adhan's      hours → s,       Verus: every     Timestamp,
  Date)      PrayerDate        root finder         seasons → s,     function proven  Day, Schedule
             (Kani; dates:     (tested: Adhan,     shares exact     for all inputs   (jiff's range
              exhaustive)       NOAA)              (Kani)                            guarantee)
```

1. **Checked types** (`geo`, `parameters`, `date`): one fallible
   constructor each; Kani proves each accepts exactly its range.
2. **Float astronomy** (`astronomy`): sunrise, transit, sunset, the sun at
   an angle, Asr's shadow, the Moonsighting seasons, the Qiblah. Not
   proven correct; tested against Adhan and NOAA (below).
3. **Conversions** (`observe`, `astronomy::ops`): every number the core
   gets from floating point passes through one of them, and Kani proves
   each lands within the core's preconditions.
4. **Integer core** (`src/verified.rs`): everything from the events in
   whole seconds to the times shown, proven with Verus for every input that
   meets its preconditions.
5. **jiff at the edges** (`schedule`): seconds to `Timestamp` (inside
   jiff's guaranteed range), the public types; Kani proves the glue between
   the proven parts can't panic.

### The trusted boundary, exactly

- **Verus's erasure.** `src/verified.rs` is written inside `verus!`; Verus
  reads its specs and proofs, and a normal build erases them through the
  same macro (`verus_builtin_macros`, pinned to the build of the Verus
  release used) and compiles the remaining Rust. The proven functions
  *are* the functions that run; there is no second copy.
- **Preconditions at the boundary.** Verus proves each core function for
  inputs meeting its `requires`; a normal build doesn't check them at run
  time. The callers outside the core meet them by construction, and Kani
  proves the parts that aren't plain construction:
  | Core precondition | Established by |
  | --- | --- |
  | events within ±`EVENT_BOUND` (4 days) of their date's 00:00 UTC | `observe::seconds` (Kani: `seconds_are_within_the_event_bound`) |
  | the transit within the bound | `observe::transit`: `seconds` of the transit, or else local mean noon, in range for any longitude by construction (that the stand-in is never used is tested, not proven: every Dhuhr in the differential tests is Adhan's) |
  | seasons within [0, `SEASON_BOUND`] | Kani: `seasons_are_within_the_bound` |
  | shares of the night ≤ ½, denominators ≤ 6000 | `HighLatitudeRule::share` (1/2, 1/7, angle/6000 for angles ≤ 3000 hundredths: Kani `angle_new_is_total`) |
  | Isha intervals 5–240 minutes | `IshaInterval`'s constructor |
  | offsets within ±1440 minutes | `MinuteOffset` (±720) × 2 (Kani: `offsets_are_within_the_core_bound`) |
  | a valid date in years 999–3001 | `Civil` (its Verus type invariant; built only by `Civil::new`, `next`, `previous`); `PrayerDate::new` accepts exactly the supported jiff dates (tested on every one of the 730,485: the domain is finite) |
  | seconds within ±`MAX_SECONDS` are a `Timestamp` | jiff's semver guarantee for `from_second` inside `Timestamp::MIN..=MAX`; a unit test pins the bounds (below) |
- **The tools**: Verus (and Z3), Kani (and CBMC), rustc.

## Proven: Verus (the integer core)

`verus src/verified.rs --crate-type=lib` (Verus 0.2026.11.02.8bf68ee):
77 items, 1.5 seconds. Unbounded: every input meeting the preconditions,
no bounded model.

| Function or lemma | Proves |
| --- | --- |
| `is_leap`, `month_length` | equal the Gregorian rules (`leap`, `days_in_month`) |
| `day_of_year_of` | the day of the year, in 1..=365/366 |
| `day_number_of` | days since 1970-01-01, by the spec (`day_number`), in range |
| `lemma_day_number_successor` | the day after a date is a date, and its day number is one more: day numbers count days |
| `lemma_epoch` | 1970-01-01 is day 0 |
| `Civil` (type invariant), `new`, `next`, `previous`, `number`, `day_of_year` | a `Civil` is always a valid date; `new` accepts exactly the valid ones; `next`/`previous` are the next/previous date (`next_date`, `previous_date`), a day number apart, None only at the ends |
| `days_since_solstice` | exactly `(day − solstice offset) mod year length`, a day of the year |
| `round` | exactly `round_spec`: nearest minute (half up), ceiling, or none |
| `lemma_round_bounds` | Nearest moves by −29..=30 s, Up by 0..=59 s, onto a minute; Exact not at all |
| `lemma_round_idempotent` | rounding twice is rounding once |
| `lemma_round_monotone`, `lemma_round_minute` | rounding keeps order; a minute stays |
| `absolute` | `day × 86400 + offset`, within ±`MAX_SECONDS` |
| `day_and_second` | a time's day and second of day (0..86400): the day rolls over at 24:00 |
| `add_minutes` | exactly `offset + 60 × minutes`, no overflow |
| `share_seconds`, `share_seconds_up` | ⌊n·share⌋ and ⌈n·share⌉, within [0, n] |
| `fajr_of`, `isha_of`, `maghrib_of` | exactly the rules (`fajr_spec`, …): the sun's time unless outside the night's bound or within 5 minutes of sunrise/sunset (then `Fallback` with the rule that gave it), Isha by interval, Maghrib's angle only after sunset and 5 minutes before Isha (else `Fallback(Sunset)`), `Missing` with the reason when an event isn't |
| `in_order` | true exactly when consecutive times are ≥ 5 minutes apart |
| `resolve` | the day's times by the rules; `Ordered` exactly when all six are there in order 5 minutes apart; otherwise `Partial`, as the rules leave them, or all `Missing(Order)` when all are there but out of order; every time within its bound |
| `finish` | each time with its offsets added and rounded, exactly |
| `lemma_finish_keeps_order` | without offsets, rounding keeps an ordered day in order (≥ 4 minutes apart) |
| `night_times` | the night's middle and last third, exactly (floored to the second, then rounded); for a night that doesn't run backwards, with Maghrib and Fajr on a minute (as rounded times are): Maghrib ≤ middle ≤ last third ≤ next Fajr |
| `latest_at_or_before`, `earliest_after` | exactly the latest time at or before the moment (the last of equals) and the earliest after it (the first of equals), None exactly when there is none |

## Proven: Kani (the boundaries)

`cargo kani -Z stubbing` (Kani 0.68.0, CBMC 6.11). Kani is a bounded model
checker: each harness's inputs are symbolic (every f64 bit pattern, every
enum variant), and it checks the assertions and every panic, overflow and
out-of-bounds access. It over-approximates `sin`, `cos`, `acos` and the like
(any value in their range, or NaN).

| Harness | Proves, for every input | Time |
| --- | --- | --- |
| `observe::seconds_are_within_the_event_bound` | Any f64 hours (NaN, ±∞, any size): no time, or whole seconds within the core's ±`EVENT_BOUND` | 1 s |
| `astronomy::ops::seasons_are_within_the_bound` | The Moonsighting Committee's seasonal twilights, any latitude, any day since the solstice, any shafaq: whole seconds in [0, `SEASON_BOUND`] | 117 s |
| `astronomy::ops::julian_day_is_exact` | The Julian day of every day number the core makes is exact (the float layer's one conversion from the calendar) | 1 s |
| `astronomy::unit::normalized_to_scale_is_in_range` | Any finite f64 normalizes into [0, max) for max 1, 24, 360 | 2 s |
| `geo::latitude_new_is_total`, `geo::longitude_new_is_total` | Any f64: a latitude exactly when in [−90, 90]; a longitude exactly when finite, then in [−180, 180] | 1 s each |
| `geo::latitudes_toward_the_equator_are_latitudes` | AqrabBalad's latitudes, any number of steps from any latitude: latitudes, no farther from the equator, on its side; at most 180 steps | 1 s |
| `parameters::angle_new_is_total` | Any f64 is an angle exactly when it rounds into (0, 30] hundredths; its share of the night is at most a half (the core's precondition) | 1 s |
| `parameters::offsets_are_within_the_core_bound` | Any parameters' offsets (method's + user's) are within ±1440 minutes (the core's precondition) | 1 s |
| `schedule::polar_circle_resolution_steps_are_total` | Every step of AqrabBalad's and AqrabYaum's searches, any date, place, parameters, and any observation (stubbed): no panic | 12 s |
| `schedule::todays_times_are_total` | A day's times after the astronomy, any place, any parameters (every polar circle resolution), any observation: the rules' result, the polar search's, offsets and rounding, absolute seconds, the public outcomes: no panic | 17 s |
| `schedule::night_is_total` | A complete day's night (the next day's times, its Fajr, the night's marks), as above: no panic | 15 s |

All 12 take 2 minutes 47 seconds one after another.

The last two harnesses together cover `Day::calculate`'s glue: `day_of`
only matches on what they return. Their date is fixed: it enters the glue
only through `Civil::number`, `next` and `absolute`, which Verus proves for
every date. Stubs (Kani's `-Z stubbing`) replace, in the harnesses that say
so: the float layer (`Observed::new`) by any observation meeting the core's
preconditions (which the conversion harnesses prove it does); the core's
`resolve`, `round` and `night_times` by any result their Verus
postconditions allow; the polar searches by any result (their every step is
`polar_circle_resolution_steps_are_total`'s); `timestamp` by a fixed
instant. Each stub
over-approximates what it replaces, so a proof with it holds without it.
Where Kani would have to re-prove what Verus proves (64-bit division, the
date arithmetic, and loops it unrolls even on paths ruled out make that
slow), the harness takes the proven contract instead.

What Kani doesn't cover, and how it's covered instead:
- `timestamp` (seconds to `jiff::Timestamp`) never fails: the core's
  seconds are within ±4·10¹⁰, inside `Timestamp::MIN..=MAX`, where jiff
  guarantees `from_second` succeeds (its docs: "a semver guarantee"); a unit
  test pins the bounds. A harness over jiff's internals didn't finish in 15
  minutes.
- `Schedule::current`, `next` and `until_next`: their searches are Verus's
  (`latest_at_or_before`, `earliest_after`); around them, a `match` over
  the marks and `.get` (a harness over symbolic jiff instants didn't
  finish in five minutes). The property tests run them over every moment
  of 40,000 days.
- `PrayerDate::new` accepts exactly the supported dates: the domain is
  finite, and a unit test goes through all 730,485 dates (with the
  calendar's agreement with jiff).

## Tested

`cargo test --release --all-features`:

- **Regression tests** (`tests/regressions.rs`): one per fixed bug, each of
  which failed before its fix (the git history has them), and the reference
  times salah and Adhan were checked against.
- **Property tests** (`tests/properties.rs`, proptest, 40,000 cases each,
  the real astronomy): no panic for any f64 coordinates, any date jiff has,
  any parameters, moment and zone; a complete day's six times in order (no
  offsets), on minutes when rounded, Dhuhr near local mean noon; a partial
  day missing something; the night in order and its next Fajr the next
  day's own; every day within 60° complete, every day with AqrabBalad
  complete; current and next bracketing any moment; a local date's Dhuhr
  on it; Qiblah bearings in [0, 360).
- **The calendar against jiff**, every date 1000–3000 (`date.rs`).
- **Differential test against Adhan** (`tests/differential.rs`): adhan-js
  at commit d2993d5 (`tests/fixtures/adhan/generate.ts`), 29 places from
  78°S to 78°N and both sides of the date line, every method, madhab,
  high-latitude rule, shafaq and polar circle resolution, on 29 dates:
  **14,976 days with all six times and the night equal to the minute**
  (tolerance 0); the 1,003 days Adhan has no times for are partial days
  here.
- **Differential test against NOAA** (`tests/noaa.rs`): NOAA's solar
  calculator, written in the test (the equation of time rather than right
  ascension and sidereal time): sunrise, transit, sunset (65°S–65°N) and
  18° twilights (45°S–45°N), 1950–2150: **436,968 times within 10 s, 3 s at
  worst**.
- **no_std**: a build for `thumbv7em-none-eabihf` without std or an
  allocator, the tests on the libm path, and a test that libm agrees with
  std.

## Not covered

- The astronomy's correctness, beyond the differential tests: Kani proves
  only that it can't panic (f64 arithmetic can't) and that what leaves it
  is in range.
- Accuracy far from 1950–2150 (ΔT and the series' error grow with distance
  from 2000).
- Whether a method's angles match an authority's tables: methods are
  Adhan's, and agree with Adhan.
- Time zones: times are UTC; `Day::on_local_date` takes any jiff
  `TimeZone`, tested with fixed offsets.
