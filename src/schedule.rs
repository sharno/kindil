//! A day's prayer times: what each is (the sun's, a rule's, or missing and
//! why), and the steps that make them.
//!
//! The steps are types, each made only from the one before:
//!
//! ```text
//! Observed ──resolve──▶ Resolved::{Ordered, Partial} ──round──▶ Rounded ──▶ Day
//!  (float astronomy,      (the rules, the order guard,          (offsets and
//!   events in seconds)     polar resolution: verified core)      rounding, once)
//! ```
//!
//! so a schedule can't be read before its rules are applied, nor rounded
//! twice: only `Rounded` becomes a public [`Day`].

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::date::PrayerDate;
use crate::geo::{Coordinates, Latitude};
use crate::observe::Observed;
use crate::parameters::{NightBound, Parameters, PolarCircleResolution};
use crate::verified::{self, Civil, Missing, Resolved as Core, Rule, Slot};

/// The day's prayers (and sunrise), in order.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Prayer {
    Fajr,
    Sunrise,
    Dhuhr,
    Asr,
    Maghrib,
    Isha,
}

impl Prayer {
    pub const ALL: [Prayer; 6] = [
        Prayer::Fajr,
        Prayer::Sunrise,
        Prayer::Dhuhr,
        Prayer::Asr,
        Prayer::Maghrib,
        Prayer::Isha,
    ];

    fn index(self) -> usize {
        match self {
            Prayer::Fajr => 0,
            Prayer::Sunrise => 1,
            Prayer::Dhuhr => 2,
            Prayer::Asr => 3,
            Prayer::Maghrib => 4,
            Prayer::Isha => 5,
        }
    }

    /// The name, transliterated into English.
    pub fn name(self) -> &'static str {
        match self {
            Prayer::Fajr => "Fajr",
            Prayer::Sunrise => "Sunrise",
            Prayer::Dhuhr => "Dhuhr",
            Prayer::Asr => "Asr",
            Prayer::Maghrib => "Maghrib",
            Prayer::Isha => "Isha",
        }
    }

    /// The name on a day that is `weekday` where the prayer is: Dhuhr is
    /// Jumua on Fridays.
    pub fn name_on(self, weekday: Weekday) -> &'static str {
        match (self, weekday) {
            (Prayer::Dhuhr, Weekday::Friday) => "Jumua",
            (prayer, _) => prayer.name(),
        }
    }
}

/// Everything a complete day has a time for, in order: its six times, and
/// its night up to the next Fajr.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Mark {
    Prayer(Prayer),
    /// The middle of the night, Maghrib to the next Fajr.
    MiddleOfTheNight,
    /// The start of its last third (Qiyam).
    LastThirdOfTheNight,
    /// The next day's Fajr.
    NextFajr,
}

/// The rule that gave a time in place of the sun's.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Fallback {
    /// Fajr no earlier, or Isha no later, than this share of the night (the
    /// sun didn't reach the angle, or did outside the share, or within five
    /// minutes of sunrise or sunset).
    NightShare(crate::parameters::HighLatitudeRule),
    /// The Moonsighting Committee's seasonal twilight.
    MoonsightingSeason,
    /// The Moonsighting Committee's seventh of the night, above 55°N.
    MoonsightingSeventh,
    /// Maghrib at sunset: the Maghrib angle wasn't reached, or would crowd
    /// Isha.
    Sunset,
    /// The whole day's times are this latitude's (AqrabBalad).
    NearestLatitude(Latitude),
    /// The whole day's times are those of the day this many days away,
    /// moved to the date (AqrabYaum).
    NearestDay(i16),
}

/// A time a day has.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Time {
    /// As the method says: the sun at its angle or event, or an interval.
    Computed(Timestamp),
    /// From a rule, in place of the sun's (it says which).
    Fallback { at: Timestamp, rule: Fallback },
}

impl Time {
    pub fn at(&self) -> Timestamp {
        match self {
            Time::Computed(at) | Time::Fallback { at, .. } => *at,
        }
    }
}

/// Why a day has no time for a prayer.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Unavailable {
    /// The sun stays below the horizon all day (polar night).
    SunNeverRises,
    /// The sun stays above the horizon all day (midnight sun).
    SunNeverSets,
    /// The next day has no sunrise: the night has no length to bound Fajr
    /// and Isha by.
    NoNextSunrise,
    /// The sun doesn't get high enough for the Asr shadow, or only at noon.
    NoAsr,
    /// The day's times aren't in order five minutes apart (near the polar
    /// circles, where the sun only just rises or sets).
    OutOfOrder,
}

/// A prayer's time on a day, or why there is none.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Outcome {
    Time(Time),
    Unavailable(Unavailable),
}

/// The night of a complete day, which needs the next day's Fajr.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Night {
    Known {
        middle: Timestamp,
        last_third: Timestamp,
        next_fajr: Time,
    },
    /// The next day has no Fajr, and why.
    Unknown(Unavailable),
}

/// A day with all six times, in order five minutes apart (before the
/// offsets; rounding keeps that order: Verus, `lemma_finish_keeps_order`).
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct Schedule {
    date: Date,
    times: [Time; 6],
    night: Night,
}

/// A day without some of its times: each is there, or says why not.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct PartialDay {
    date: Date,
    outcomes: [Outcome; 6],
}

/// A day's prayer times: all of them, or some.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Day {
    Complete(Schedule),
    Partial(PartialDay),
}

impl Day {
    /// The prayer times of the solar day of the UTC date `date` (whose noon
    /// is near 12:00 local mean time on it) at `coordinates`.
    pub fn calculate(date: PrayerDate, coordinates: Coordinates, parameters: &Parameters) -> Day {
        day_of(date.date(), date.civil(), coordinates, parameters)
    }

    /// The prayer times of the local calendar day `date` in `zone`: those
    /// of the solar day whose noon falls on `date` there. That is the UTC
    /// date's where the zone keeps near its meridian; Kiritimati keeps
    /// UTC+14 at 157°W, where the solar day of a UTC date has its noon on
    /// the next local day.
    pub fn on_local_date(
        date: PrayerDate,
        zone: &TimeZone,
        coordinates: Coordinates,
        parameters: &Parameters,
    ) -> Day {
        let civil = date.civil();
        let noon = solar_noon(civil, coordinates, parameters);
        let local = zone.to_datetime(noon).date();
        let solar = match local.cmp(&date.date()) {
            core::cmp::Ordering::Equal => Some(civil),
            core::cmp::Ordering::Greater => civil.previous(),
            core::cmp::Ordering::Less => civil.next(),
        };
        // Within the core's years: a supported date's neighbours are.
        day_of(date.date(), solar.unwrap_or(civil), coordinates, parameters)
    }

    /// The date asked for.
    pub fn date(&self) -> Date {
        match self {
            Day::Complete(schedule) => schedule.date,
            Day::Partial(partial) => partial.date,
        }
    }

    /// The prayer's time, or why there is none.
    pub fn outcome(&self, prayer: Prayer) -> Outcome {
        match self {
            Day::Complete(schedule) => Outcome::Time(schedule.time(prayer)),
            Day::Partial(partial) => partial.outcome(prayer),
        }
    }

    pub fn schedule(&self) -> Option<&Schedule> {
        match self {
            Day::Complete(schedule) => Some(schedule),
            Day::Partial(_) => None,
        }
    }
}

impl Schedule {
    pub fn date(&self) -> Date {
        self.date
    }

    pub fn time(&self, prayer: Prayer) -> Time {
        self.times[prayer.index()]
    }

    /// The prayer's time as an instant.
    pub fn at(&self, prayer: Prayer) -> Timestamp {
        self.time(prayer).at()
    }

    pub fn night(&self) -> Night {
        self.night
    }

    /// The mark's time; None for a night mark of an unknown night.
    pub fn mark(&self, mark: Mark) -> Option<Timestamp> {
        match (mark, self.night) {
            (Mark::Prayer(prayer), _) => Some(self.at(prayer)),
            (Mark::MiddleOfTheNight, Night::Known { middle, .. }) => Some(middle),
            (Mark::LastThirdOfTheNight, Night::Known { last_third, .. }) => Some(last_third),
            (Mark::NextFajr, Night::Known { next_fajr, .. }) => Some(next_fajr.at()),
            (
                Mark::MiddleOfTheNight | Mark::LastThirdOfTheNight | Mark::NextFajr,
                Night::Unknown(_),
            ) => None,
        }
    }

    /// The marks with times, and their seconds.
    fn marks(&self) -> ([Mark; 9], [i64; 9], usize) {
        const MARKS: [Mark; 9] = [
            Mark::Prayer(Prayer::Fajr),
            Mark::Prayer(Prayer::Sunrise),
            Mark::Prayer(Prayer::Dhuhr),
            Mark::Prayer(Prayer::Asr),
            Mark::Prayer(Prayer::Maghrib),
            Mark::Prayer(Prayer::Isha),
            Mark::MiddleOfTheNight,
            Mark::LastThirdOfTheNight,
            Mark::NextFajr,
        ];
        let seconds = MARKS.map(|mark| self.mark(mark).map_or(0, |t| t.as_second()));
        let count = match self.night {
            Night::Known { .. } => 9,
            Night::Unknown(_) => 6,
        };
        (MARKS, seconds, count)
    }

    /// The mark whose time it is at `now`: the latest at or before it
    /// (Verus: `latest_at_or_before`); None before Fajr.
    pub fn current(&self, now: Timestamp) -> Option<Mark> {
        let (marks, seconds, count) = self.marks();
        let index = verified::latest_at_or_before(&seconds[..count], now.as_second())?;
        marks.get(index).copied()
    }

    /// The next mark after `now`: the earliest after it (Verus:
    /// `earliest_after`); None once the day and its night are over.
    pub fn next(&self, now: Timestamp) -> Option<Mark> {
        let (marks, seconds, count) = self.marks();
        let index = verified::earliest_after(&seconds[..count], now.as_second())?;
        marks.get(index).copied()
    }

    /// The time from `now` to the next mark.
    pub fn until_next(&self, now: Timestamp) -> Option<SignedDuration> {
        let next = self.mark(self.next(now)?)?;
        Some(next.duration_since(now))
    }
}

impl PartialDay {
    pub fn date(&self) -> Date {
        self.date
    }

    pub fn outcome(&self, prayer: Prayer) -> Outcome {
        self.outcomes[prayer.index()]
    }
}

// ---------------------------------------------------------------- the steps

/// Where a day's times come from.
#[derive(Debug, Copy, Clone)]
enum Source {
    Sun,
    NearestLatitude(Latitude),
    NearestDay(i16),
}

/// The rules applied to a day's events: all six in order, or not.
enum Resolved {
    /// All six, in order (Verus: `resolve`), not yet rounded.
    Ordered(Unrounded),
    Partial(Unrounded),
}

/// A day's times in seconds from its 00:00 UTC, before offsets and
/// rounding.
struct Unrounded {
    civil: Civil,
    six: verified::Six,
    source: Source,
    polar_night: bool,
}

/// A day's times, offsets added and rounded, in seconds since 1970: only
/// this becomes a public day, and it can't be rounded again.
struct Rounded {
    six: verified::Six,
    source: Source,
    polar_night: bool,
    complete: bool,
}

impl Observed {
    fn resolve(self) -> Resolved {
        let unrounded = |six| Unrounded {
            civil: self.civil,
            six,
            source: Source::Sun,
            polar_night: self.polar_night,
        };
        match verified::resolve(&self.events, &self.bound, self.isha_by) {
            Core::Ordered(six) => Resolved::Ordered(unrounded(six)),
            Core::Partial(six) => Resolved::Partial(unrounded(six)),
        }
    }
}

impl Resolved {
    fn round(self, parameters: &Parameters) -> Rounded {
        let (unrounded, complete) = match self {
            Resolved::Ordered(unrounded) => (unrounded, true),
            Resolved::Partial(unrounded) => (unrounded, false),
        };
        let finished = verified::finish(
            unrounded.six,
            parameters.minutes(),
            parameters.rounding().core(),
        );
        let day = unrounded.civil.number();
        Rounded {
            six: finished.map(|slot| match slot {
                Slot::At(t) => Slot::At(verified::absolute(day, t)),
                Slot::Fallback(t, rule) => Slot::Fallback(verified::absolute(day, t), rule),
                Slot::Missing(m) => Slot::Missing(m),
            }),
            source: unrounded.source,
            polar_night: unrounded.polar_night,
            complete,
        }
    }
}

/// The rules applied to `civil`'s events, and a polar circle resolution to
/// a day without all its times.
fn resolved(civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Resolved {
    match Observed::new(civil, coordinates, parameters).resolve() {
        Resolved::Partial(partial) => match parameters.polar_circle_resolution() {
            PolarCircleResolution::Unresolved => None,
            PolarCircleResolution::AqrabBalad => nearest_latitude(civil, coordinates, parameters),
            PolarCircleResolution::AqrabYaum => nearest_day(civil, coordinates, parameters),
        }
        .map_or(Resolved::Partial(partial), Resolved::Ordered),
        ordered @ Resolved::Ordered(_) => ordered,
    }
}

/// The times of the nearest latitude toward the equator that has them.
fn nearest_latitude(
    civil: Civil,
    coordinates: Coordinates,
    parameters: &Parameters,
) -> Option<Unrounded> {
    (1..=coordinates.latitude.steps_to_equator())
        .find_map(|steps| at_latitude(civil, coordinates, parameters, steps))
}

/// AqrabBalad's try `steps` half degrees toward the equator.
fn at_latitude(
    civil: Civil,
    coordinates: Coordinates,
    parameters: &Parameters,
    steps: u32,
) -> Option<Unrounded> {
    let latitude = coordinates.latitude.toward_equator(steps);
    let place = Coordinates::new(latitude, coordinates.longitude);
    match Observed::new(civil, place, parameters).resolve() {
        Resolved::Ordered(ordered) => Some(Unrounded {
            source: Source::NearestLatitude(latitude),
            ..ordered
        }),
        Resolved::Partial(_) => None,
    }
}

/// The times of the nearest day that has them, a day after then a day
/// before, moved to `civil`.
fn nearest_day(
    civil: Civil,
    coordinates: Coordinates,
    parameters: &Parameters,
) -> Option<Unrounded> {
    let mut later = Some(civil);
    let mut earlier = Some(civil);
    for days in 1..=SEARCH_DAYS {
        later = later.and_then(|d| d.next());
        earlier = earlier.and_then(|d| d.previous());
        let found = later
            .and_then(|other| moved(civil, other, days, coordinates, parameters))
            .or_else(|| {
                earlier.and_then(|other| moved(civil, other, -days, coordinates, parameters))
            });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// AqrabYaum looks this many days either way: half a year.
const SEARCH_DAYS: i16 = 183;

/// AqrabYaum's try: the times of `other`, `days` away, moved to `civil`:
/// the same seconds from its midnight (the core's times count from their
/// date's 00:00 UTC, so moving a day's times to another is keeping them).
fn moved(
    civil: Civil,
    other: Civil,
    days: i16,
    coordinates: Coordinates,
    parameters: &Parameters,
) -> Option<Unrounded> {
    match Observed::new(other, coordinates, parameters).resolve() {
        Resolved::Ordered(ordered) => Some(Unrounded {
            civil,
            source: Source::NearestDay(days),
            ..ordered
        }),
        Resolved::Partial(_) => None,
    }
}

/// `civil`'s times, rounded.
fn rounded(civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Rounded {
    resolved(civil, coordinates, parameters).round(parameters)
}

/// The day `date` asks for, from `civil`'s solar day: its times, and its
/// night from the next day's Fajr.
fn day_of(date: Date, civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Day {
    let today = rounded(civil, coordinates, parameters);
    let outcomes = today.six.map(|slot| outcome(slot, &today, parameters));
    let times = outcomes.map(|outcome| match outcome {
        Outcome::Time(time) => Some(time),
        Outcome::Unavailable(_) => None,
    });
    match (today.complete, times) {
        (
            true,
            [
                Some(fajr),
                Some(sunrise),
                Some(dhuhr),
                Some(asr),
                Some(maghrib),
                Some(isha),
            ],
        ) => {
            let night = night(maghrib, civil, coordinates, parameters);
            Day::Complete(Schedule {
                date,
                times: [fajr, sunrise, dhuhr, asr, maghrib, isha],
                night,
            })
        }
        _ => Day::Partial(PartialDay { date, outcomes }),
    }
}

/// The night of a complete day: Maghrib to the next day's Fajr.
fn night(maghrib: Time, civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Night {
    let Some(next) = civil.next() else {
        return Night::Unknown(Unavailable::NoNextSunrise);
    };
    let tomorrow = rounded(next, coordinates, parameters);
    match outcome(tomorrow.six[0], &tomorrow, parameters) {
        Outcome::Time(next_fajr) => {
            let (middle, last_third) = verified::night_times(
                maghrib.at().as_second(),
                next_fajr.at().as_second(),
                parameters.rounding().core(),
            );
            Night::Known {
                middle: timestamp(middle),
                last_third: timestamp(last_third),
                next_fajr,
            }
        }
        Outcome::Unavailable(why) => Night::Unknown(why),
    }
}

/// A rounded slot as a public outcome.
fn outcome(slot: Slot, day: &Rounded, parameters: &Parameters) -> Outcome {
    let rule = |rule: Rule| match rule {
        Rule::NightShare => match parameters.night_bound() {
            NightBound::HighLatitude(high_latitude) => Fallback::NightShare(high_latitude),
            // The core takes shares only by a high-latitude rule.
            NightBound::MoonsightingSeasons(_) => Fallback::MoonsightingSeason,
        },
        Rule::Season => Fallback::MoonsightingSeason,
        Rule::Seventh => Fallback::MoonsightingSeventh,
        Rule::Sunset => Fallback::Sunset,
    };
    let time = |at: i64, own: Option<Fallback>| match (day.source, own) {
        (Source::NearestLatitude(latitude), _) => Time::Fallback {
            at: timestamp(at),
            rule: Fallback::NearestLatitude(latitude),
        },
        (Source::NearestDay(days), _) => Time::Fallback {
            at: timestamp(at),
            rule: Fallback::NearestDay(days),
        },
        (Source::Sun, Some(rule)) => Time::Fallback {
            at: timestamp(at),
            rule,
        },
        (Source::Sun, None) => Time::Computed(timestamp(at)),
    };
    match slot {
        Slot::At(t) => Outcome::Time(time(t, None)),
        Slot::Fallback(t, r) => Outcome::Time(time(t, Some(rule(r)))),
        Slot::Missing(missing) => Outcome::Unavailable(match missing {
            Missing::Sunrise | Missing::Sunset if day.polar_night => Unavailable::SunNeverRises,
            Missing::Sunrise | Missing::Sunset => Unavailable::SunNeverSets,
            Missing::NextSunrise => Unavailable::NoNextSunrise,
            Missing::Asr => Unavailable::NoAsr,
            Missing::Order => Unavailable::OutOfOrder,
        }),
    }
}

/// The solar noon of `civil` at the place, unrounded: what decides a
/// local date's solar day.
fn solar_noon(civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Timestamp {
    let observed = Observed::new(civil, coordinates, parameters);
    timestamp(verified::absolute(civil.number(), observed.events.transit))
}

/// Seconds since 1970 as a jiff instant. The core's seconds are within
/// ±4·10¹⁰ (Verus: `absolute`), inside jiff's range, where `from_second`
/// never fails (jiff's semver guarantee; tests::the_cores_seconds_are_jiff_instants):
/// its stand-in is never reached.
pub(crate) fn timestamp(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap_or(Timestamp::UNIX_EPOCH)
}

#[cfg(kani)]
mod proofs {
    use super::*;
    use crate::verified::{
        Bound, EVENT_BOUND, Events, IshaBy, MAX_SECONDS, MaghribBy, SEASON_BOUND, Share,
    };

    fn any_offset() -> i64 {
        let t: i64 = kani::any();
        kani::assume((-EVENT_BOUND..=EVENT_BOUND).contains(&t));
        t
    }

    fn any_event() -> Option<i64> {
        kani::any::<bool>().then(any_offset)
    }

    fn any_share() -> Share {
        let (numerator, denominator): (i64, i64) = (kani::any(), kani::any());
        kani::assume((1..=6000).contains(&denominator));
        kani::assume((0..=3000).contains(&numerator) && 2 * numerator <= denominator);
        Share {
            numerator,
            denominator,
        }
    }

    pub(super) fn any_civil() -> Civil {
        let (year, month, day): (i32, u8, u8) = (kani::any(), kani::any(), kani::any());
        let civil = Civil::new(year, month, day);
        kani::assume(civil.is_some());
        civil.unwrap()
    }

    /// The float layer, for the proofs below: any observation meeting the
    /// core's preconditions, which `observe`'s conversions are proven to
    /// (seconds_are_within_the_event_bound, seasons_are_within_the_bound,
    /// angle_new_is_total).
    #[allow(dead_code)] // Used by kani::stub.
    fn any_observed(civil: Civil, _: Coordinates, _: &Parameters) -> Observed {
        let season = || {
            let s: i64 = kani::any();
            kani::assume((0..=SEASON_BOUND).contains(&s));
            s
        };
        Observed {
            civil,
            events: Events {
                sunrise: any_event(),
                transit: any_offset(),
                sunset: any_event(),
                next_sunrise: any_event(),
                fajr: any_event(),
                isha: any_event(),
                maghrib: match kani::any() {
                    true => MaghribBy::Sunset,
                    false => MaghribBy::Angle(any_event()),
                },
                asr: any_event(),
            },
            bound: match kani::any() {
                true => Bound::Share {
                    fajr_share: any_share(),
                    isha_share: any_share(),
                },
                false => Bound::Seasons {
                    fajr_season: season(),
                    isha_season: season(),
                    seventh: kani::any(),
                },
            },
            isha_by: match kani::any() {
                true => IshaBy::Angle,
                false => {
                    let minutes: i64 = kani::any();
                    kani::assume((5..=240).contains(&minutes));
                    IshaBy::Interval(minutes)
                }
            },
            polar_night: kani::any(),
        }
    }

    /// `timestamp`, for the proofs below: total (see its doc), and slow to
    /// model check.
    #[allow(dead_code)] // Used by kani::stub.
    fn any_timestamp(_: i64) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    /// `verified::round`, for the proof below: any time within what Verus
    /// proves rounding moves one by (`lemma_round_bounds`). 64-bit division
    /// is slow to model check, and proven already.
    #[allow(dead_code)] // Used by kani::stub.
    fn any_rounding(seconds: i64, _: verified::Round) -> i64 {
        let moved: i64 = kani::any();
        kani::assume((-29..=59).contains(&moved));
        seconds + moved
    }

    fn any_slot(timed: bool) -> Slot {
        let t: i64 = kani::any();
        kani::assume((-verified::DAY_BOUND..=verified::DAY_BOUND).contains(&t));
        match (timed, kani::any::<u8>()) {
            (_, 0) => Slot::At(t),
            (_, 1) => Slot::Fallback(t, kani::any()),
            (true, _) => Slot::At(t),
            (false, _) => Slot::Missing(kani::any()),
        }
    }

    /// `verified::resolve`, for the proof below: any day its Verus contract
    /// allows (every time within DAY_BOUND; Ordered only with all six).
    #[allow(dead_code)] // Used by kani::stub.
    fn any_resolved(_: &Events, _: &Bound, _: IshaBy) -> Core {
        match kani::any() {
            true => Core::Ordered([0; 6].map(|_| any_slot(true))),
            false => Core::Partial([0; 6].map(|_| any_slot(false))),
        }
    }

    /// `verified::night_times`, for the proof below: any two times (Verus
    /// proves what they are).
    #[allow(dead_code)] // Used by kani::stub.
    fn any_night(_: i64, _: i64, _: verified::Round) -> (i64, i64) {
        let (middle, last_third): (i64, i64) = (kani::any(), kani::any());
        kani::assume((-MAX_SECONDS..=MAX_SECONDS).contains(&middle));
        kani::assume((-MAX_SECONDS..=MAX_SECONDS).contains(&last_third));
        (middle, last_third)
    }

    /// A day's times after the astronomy, for any place and parameters
    /// (every polar circle resolution) and any observation: the rules, the
    /// polar search's result, offsets and rounding, absolute seconds, the
    /// public outcomes. No panic. The float layer, the core's `resolve` and
    /// `round` and the polar searches are stubbed by their proven
    /// contracts; the date is fixed (it enters only through `Civil::number`
    /// and `absolute`, which Verus proves for every date). With
    /// `night_is_total`, all of `Day::calculate`'s glue: `day_of` only
    /// matches on what these return.
    #[kani::proof]
    #[kani::stub(Observed::new, any_observed)]
    #[kani::stub(verified::resolve, any_resolved)]
    #[kani::stub(timestamp, any_timestamp)]
    #[kani::stub(verified::round, any_rounding)]
    #[kani::stub(nearest_latitude, any_search)]
    #[kani::stub(nearest_day, any_search)]
    fn todays_times_are_total() {
        let parameters: Parameters = kani::any();
        let civil = Civil::new(2026, 3, 20).unwrap();
        let today = rounded(civil, kani::any(), &parameters);
        let _ = today.six.map(|slot| outcome(slot, &today, &parameters));
    }

    /// A complete day's night, as `todays_times_are_total` (the next day's
    /// times, its Fajr, the core's `night_times` by its contract): no
    /// panic.
    #[kani::proof]
    #[kani::stub(Observed::new, any_observed)]
    #[kani::stub(verified::resolve, any_resolved)]
    #[kani::stub(timestamp, any_timestamp)]
    #[kani::stub(verified::round, any_rounding)]
    #[kani::stub(verified::night_times, any_night)]
    #[kani::stub(nearest_latitude, any_search)]
    #[kani::stub(nearest_day, any_search)]
    fn night_is_total() {
        let parameters: Parameters = kani::any();
        let civil = Civil::new(2026, 3, 20).unwrap();
        let _ = night(
            Time::Computed(Timestamp::UNIX_EPOCH),
            civil,
            kani::any(),
            &parameters,
        );
    }

    /// The polar circle resolutions' searches, for the proof below: none,
    /// or any day borrowed from elsewhere (their every step is proven by
    /// polar_circle_resolution_steps_are_total; the model checker would
    /// unroll their 180 and 183 steps).
    #[allow(dead_code)] // Used by kani::stub.
    fn any_search(civil: Civil, _: Coordinates, _: &Parameters) -> Option<Unrounded> {
        kani::any::<bool>().then(|| Unrounded {
            civil,
            six: [0; 6].map(|_| any_slot(true)),
            source: match kani::any() {
                true => Source::NearestLatitude(kani::any()),
                false => Source::NearestDay(kani::any()),
            },
            polar_night: kani::any(),
        })
    }

    /// Every step of the polar circle resolutions' searches (any of the
    /// latitudes AqrabBalad tries, any day AqrabYaum tries), for any date,
    /// place and parameters, and any observation (stubbed): no panic. The
    /// searches around the steps are std's iterators over those ranges.
    #[kani::proof]
    #[kani::stub(Observed::new, any_observed)]
    fn polar_circle_resolution_steps_are_total() {
        let (civil, other) = (any_civil(), any_civil());
        let coordinates: Coordinates = kani::any();
        let parameters: Parameters = kani::any();
        let steps: u32 = kani::any();
        kani::assume(1 <= steps && steps <= coordinates.latitude.steps_to_equator());
        let _ = at_latitude(civil, coordinates, &parameters, steps);
        let days: i16 = kani::any();
        kani::assume(days != 0 && (-SEARCH_DAYS..=SEARCH_DAYS).contains(&days));
        let _ = moved(civil, other, days, coordinates, &parameters);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verified::MAX_SECONDS;

    /// The core's seconds are inside jiff's range, where `from_second`
    /// never fails ("a semver guarantee", jiff's docs): `timestamp`'s
    /// stand-in is never reached.
    #[test]
    fn the_cores_seconds_are_jiff_instants() {
        assert!(Timestamp::MIN.as_second() < -MAX_SECONDS);
        assert!(MAX_SECONDS < Timestamp::MAX.as_second());
        assert!(Timestamp::from_second(-MAX_SECONDS).is_ok());
        assert!(Timestamp::from_second(MAX_SECONDS).is_ok());
    }
}
